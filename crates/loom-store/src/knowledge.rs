//! Knowledge graph storage and the user's edits to it.

use std::collections::HashMap;

use loom_core::{
    knowledge::{AutoFact, EntityRef, InjectFact},
    model::{EntityKind, Node},
};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteRow, Row, Sqlite, Transaction};

use crate::{double_option, node_from_row, now_ms, Store};

#[derive(Debug)]
pub enum KgError {
    NotFound(&'static str),
    Conflict(String),
    Invalid(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for KgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KgError::NotFound(what) => write!(f, "unknown {what}"),
            KgError::Conflict(m) | KgError::Invalid(m) => write!(f, "{m}"),
            KgError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for KgError {}

impl From<anyhow::Error> for KgError {
    fn from(e: anyhow::Error) -> Self {
        KgError::Other(e)
    }
}

impl From<sqlx::Error> for KgError {
    fn from(e: sqlx::Error) -> Self {
        KgError::Other(e.into())
    }
}

type Res<T> = std::result::Result<T, KgError>;

pub const MAX_NAME: usize = 200;
pub const MAX_PREDICATE: usize = 60;
pub const MAX_TEXT: usize = 2000;
pub const MAX_OVERRIDE: usize = 100_000;

#[derive(Debug, Clone, Serialize)]
pub struct Entity {
    pub id: String,
    pub session_id: String,
    pub kind: EntityKind,
    pub name: String,
    pub note: Option<String>,
    /// auto | model | user
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub id: String,
    pub session_id: String,
    pub subject_id: String,
    pub predicate: String,
    pub object_id: Option<String>,
    pub text: String,
    /// auto | model | user
    pub source: String,
    /// The context node it came from.
    pub node_id: Option<String>,
    /// Sent to the model in Enforce mode.
    pub inject: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Knowledge {
    pub entities: Vec<Entity>,
    pub facts: Vec<Fact>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntityPatch {
    pub name: Option<String>,
    pub kind: Option<EntityKind>,
    #[serde(default, deserialize_with = "double_option")]
    pub note: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FactPatch {
    pub subject_id: Option<String>,
    pub predicate: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub object_id: Option<Option<String>>,
    pub text: Option<String>,
    pub inject: Option<bool>,
}

/// A node with its full text and any user override.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDetail {
    #[serde(flatten)]
    pub node: Node,
    #[serde(rename = "override")]
    pub override_text: Option<String>,
}

fn entity_from_row(r: &SqliteRow) -> Res<Entity> {
    Ok(Entity {
        id: r.get("id"),
        session_id: r.get("session_id"),
        kind: EntityKind::parse(r.get("kind")).ok_or_else(|| KgError::Other(anyhow::anyhow!("corrupt entity kind")))?,
        name: r.get("name"),
        note: r.get("note"),
        source: r.get("source"),
    })
}

fn fact_from_row(r: &SqliteRow) -> Fact {
    Fact {
        id: r.get("id"),
        session_id: r.get("session_id"),
        subject_id: r.get("subject_id"),
        predicate: r.get("predicate"),
        object_id: r.get("object_id"),
        text: r.get("text"),
        source: r.get("source"),
        node_id: r.get("node_id"),
        inject: r.get::<i64, _>("inject") != 0,
    }
}

fn clean(s: &str, max: usize, what: &str) -> Res<String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(KgError::Invalid(format!("{what} cannot be empty")));
    }
    if t.chars().count() > max {
        return Err(KgError::Invalid(format!("{what} is longer than {max} characters")));
    }
    Ok(t.to_string())
}

fn clean_text(s: &str) -> Res<String> {
    let t = s.trim();
    if t.chars().count() > MAX_TEXT {
        return Err(KgError::Invalid(format!("text is longer than {MAX_TEXT} characters")));
    }
    Ok(t.to_string())
}

/// Find an entity by identity, creating it if it does not exist. A deleted entity stays deleted (`None`).
async fn entity_id(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    e: &EntityRef,
    source: &str,
    now: i64,
    cache: &mut HashMap<EntityRef, Option<String>>,
) -> anyhow::Result<Option<String>> {
    if let Some(hit) = cache.get(e) {
        return Ok(hit.clone());
    }
    let row = sqlx::query("SELECT id, deleted FROM kg_entities WHERE session_id = ? AND kind = ? AND name = ?")
        .bind(session)
        .bind(e.kind.as_str())
        .bind(&e.name)
        .fetch_optional(&mut **tx)
        .await?;
    let id = match row {
        Some(r) => (r.get::<i64, _>("deleted") == 0).then(|| r.get::<String, _>("id")),
        None => {
            let id = ulid::Ulid::new().to_string();
            sqlx::query(
                "INSERT INTO kg_entities (id, session_id, kind, name, source, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(session)
            .bind(e.kind.as_str())
            .bind(&e.name)
            .bind(source)
            .bind(now)
            .execute(&mut **tx)
            .await?;
            Some(id)
        }
    };
    cache.insert(e.clone(), id.clone());
    Ok(id)
}

/// Add extracted facts (and the entities they need). Facts that already exist, or that the user deleted, are skipped.
/// Returns how many facts were new.
pub(crate) async fn insert_facts(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    facts: &[AutoFact],
    source: &str,
) -> anyhow::Result<usize> {
    let now = now_ms();
    let mut cache = HashMap::new();
    let mut added = 0;
    for f in facts {
        let Some(subject) = entity_id(tx, session, &f.subject, source, now, &mut cache).await? else { continue };
        let object = match &f.object {
            Some(o) => match entity_id(tx, session, o, source, now, &mut cache).await? {
                Some(id) => Some(id),
                None => continue, // the object was deleted by the user
            },
            None => None,
        };
        added += sqlx::query(
            "INSERT OR IGNORE INTO kg_facts (id, session_id, subject_id, predicate, object_id, text, source, key, node_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ulid::Ulid::new().to_string())
        .bind(session)
        .bind(subject)
        .bind(&f.predicate)
        .bind(object)
        .bind(&f.text)
        .bind(source)
        .bind(f.key())
        .bind(&f.node_id)
        .bind(now)
        .execute(&mut **tx)
        .await?
        .rows_affected() as usize;
    }
    Ok(added)
}

impl Store {
    /// Add facts that did not come from ingest (for example from a model). Returns how many were new.
    pub async fn add_facts(&self, session: &str, facts: &[AutoFact], source: &str) -> anyhow::Result<usize> {
        let mut tx = self.pool.begin().await?;
        let n = insert_facts(&mut tx, session, facts, source).await?;
        tx.commit().await?;
        Ok(n)
    }

    pub async fn knowledge(&self, session: &str) -> anyhow::Result<Knowledge> {
        let entities =
            sqlx::query("SELECT * FROM kg_entities WHERE session_id = ? AND deleted = 0 ORDER BY created_at, rowid")
                .bind(session)
                .fetch_all(&self.pool)
                .await?
                .iter()
                .map(|r| entity_from_row(r).map_err(|e| anyhow::anyhow!(e.to_string())))
                .collect::<anyhow::Result<Vec<_>>>()?;
        let facts = sqlx::query(
            "SELECT f.* FROM kg_facts f
             JOIN kg_entities s ON s.id = f.subject_id AND s.deleted = 0
             LEFT JOIN kg_entities o ON o.id = f.object_id
             WHERE f.session_id = ? AND f.deleted = 0 AND (f.object_id IS NULL OR o.deleted = 0)
             ORDER BY f.created_at, f.rowid",
        )
        .bind(session)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(fact_from_row)
        .collect();
        Ok(Knowledge { entities, facts })
    }

    pub async fn get_entity(&self, id: &str) -> Res<Entity> {
        let r = sqlx::query("SELECT * FROM kg_entities WHERE id = ? AND deleted = 0")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        entity_from_row(&r.ok_or(KgError::NotFound("entity"))?)
    }

    pub async fn get_fact(&self, id: &str) -> Res<Fact> {
        let r = sqlx::query("SELECT * FROM kg_facts WHERE id = ? AND deleted = 0")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(fact_from_row(&r.ok_or(KgError::NotFound("fact"))?))
    }

    pub async fn create_entity(&self, session: &str, kind: EntityKind, name: &str, note: Option<&str>) -> Res<Entity> {
        let name = clean(name, MAX_NAME, "name")?;
        let note = note.map(clean_text).transpose()?.filter(|n| !n.is_empty());
        if self.session_settings(session).await?.is_none() {
            return Err(KgError::NotFound("session"));
        }
        let existing =
            sqlx::query("SELECT id, deleted FROM kg_entities WHERE session_id = ? AND kind = ? AND name = ?")
                .bind(session)
                .bind(kind.as_str())
                .bind(&name)
                .fetch_optional(&self.pool)
                .await?;
        let id = match existing {
            Some(r) if r.get::<i64, _>("deleted") == 0 => {
                return Err(KgError::Conflict(format!("a {} named '{name}' already exists", kind.as_str())));
            }
            Some(r) => {
                // Bring a deleted entity back, as the user's own.
                let id: String = r.get("id");
                sqlx::query("UPDATE kg_entities SET deleted = 0, source = 'user', note = ? WHERE id = ?")
                    .bind(&note)
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                id
            }
            None => {
                let id = ulid::Ulid::new().to_string();
                sqlx::query("INSERT INTO kg_entities (id, session_id, kind, name, note, source, created_at) VALUES (?, ?, ?, ?, ?, 'user', ?)")
                    .bind(&id)
                    .bind(session)
                    .bind(kind.as_str())
                    .bind(&name)
                    .bind(&note)
                    .bind(now_ms())
                    .execute(&self.pool)
                    .await?;
                id
            }
        };
        self.touch(Some(session)).await?;
        self.get_entity(&id).await
    }

    pub async fn update_entity(&self, id: &str, p: &EntityPatch) -> Res<Entity> {
        let cur = self.get_entity(id).await?;
        let name = match &p.name {
            Some(n) => clean(n, MAX_NAME, "name")?,
            None => cur.name.clone(),
        };
        let kind = p.kind.unwrap_or(cur.kind);
        let note = match &p.note {
            Some(n) => n.as_deref().map(clean_text).transpose()?.filter(|n| !n.is_empty()),
            None => cur.note.clone(),
        };
        if (name.as_str(), kind) != (cur.name.as_str(), cur.kind) {
            let clash =
                sqlx::query("SELECT id FROM kg_entities WHERE session_id = ? AND kind = ? AND name = ? AND id != ?")
                    .bind(&cur.session_id)
                    .bind(kind.as_str())
                    .bind(&name)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?;
            if clash.is_some() {
                return Err(KgError::Conflict(format!("a {} named '{name}' already exists", kind.as_str())));
            }
        }
        sqlx::query("UPDATE kg_entities SET name = ?, kind = ?, note = ?, source = 'user' WHERE id = ?")
            .bind(&name)
            .bind(kind.as_str())
            .bind(&note)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.touch(Some(&cur.session_id)).await?;
        self.get_entity(id).await
    }

    /// Delete an entity and every fact that touches it. Kept as a tombstone so extraction does not recreate it.
    pub async fn delete_entity(&self, id: &str) -> Res<String> {
        let cur = self.get_entity(id).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE kg_entities SET deleted = 1 WHERE id = ?").bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE kg_facts SET deleted = 1 WHERE subject_id = ?1 OR object_id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.touch(Some(&cur.session_id)).await?;
        Ok(cur.session_id)
    }

    async fn check_entity(&self, session: &str, id: &str) -> Res<()> {
        let e = self.get_entity(id).await?;
        if e.session_id != session {
            return Err(KgError::Invalid("that entity belongs to another session".into()));
        }
        Ok(())
    }

    pub async fn create_fact(
        &self,
        session: &str,
        subject_id: &str,
        predicate: &str,
        object_id: Option<&str>,
        text: &str,
        inject: bool,
    ) -> Res<Fact> {
        let predicate = clean(predicate, MAX_PREDICATE, "predicate")?;
        let text = clean_text(text)?;
        self.check_entity(session, subject_id).await?;
        if let Some(o) = object_id {
            self.check_entity(session, o).await?;
        }
        let id = ulid::Ulid::new().to_string();
        sqlx::query(
            "INSERT INTO kg_facts (id, session_id, subject_id, predicate, object_id, text, source, key, inject, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'user', ?, ?, ?)",
        )
        .bind(&id)
        .bind(session)
        .bind(subject_id)
        .bind(&predicate)
        .bind(object_id)
        .bind(&text)
        .bind(format!("user:{id}"))
        .bind(inject as i64)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        self.touch(Some(session)).await?;
        self.get_fact(&id).await
    }

    pub async fn update_fact(&self, id: &str, p: &FactPatch) -> Res<Fact> {
        let cur = self.get_fact(id).await?;
        let predicate = match &p.predicate {
            Some(x) => clean(x, MAX_PREDICATE, "predicate")?,
            None => cur.predicate.clone(),
        };
        let text = match &p.text {
            Some(x) => clean_text(x)?,
            None => cur.text.clone(),
        };
        let subject = p.subject_id.clone().unwrap_or_else(|| cur.subject_id.clone());
        let object = match &p.object_id {
            Some(o) => o.clone(),
            None => cur.object_id.clone(),
        };
        self.check_entity(&cur.session_id, &subject).await?;
        if let Some(o) = &object {
            self.check_entity(&cur.session_id, o).await?;
        }
        let inject = p.inject.unwrap_or(cur.inject);
        // Changing what the fact says makes it the user's own; only toggling "send to model" does not.
        let edited =
            predicate != cur.predicate || text != cur.text || subject != cur.subject_id || object != cur.object_id;
        sqlx::query("UPDATE kg_facts SET predicate = ?, text = ?, subject_id = ?, object_id = ?, inject = ?, source = CASE WHEN ? THEN 'user' ELSE source END WHERE id = ?")
            .bind(&predicate)
            .bind(&text)
            .bind(&subject)
            .bind(&object)
            .bind(inject as i64)
            .bind(edited)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.touch(Some(&cur.session_id)).await?;
        self.get_fact(id).await
    }

    /// Returns the session id. Facts you made are removed; extracted ones are kept as tombstones so they do not come back.
    pub async fn delete_fact(&self, id: &str) -> Res<String> {
        let cur = self.get_fact(id).await?;
        if cur.source == "user" {
            sqlx::query("DELETE FROM kg_facts WHERE id = ?").bind(id).execute(&self.pool).await?;
        } else {
            sqlx::query("UPDATE kg_facts SET deleted = 1, inject = 0 WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        self.touch(Some(&cur.session_id)).await?;
        Ok(cur.session_id)
    }

    /// The facts the user chose to send to the model, in the order they were created.
    pub async fn injected_facts(&self, session: &str) -> anyhow::Result<Vec<InjectFact>> {
        Ok(sqlx::query(
            "SELECT s.name AS subject, f.predicate, o.name AS object, f.text FROM kg_facts f
             JOIN kg_entities s ON s.id = f.subject_id AND s.deleted = 0
             LEFT JOIN kg_entities o ON o.id = f.object_id
             WHERE f.session_id = ? AND f.deleted = 0 AND f.inject = 1 AND (f.object_id IS NULL OR o.deleted = 0)
             ORDER BY f.created_at, f.rowid",
        )
        .bind(session)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| InjectFact {
            subject: r.get("subject"),
            predicate: r.get("predicate"),
            object: r.get("object"),
            text: r.get("text"),
        })
        .collect())
    }

    // ---- node text overrides

    pub async fn node_detail(&self, id: &str) -> anyhow::Result<Option<NodeDetail>> {
        let Some(r) = sqlx::query("SELECT * FROM nodes WHERE id = ?").bind(id).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        Ok(Some(NodeDetail { node: node_from_row(&r)?, override_text: r.get("override") }))
    }

    /// Replace what the model sees for a node. Returns its session id.
    pub async fn set_override(&self, node_id: &str, text: &str) -> Res<String> {
        if text.trim().is_empty() {
            return Err(KgError::Invalid("the text cannot be empty (use Drop to remove a node)".into()));
        }
        if text.len() > MAX_OVERRIDE {
            return Err(KgError::Invalid(format!("the text is longer than {MAX_OVERRIDE} bytes")));
        }
        let d = self.node_detail(node_id).await?.ok_or(KgError::NotFound("node"))?;
        use loom_core::model::NodeKind::*;
        if matches!(d.node.kind, System | ToolCall | Summary) || d.node.locked {
            return Err(KgError::Invalid(format!(
                "{} nodes cannot be edited (their exact form matters to the provider)",
                d.node.kind.as_str()
            )));
        }
        sqlx::query("UPDATE nodes SET override = ? WHERE id = ?").bind(text).bind(node_id).execute(&self.pool).await?;
        self.touch(Some(&d.node.session_id)).await?;
        Ok(d.node.session_id)
    }

    pub async fn clear_override(&self, node_id: &str) -> Res<String> {
        let d = self.node_detail(node_id).await?.ok_or(KgError::NotFound("node"))?;
        sqlx::query("UPDATE nodes SET override = NULL WHERE id = ?").bind(node_id).execute(&self.pool).await?;
        self.touch(Some(&d.node.session_id)).await?;
        Ok(d.node.session_id)
    }
}
