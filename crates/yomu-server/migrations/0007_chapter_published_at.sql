-- Release date as printed by the source listing; best-effort, NULL when
-- the source doesn't expose one.
ALTER TABLE chapters ADD COLUMN published_at TEXT;
