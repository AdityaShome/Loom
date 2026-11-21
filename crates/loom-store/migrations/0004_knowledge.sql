-- Knowledge graph: entities and facts about what the agent has seen and done. Auto-extracted rows can be
-- edited or deleted by the user; deletions are kept as tombstones (deleted = 1) so re-extraction does not
-- bring them back.
CREATE TABLE kg_entities (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  note TEXT,
  source TEXT NOT NULL DEFAULT 'auto',   -- auto | model | user
  deleted INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_kg_entity ON kg_entities(session_id, kind, name);

CREATE TABLE kg_facts (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  predicate TEXT NOT NULL,
  object_id TEXT,
  text TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL DEFAULT 'auto',   -- auto | model | user
  key TEXT NOT NULL,                     -- stable identity, so the same fact is never added twice
  node_id TEXT,                          -- the context node it came from
  inject INTEGER NOT NULL DEFAULT 0,     -- send this fact to the model (Enforce mode)
  deleted INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_kg_fact_key ON kg_facts(session_id, key);
CREATE INDEX idx_kg_fact_session ON kg_facts(session_id, deleted);

-- Text the user wrote to replace a node's content in what the model sees (Enforce mode). The original stays in `content`.
ALTER TABLE nodes ADD COLUMN override TEXT;
