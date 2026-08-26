-- A single rollback-only row used by /health/readiness to prove that the main
-- database (not merely SQLite's TEMP database) can acquire and execute a write.
CREATE TABLE readiness_probe (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    checked_at TEXT NOT NULL
);
