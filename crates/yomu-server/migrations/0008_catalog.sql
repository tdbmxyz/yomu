-- Source catalog cache: every summary the server has seen, plus the
-- composition of each browse page (stale-while-revalidate reads).
CREATE TABLE catalog_entries (
    source_id    TEXT NOT NULL,
    key          TEXT NOT NULL,
    title        TEXT NOT NULL,
    cover_url    TEXT,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (source_id, key)
);
CREATE INDEX catalog_entries_cover ON catalog_entries (cover_url);
CREATE TABLE catalog_pages (
    source_id  TEXT NOT NULL,
    sort       TEXT NOT NULL,
    page       INTEGER NOT NULL,
    keys       TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (source_id, sort, page)
);
