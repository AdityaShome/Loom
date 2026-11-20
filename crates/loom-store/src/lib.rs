//! SQLite persistence: sessions, nodes, turns, snapshots.

use std::{collections::HashMap, str::FromStr, time::Duration};

use anyhow::{anyhow, Result};
use loom_core::{
    graph::{build_edges, Edge, GNode},
    matching::{canonical_hash, count_tokens, match_nodes, message_overhead, occurrences, Match},
    model::{Action, CanonMsg, EdgeKind, Node, NodeKind, Provider, Role, Status},
    policy::{budget_for, Matcher, PNode, PolicyInput, PolicyRule, Weights},
};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous},
    Row, SqlitePool,
};
use tokio::sync::Mutex;

pub struct Store {
    pool: SqlitePool,
    /// Serializes ingests so read-match-write sequences never interleave.
    write: Mutex<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub turn: u32,
    /// Messages in the request.
    pub total: usize,
    /// Nodes created by this request.
    pub new: usize,
    /// Nodes previously seen that the agent no longer sends.
    pub missing: usize,
    /// Relations (edges) created by this request.
    pub edges: usize,
    pub tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub mode: String,
    pub budget_tokens: i64,
    pub turns: i64,
    pub nodes: i64,
    /// Tokens not sent because Enforce rewrote requests.
    pub saved_tokens: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnRow {
    pub idx: i64,
    /// Tokens of the request as the agent sent it.
    pub request_tokens: i64,
    /// Tokens of what the model actually received (smaller when Enforce rewrote it).
    pub sent_tokens: i64,
    pub rewritten: bool,
    pub created_at: i64,
    pub nodes: i64,
}

/// Node ids that entered / left the request between two turns.
#[derive(Debug, Clone, Serialize)]
pub struct TurnDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Partial update of a node's user controls. `user_action: null` clears the override.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodePatch {
    pub importance: Option<f32>,
    pub pinned: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub user_action: Option<Option<Action>>,
}

fn double_option<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Policy input plus the cached summary text for nodes that already have one.
pub struct PolicyData {
    pub input: PolicyInput,
    /// node id -> summary text
    pub summaries: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionUpdate {
    pub budget_tokens: Option<i64>,
    pub mode: Option<String>,
    pub weights: Option<Weights>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSettings {
    pub id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// 0 = use the model default.
    pub budget_tokens: i64,
    pub mode: String,
    pub weights: Weights,
    pub edited_at: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Store {
    /// `path` is a file path, or `:memory:`.
    pub async fn open(path: &str) -> Result<Self> {
        let pool = if path == ":memory:" {
            // Each in-memory connection is its own database, so keep exactly one alive.
            SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect_with(SqliteConnectOptions::from_str("sqlite::memory:")?)
                .await?
        } else {
            let opts = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .busy_timeout(Duration::from_secs(5));
            SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?
        };
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool, write: Mutex::new(()) })
    }

    /// Record one request: match messages to nodes, insert new ones, flag nodes
    /// the agent dropped, and save a snapshot of what was sent.
    pub async fn ingest(
        &self,
        session_id: &str,
        provider: Provider,
        model: Option<&str>,
        msgs: &[CanonMsg],
    ) -> Result<IngestOutcome> {
        let _guard = self.write.lock().await;
        let mut tx = self.pool.begin().await?;
        let now = now_ms();

        sqlx::query(
            "INSERT INTO sessions (id, name, provider, model, created_at) VALUES (?1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET model = COALESCE(?3, model)",
        )
        .bind(session_id)
        .bind(provider.as_str())
        .bind(model)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let turn: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(idx), -1) + 1 FROM turns WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&mut *tx)
            .await?;

