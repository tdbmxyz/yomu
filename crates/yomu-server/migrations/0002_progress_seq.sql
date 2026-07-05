-- The sync cursor must reflect *arrival* order, not event time: ids are
-- UUIDv7s stamped by the observing device, so a reconnecting offline client
-- pushes events whose ids sort before cursors other clients have already
-- advanced past — an id cursor would skip them forever. `seq` is assigned
-- by the server at insert; `?since=<seq>` never misses a late push.

CREATE TABLE progress_events_new (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    manga_id   TEXT NOT NULL REFERENCES manga(id) ON DELETE CASCADE,
    chapter_id TEXT NOT NULL,
    page       INTEGER NOT NULL,
    device     TEXT NOT NULL,
    at         TEXT NOT NULL
);

INSERT INTO progress_events_new (id, manga_id, chapter_id, page, device, at)
    SELECT id, manga_id, chapter_id, page, device, at
    FROM progress_events ORDER BY id;

DROP TABLE progress_events;
ALTER TABLE progress_events_new RENAME TO progress_events;

CREATE INDEX idx_progress_manga ON progress_events(manga_id, at DESC, id DESC);
