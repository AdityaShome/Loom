CREATE TABLE sessions (
  id TEXT PRIMARY KEY, name TEXT, provider TEXT, model TEXT,
  budget_tokens INTEGER NOT NULL DEFAULT 0, mode TEXT NOT NULL DEFAULT 'Observe',
  created_at INTEGER NOT NULL
);

CREATE TABLE nodes (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, content_hash BLOB NOT NULL,
  occ INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL, role TEXT NOT NULL,
  content TEXT NOT NULL, tokens INTEGER NOT NULL,
  first_seen_turn INTEGER NOT NULL, last_seen_turn INTEGER NOT NULL,
  position INTEGER NOT NULL, source TEXT,
  importance REAL NOT NULL DEFAULT 0.5, pinned INTEGER NOT NULL DEFAULT 0,
  user_action TEXT, status TEXT NOT NULL DEFAULT 'Full',
  summary_node TEXT, score REAL NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX idx_nodes_identity ON nodes(session_id, content_hash, occ);

CREATE TABLE edges (
  from_id TEXT NOT NULL, to_id TEXT NOT NULL, kind TEXT NOT NULL, weight REAL NOT NULL DEFAULT 1,
  PRIMARY KEY (from_id, to_id, kind)
);

CREATE TABLE turns (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, idx INTEGER NOT NULL,
  request_tokens INTEGER NOT NULL, response_tokens INTEGER NOT NULL DEFAULT 0,
  latency_ms INTEGER NOT NULL DEFAULT 0, cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (session_id, idx)
);

CREATE TABLE snapshots (
  turn_id TEXT PRIMARY KEY, sent_node_ids TEXT NOT NULL, sent_tokens INTEGER NOT NULL,
  original_tokens INTEGER NOT NULL, rewritten INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE rules (
  id TEXT PRIMARY KEY, session_id TEXT, matcher TEXT NOT NULL, action TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE summaries_cache (
  content_hash BLOB PRIMARY KEY, summary TEXT NOT NULL, tokens INTEGER NOT NULL, model TEXT
);
