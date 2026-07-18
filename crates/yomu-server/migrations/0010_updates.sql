-- Updater-found new chapters, one row per manga per round; feeds shell
-- notifications (GET /api/v1/updates). Pruned after 30 days.
CREATE TABLE updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    manga_id TEXT NOT NULL,
    chapter_count INTEGER NOT NULL,
    first_title TEXT NOT NULL,
    last_title TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX updates_created_at ON updates (created_at);
