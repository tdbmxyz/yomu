-- Library domain: mirrors yomu-domain. Conventions as in chaos: UUIDs as
-- hyphenated TEXT, timestamps as RFC3339 TEXT.

CREATE TABLE manga (
    id              TEXT PRIMARY KEY,
    source_id       TEXT NOT NULL,
    source_key      TEXT NOT NULL,
    title           TEXT NOT NULL,
    description     TEXT,
    cover_url       TEXT,
    auto_download   INTEGER NOT NULL DEFAULT 0,
    added_at        TEXT NOT NULL,
    last_checked_at TEXT,
    UNIQUE (source_id, source_key)
);

CREATE TABLE chapters (
    id             TEXT PRIMARY KEY,
    manga_id       TEXT NOT NULL REFERENCES manga(id) ON DELETE CASCADE,
    source_key     TEXT NOT NULL,
    title          TEXT NOT NULL,
    number         REAL,
    source_order   INTEGER NOT NULL,
    scanlator      TEXT,
    fetched_at     TEXT NOT NULL,

    download_state TEXT NOT NULL DEFAULT 'none'
                   CHECK (download_state IN ('none', 'pending', 'downloading', 'downloaded', 'failed')),
    downloaded_at  TEXT,
    download_error TEXT,
    page_count     INTEGER,

    UNIQUE (manga_id, source_key)
);

-- Append-only reading journal (see yomu-domain::progress). Rows are never
-- updated or deleted except through manga cascade; the current position is
-- the merge (max at, then max id) per manga.
CREATE TABLE progress_events (
    id         TEXT PRIMARY KEY,
    manga_id   TEXT NOT NULL REFERENCES manga(id) ON DELETE CASCADE,
    chapter_id TEXT NOT NULL,
    page       INTEGER NOT NULL,
    device     TEXT NOT NULL,
    at         TEXT NOT NULL
);

CREATE INDEX idx_chapters_manga ON chapters(manga_id);
CREATE INDEX idx_chapters_pending ON chapters(download_state) WHERE download_state = 'pending';
CREATE INDEX idx_progress_manga ON progress_events(manga_id, at DESC, id DESC);
