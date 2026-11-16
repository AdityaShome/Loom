//! SQLite persistence: sessions, nodes, turns, snapshots.

use std::{collections::HashMap, str::FromStr, time::Duration};

use anyhow::{anyhow, Result};
use loom_core::{
    matching::{canonical_hash, count_tokens, match_nodes, message_overhead, occurrences, Match},
    model::{CanonMsg, Node, NodeKind, Provider, Role, Status},
};
use serde::Serialize;
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
    pub created_at: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
        for (i, ((m, h), (o, mt))) in msgs.iter().zip(&hashes).zip(occs.iter().zip(matches)).enumerate() {
            let t = count_tokens(&m.content) + overhead;
            tokens += t;
            match mt {
                Match::Existing(id) => {
                    sqlx::query(
                        "UPDATE nodes SET last_seen_turn = ?, position = ?,
                           status = CASE WHEN status = 'DroppedByAgent' THEN 'Full' ELSE status END
                         WHERE id = ?",
                    )
                    .bind(turn)
                    .bind(i as i64)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                    ids.push(id);
                }
                Match::New => {
                    let id = ulid::Ulid::new().to_string();
                    sqlx::query(
                        "INSERT INTO nodes (id, session_id, content_hash, occ, kind, role, content, tokens,
                           first_seen_turn, last_seen_turn, position, source)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)",
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
                    .execute(&mut *tx)
                    .await?;
                    new += 1;
                    ids.push(id);
                }
            }
        }

        // Nodes we knew about that the agent stopped sending (it compacted or trimmed).
        let missing = sqlx::query(
            "UPDATE nodes SET status = 'DroppedByAgent'
             WHERE session_id = ? AND last_seen_turn < ? AND status = 'Full'",
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
        Ok(IngestOutcome { turn: turn as u32, total: msgs.len(), new, missing, tokens })
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let rows = sqlx::query(
            "SELECT s.id, s.name, s.provider, s.model, s.mode, s.budget_tokens, s.created_at,
               (SELECT COUNT(*) FROM turns t WHERE t.session_id = s.id) AS turns,
               (SELECT COUNT(*) FROM nodes n WHERE n.session_id = s.id) AS nodes
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
        user_action: r.get("user_action"),
        status: Status::parse(r.get("status")).ok_or_else(|| enum_err("status"))?,
        summary_node: r.get("summary_node"),
        score: r.get::<f64, _>("score") as f32,
    })
}