        let mut existing: HashMap<([u8; 32], u32), String> = HashMap::new();
        for r in sqlx::query("SELECT id, content_hash, occ FROM nodes WHERE session_id = ?")
            .bind(session_id)
            .fetch_all(&mut *tx)
            .await?
        {
            let h: Vec<u8> = r.get("content_hash");
            let h: [u8; 32] = h.try_into().map_err(|_| anyhow!("corrupt content_hash"))?;
            existing.insert((h, r.get::<i64, _>("occ") as u32), r.get("id"));
        }

        let hashes: Vec<[u8; 32]> = msgs.iter().map(canonical_hash).collect();
        let occs = occurrences(&hashes);
        let matches = match_nodes(&hashes, &occs, &existing);

        let overhead = message_overhead(provider);
        let mut ids = Vec::with_capacity(msgs.len());
        let (mut new, mut tokens) = (0usize, 0u32);
        let mut new_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Request position -> id of the node already stored there (null for new nodes), updated in one statement.
        let mut known_by_position: Vec<Option<String>> = Vec::with_capacity(msgs.len());
        for (i, ((m, h), (o, mt))) in msgs.iter().zip(&hashes).zip(occs.iter().zip(matches)).enumerate() {
            let t = count_tokens(&m.content) + overhead;
            tokens += t;
            match mt {
                Match::Existing(id) => {
                    known_by_position.push(Some(id.clone()));
                    ids.push(id);
                }
                Match::New => {
                    known_by_position.push(None);
                    let id = ulid::Ulid::new().to_string();
                    sqlx::query(
                        "INSERT INTO nodes (id, session_id, content_hash, occ, kind, role, content, tokens,
                           first_seen_turn, last_seen_turn, position, source, locked)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12)",
                    )
                    .bind(&id)
                    .bind(session_id)
                    .bind(h.as_slice())
                    .bind(*o as i64)
                    .bind(m.kind.as_str())
                    .bind(m.role.as_str())
                    .bind(&m.content)
                    .bind(t as i64)
                    .bind(turn)
                    .bind(i as i64)
                    .bind(&m.source)
                    .bind(m.opaque as i64)
                    .execute(&mut *tx)
                    .await?;
                    new += 1;
                    new_ids.insert(id.clone());
                    ids.push(id);
                }
            }
        }

        // Nodes seen again: refresh turn, position and status in ONE statement (cost does not grow with the
        // number of statements per node). Status is recomputed every request: Full unless Enforce rewrites it.
        sqlx::query(
            "UPDATE nodes SET last_seen_turn = ?1, status = 'Full', position = m.key
             FROM json_each(?2) AS m WHERE nodes.id = m.value",
        )
        .bind(turn)
        .bind(serde_json::to_string(&known_by_position)?)
        .execute(&mut *tx)
        .await?;

        // Relations among the nodes of this request; only edges touching new nodes are written.
        let gnodes: Vec<GNode> = msgs
            .iter()
            .zip(&ids)
            .map(|(m, id)| GNode {
                id,
                kind: m.kind,
                source: m.source.as_deref(),
                tool_id: m.tool_id.as_deref(),
                text: Some(&m.content),
            })
            .collect();
        let new_refs: std::collections::HashSet<&str> = new_ids.iter().map(String::as_str).collect();
        let mut edges = 0usize;
        for e in build_edges(&gnodes, &new_refs) {
            edges += sqlx::query("INSERT OR IGNORE INTO edges (from_id, to_id, kind, weight) VALUES (?, ?, ?, ?)")
                .bind(&e.from)
                .bind(&e.to)
                .bind(e.kind.as_str())
                .bind(e.weight as f64)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
        }

        // Nodes we knew about that the agent stopped sending (it compacted or trimmed).
        let missing = sqlx::query(
            "UPDATE nodes SET status = 'DroppedByAgent'
             WHERE session_id = ? AND last_seen_turn < ? AND status != 'DroppedByAgent'",
        )
        .bind(session_id)
        .bind(turn)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;

        let turn_id = ulid::Ulid::new().to_string();
        sqlx::query("INSERT INTO turns (id, session_id, idx, request_tokens, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&turn_id)
            .bind(session_id)
            .bind(turn)
            .bind(tokens as i64)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO snapshots (turn_id, sent_node_ids, sent_tokens, original_tokens, rewritten)
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(&turn_id)
        .bind(serde_json::to_string(&ids)?)
        .bind(tokens as i64)
        .bind(tokens as i64)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(IngestOutcome { turn: turn as u32, total: msgs.len(), new, missing, edges, tokens })
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let rows = sqlx::query(
            "SELECT s.id, s.name, s.provider, s.model, s.mode, s.budget_tokens, s.created_at,
               (SELECT COUNT(*) FROM turns t WHERE t.session_id = s.id) AS turns,
               (SELECT COUNT(*) FROM nodes n WHERE n.session_id = s.id) AS nodes,
               (SELECT COALESCE(SUM(sn.original_tokens - sn.sent_tokens), 0) FROM snapshots sn
                  JOIN turns t ON t.id = sn.turn_id WHERE t.session_id = s.id) AS saved_tokens
             FROM sessions s ORDER BY s.created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| SessionRow {
                id: r.get("id"),
                name: r.get("name"),
                provider: r.get("provider"),
                model: r.get("model"),
                mode: r.get("mode"),
                budget_tokens: r.get("budget_tokens"),
                turns: r.get("turns"),
                nodes: r.get("nodes"),
                saved_tokens: r.get("saved_tokens"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    /// All nodes of a session in first-seen order.
    pub async fn list_nodes(&self, session_id: &str) -> Result<Vec<Node>> {
        sqlx::query("SELECT * FROM nodes WHERE session_id = ? ORDER BY rowid")
            .bind(session_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(node_from_row)
            .collect()
    }

    /// Node ids sent on a given turn, in order.
    pub async fn snapshot_node_ids(&self, session_id: &str, turn: u32) -> Result<Option<Vec<String>>> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT sn.sent_node_ids FROM snapshots sn JOIN turns t ON t.id = sn.turn_id
             WHERE t.session_id = ? AND t.idx = ?",
        )
        .bind(session_id)
        .bind(turn as i64)
        .fetch_optional(&self.pool)
        .await?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
    }

    /// All edges whose source node belongs to the session.
    pub async fn list_edges(&self, session_id: &str) -> Result<Vec<Edge>> {
        sqlx::query(
            "SELECT e.from_id, e.to_id, e.kind, e.weight FROM edges e
             JOIN nodes n ON n.id = e.from_id WHERE n.session_id = ? ORDER BY e.rowid",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(Edge {
                from: r.get("from_id"),
                to: r.get("to_id"),
                kind: EdgeKind::parse(r.get("kind")).ok_or_else(|| anyhow!("corrupt edge kind"))?,
                weight: r.get::<f64, _>("weight") as f32,
            })
        })
        .collect()
    }

    pub async fn list_turns(&self, session_id: &str) -> Result<Vec<TurnRow>> {
        let rows = sqlx::query(
            "SELECT t.idx, t.request_tokens, t.created_at, sn.sent_node_ids, sn.sent_tokens, sn.rewritten
             FROM turns t JOIN snapshots sn ON sn.turn_id = t.id
             WHERE t.session_id = ? ORDER BY t.idx",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let ids: Vec<String> = serde_json::from_str(r.get::<&str, _>("sent_node_ids"))?;
                Ok(TurnRow {
                    idx: r.get("idx"),
                    request_tokens: r.get("request_tokens"),
                    sent_tokens: r.get("sent_tokens"),
                    rewritten: r.get::<i64, _>("rewritten") != 0,
                    created_at: r.get("created_at"),
                    nodes: ids.len() as i64,
                })
            })
            .collect()
    }

    /// What changed between turn `a` and turn `b`; `None` if either turn is unknown.
    pub async fn diff_turns(&self, session_id: &str, a: u32, b: u32) -> Result<Option<TurnDiff>> {
        let (Some(ia), Some(ib)) =
            (self.snapshot_node_ids(session_id, a).await?, self.snapshot_node_ids(session_id, b).await?)
        else {
            return Ok(None);
        };
        let (sa, sb): (std::collections::HashSet<_>, std::collections::HashSet<_>) =
            (ia.iter().collect(), ib.iter().collect());
        Ok(Some(TurnDiff {
            added: ib.iter().filter(|i| !sa.contains(i)).cloned().collect(),
            removed: ia.iter().filter(|i| !sb.contains(i)).cloned().collect(),
        }))
    }

    pub async fn session_settings(&self, id: &str) -> Result<Option<SessionSettings>> {
        let Some(r) = sqlx::query(
            "SELECT id, provider, model, budget_tokens, mode, weights, edited_at FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let weights: Option<String> = r.get("weights");
        Ok(Some(SessionSettings {
            id: r.get("id"),
            provider: r.get("provider"),
            model: r.get("model"),
            budget_tokens: r.get("budget_tokens"),
            mode: r.get("mode"),
            weights: weights.and_then(|w| serde_json::from_str(&w).ok()).unwrap_or_default(),
            edited_at: r.get("edited_at"),
        }))
    }

    /// Returns false if the session does not exist.
    pub async fn update_session(&self, id: &str, u: &SessionUpdate) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        if let Some(b) = u.budget_tokens {
            sqlx::query("UPDATE sessions SET budget_tokens = ? WHERE id = ?")
                .bind(b)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(m) = &u.mode {
            sqlx::query("UPDATE sessions SET mode = ? WHERE id = ?").bind(m).bind(id).execute(&mut *tx).await?;
        }
        if let Some(w) = &u.weights {
            sqlx::query("UPDATE sessions SET weights = ? WHERE id = ?")
                .bind(serde_json::to_string(w)?)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        let n = sqlx::query("UPDATE sessions SET edited_at = ? WHERE id = ?")
            .bind(now_ms())
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(n > 0)
    }

    /// Apply a patch; returns the node's session id, or `None` if the node does not exist.
    pub async fn patch_node(&self, id: &str, p: &NodePatch) -> Result<Option<String>> {
        let mut tx = self.pool.begin().await?;
        let Some(session): Option<String> =
            sqlx::query_scalar("SELECT session_id FROM nodes WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?
        else {
            return Ok(None);
        };
        if let Some(i) = p.importance {
            sqlx::query("UPDATE nodes SET importance = ? WHERE id = ?")
                .bind(i.clamp(0.0, 1.0) as f64)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(pin) = p.pinned {
            sqlx::query("UPDATE nodes SET pinned = ? WHERE id = ?").bind(pin as i64).bind(id).execute(&mut *tx).await?;
        }
        if let Some(a) = &p.user_action {
            sqlx::query("UPDATE nodes SET user_action = ? WHERE id = ?")
                .bind(a.map(|a| a.as_str()))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE sessions SET edited_at = ? WHERE id = ?")
            .bind(now_ms())
            .bind(&session)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(session))
    }

    /// Bring a dropped or summarized node back: force it to be kept.
    pub async fn restore_node(&self, id: &str) -> Result<Option<String>> {
        self.patch_node(id, &NodePatch { user_action: Some(Some(Action::Keep)), ..Default::default() }).await
    }

    /// Rules for a session: global rules plus that session's own. `None` lists all.
    pub async fn list_rules(&self, session: Option<&str>) -> Result<Vec<PolicyRule>> {
        let rows = match session {
            Some(s) => {
                sqlx::query(
                    "SELECT * FROM rules WHERE session_id IS NULL OR session_id = ? ORDER BY priority DESC, rowid",
                )
                .bind(s)
                .fetch_all(&self.pool)
                .await?
            }
            None => sqlx::query("SELECT * FROM rules ORDER BY priority DESC, rowid").fetch_all(&self.pool).await?,
        };
        rows.iter()
            .map(|r| {
                Ok(PolicyRule {
                    id: r.get("id"),
                    session_id: r.get("session_id"),
                    matcher: serde_json::from_str::<Matcher>(r.get::<&str, _>("matcher"))?,
                    action: Action::parse(r.get("action")).ok_or_else(|| anyhow!("corrupt rule action"))?,
                    priority: r.get::<i64, _>("priority") as i32,
                })
            })
            .collect()
    }

    pub async fn upsert_rule(&self, r: &PolicyRule) -> Result<()> {
        sqlx::query(
            "INSERT INTO rules (id, session_id, matcher, action, priority) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET session_id = excluded.session_id, matcher = excluded.matcher,
               action = excluded.action, priority = excluded.priority",
        )
        .bind(&r.id)
        .bind(&r.session_id)
        .bind(serde_json::to_string(&r.matcher)?)
        .bind(r.action.as_str())
        .bind(r.priority as i64)
        .execute(&self.pool)
        .await?;
        self.touch(r.session_id.as_deref()).await
    }

    pub async fn delete_rule(&self, id: &str) -> Result<bool> {
        let scope: Option<Option<String>> =
            sqlx::query_scalar("SELECT session_id FROM rules WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        let Some(scope) = scope else { return Ok(false) };
        sqlx::query("DELETE FROM rules WHERE id = ?").bind(id).execute(&self.pool).await?;
        self.touch(scope.as_deref()).await?;
        Ok(true)
    }

    /// Mark edits as pending: one session, or every session for a global rule.
    async fn touch(&self, session: Option<&str>) -> Result<()> {
        match session {
            Some(s) => {
                sqlx::query("UPDATE sessions SET edited_at = ? WHERE id = ?")
                    .bind(now_ms())
                    .bind(s)
                    .execute(&self.pool)
                    .await?
            }
            None => sqlx::query("UPDATE sessions SET edited_at = ?").bind(now_ms()).execute(&self.pool).await?,
        };
        Ok(())
    }

    /// Edits made since the last model call that the next request will reflect.
    pub async fn pending_changes(&self, session_id: &str) -> Result<bool> {
        let r: Option<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT s.edited_at, (SELECT MAX(created_at) FROM turns t WHERE t.session_id = s.id) FROM sessions s WHERE s.id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(matches!(r, Some((edited, last)) if edited > last.unwrap_or(0)))
    }

    /// Everything the policy needs for the next request of a session: the nodes
    /// of the latest request (in order), their edges, settings, rules, cached
    /// summaries, and what was sent for each node last call.
    pub async fn policy_data(&self, session_id: &str) -> Result<Option<PolicyData>> {
        self.policy_data_inner(session_id, None).await
    }

    /// Same, for the request that was just ingested: node text comes from `msgs` (already in memory)
    /// instead of being read back from the database.
    pub async fn policy_data_for_request(&self, session_id: &str, msgs: &[CanonMsg]) -> Result<Option<PolicyData>> {
        self.policy_data_inner(session_id, Some(msgs)).await
    }

    async fn policy_data_inner(&self, session_id: &str, msgs: Option<&[CanonMsg]>) -> Result<Option<PolicyData>> {
        let Some(settings) = self.session_settings(session_id).await? else { return Ok(None) };
        let latest: Option<i64> = sqlx::query_scalar("SELECT MAX(idx) FROM turns WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?;
        let Some(latest) = latest else { return Ok(None) };

        // Skip the (large) content column when the caller already has the text.
        let columns = if msgs.is_some() {
            "id, session_id, content_hash, occ, kind, role, '' AS content, tokens, first_seen_turn, last_seen_turn,              position, source, importance, pinned, locked, user_action, status, summary_node, score"
        } else {
            "*"
        };
        let mut nodes: Vec<Node> = sqlx::query(&format!(
            "SELECT {columns} FROM nodes WHERE session_id = ? AND last_seen_turn = ? AND status != 'DroppedByAgent' ORDER BY position"
        ))
        .bind(session_id)
        .bind(latest)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(node_from_row)
        .collect::<Result<_>>()?;
        if let Some(msgs) = msgs {
            for n in &mut nodes {
                n.content = msgs
                    .get(n.position as usize)
                    .map(|m| m.content.clone())
                    .ok_or_else(|| anyhow!("node position outside the request"))?;
            }
        }

        let present: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges: Vec<Edge> = self
            .list_edges(session_id)
            .await?
            .into_iter()
            .filter(|e| present.contains(e.from.as_str()) && present.contains(e.to.as_str()))
            .collect();

        // Cached summaries for these nodes (keyed by content hash).
        let mut summaries: HashMap<String, (String, u32)> = HashMap::new();
        for r in sqlx::query(
            "SELECT n.id, s.summary, s.tokens FROM nodes n JOIN summaries_cache s ON s.content_hash = n.content_hash
             WHERE n.session_id = ? AND n.last_seen_turn = ?",
        )
        .bind(session_id)
        .bind(latest)
        .fetch_all(&self.pool)
        .await?
        {
            summaries.insert(r.get("id"), (r.get("summary"), r.get::<i64, _>("tokens") as u32));
        }

        // What the previous call actually sent for each node.
        let prev: HashMap<String, Action> = if latest == 0 {
            HashMap::new()
        } else {
            let row = sqlx::query(
                "SELECT sn.decisions, sn.sent_node_ids FROM snapshots sn JOIN turns t ON t.id = sn.turn_id
                 WHERE t.session_id = ? AND t.idx = ?",
            )
            .bind(session_id)
            .bind(latest - 1)
            .fetch_optional(&self.pool)
            .await?;
            match row {
                Some(r) => match r.get::<Option<String>, _>("decisions") {
                    Some(d) => serde_json::from_str::<HashMap<String, Action>>(&d).unwrap_or_default(),
                    // Nothing was rewritten: every node of that request went out unchanged.
                    None => serde_json::from_str::<Vec<String>>(r.get::<&str, _>("sent_node_ids"))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|id| (id, Action::Keep))
                        .collect(),
                },
                None => HashMap::new(),
            }
        };

        let budget = if settings.budget_tokens > 0 {
            settings.budget_tokens.min(u32::MAX as i64) as u32
        } else {
            budget_for(settings.model.as_deref())
        };
        let pnodes = nodes
            .into_iter()
            .map(|n| PNode {
                prev: prev.get(&n.id).copied(),
                summary_tokens: summaries.get(&n.id).map(|s| s.1),
                id: n.id,
                kind: n.kind,
                content: n.content,
                content_hash: n.content_hash,
                tokens: n.tokens,
                first_seen_turn: n.first_seen_turn,
                source: n.source,
                importance: n.importance,
                pinned: n.pinned,
                locked: n.locked,
                user_action: n.user_action.as_deref().and_then(Action::parse),
            })
            .collect();
        Ok(Some(PolicyData {
            input: PolicyInput {
                nodes: pnodes,
                edges,
                latest_turn: latest as u32,
                budget,
                weights: settings.weights,
                rules: self.list_rules(Some(session_id)).await?,
                provider: settings.provider.as_deref().and_then(Provider::parse).unwrap_or(Provider::Anthropic),
                no_summary: Default::default(),
            },
            summaries: summaries.into_iter().map(|(k, v)| (k, v.0)).collect(),
        }))
    }

    pub async fn policy_input(&self, session_id: &str) -> Result<Option<PolicyInput>> {
        Ok(self.policy_data(session_id).await?.map(|d| d.input))
    }

    pub async fn session_mode(&self, id: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT mode FROM sessions WHERE id = ?").bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn get_summary(&self, hash: &[u8; 32]) -> Result<Option<(String, u32)>> {
        let r = sqlx::query("SELECT summary, tokens FROM summaries_cache WHERE content_hash = ?")
            .bind(hash.as_slice())
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| (r.get("summary"), r.get::<i64, _>("tokens") as u32)))
    }

    pub async fn put_summary(&self, hash: &[u8; 32], summary: &str, tokens: u32, model: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO summaries_cache (content_hash, summary, tokens, model) VALUES (?, ?, ?, ?)",
        )
        .bind(hash.as_slice())
        .bind(summary)
        .bind(tokens as i64)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record what Enforce actually sent on `turn`: replaces the snapshot's
    /// "everything" with the kept and summarized nodes, stores the per-node
    /// decisions, and updates node statuses.
    pub async fn record_enforced(
        &self,
        session_id: &str,
        turn: u32,
        decisions: &[(String, Action)],
        sent_tokens: u32,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let sent_ids: Vec<&str> = decisions.iter().filter(|d| d.1 != Action::Drop).map(|d| d.0.as_str()).collect();
        let map: HashMap<&str, Action> = decisions.iter().map(|(id, a)| (id.as_str(), *a)).collect();
        let rewritten = decisions.iter().any(|d| d.1 != Action::Keep);
        sqlx::query(
            "UPDATE snapshots SET sent_node_ids = ?, sent_tokens = ?, rewritten = ?, decisions = ?
             WHERE turn_id = (SELECT id FROM turns WHERE session_id = ? AND idx = ?)",
        )
        .bind(serde_json::to_string(&sent_ids)?)
        .bind(sent_tokens as i64)
        .bind(rewritten as i64)
        .bind(if rewritten { Some(serde_json::to_string(&map)?) } else { None })
        .bind(session_id)
        .bind(turn as i64)
        .execute(&mut *tx)
        .await?;
        // Ingest already reset every node to Full; only the rewritten ones need a new status.
        let changed: HashMap<&str, &str> = decisions
            .iter()
            .filter_map(|(id, a)| match a {
                Action::Keep => None,
                Action::Summarize => Some((id.as_str(), Status::Summarized.as_str())),
                Action::Drop => Some((id.as_str(), Status::Dropped.as_str())),
            })
            .collect();
        if !changed.is_empty() {
            sqlx::query("UPDATE nodes SET status = m.value FROM json_each(?1) AS m WHERE nodes.id = m.key")
                .bind(serde_json::to_string(&changed)?)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn turn_count(&self, session_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM turns WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?)
    }
}

fn node_from_row(r: &SqliteRow) -> Result<Node> {
    let h: Vec<u8> = r.get("content_hash");
    let enum_err = |what: &str| anyhow!("corrupt {what} in nodes row");
    Ok(Node {
        id: r.get("id"),
        session_id: r.get("session_id"),
        content_hash: h.try_into().map_err(|_| enum_err("content_hash"))?,
        occ: r.get::<i64, _>("occ") as u32,
        kind: NodeKind::parse(r.get("kind")).ok_or_else(|| enum_err("kind"))?,
        role: Role::parse(r.get("role")).ok_or_else(|| enum_err("role"))?,
        content: r.get("content"),
        tokens: r.get::<i64, _>("tokens") as u32,
        first_seen_turn: r.get::<i64, _>("first_seen_turn") as u32,
        last_seen_turn: r.get::<i64, _>("last_seen_turn") as u32,
        position: r.get::<i64, _>("position") as u32,
        source: r.get("source"),
        importance: r.get::<f64, _>("importance") as f32,
        pinned: r.get::<i64, _>("pinned") != 0,
        locked: r.get::<i64, _>("locked") != 0,
        user_action: r.get("user_action"),
        status: Status::parse(r.get("status")).ok_or_else(|| enum_err("status"))?,
        summary_node: r.get("summary_node"),
        score: r.get::<f64, _>("score") as f32,
    })
}
