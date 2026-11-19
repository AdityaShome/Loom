ALTER TABLE nodes ADD COLUMN locked INTEGER NOT NULL DEFAULT 0;
-- Per-node decision actually applied on this turn (JSON object id -> action); NULL when nothing was rewritten.
ALTER TABLE snapshots ADD COLUMN decisions TEXT;
