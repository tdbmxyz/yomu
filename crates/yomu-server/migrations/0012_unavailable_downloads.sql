-- A chapter the source does not offer (premium, locked) is not a failed
-- download: retrying it accomplishes nothing until the source frees it, so
-- it needs a state of its own that the retry and auto-download paths can
-- pass over. SQLite cannot widen a CHECK in place, so the table is rebuilt.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE reading_units_new (
    id             TEXT PRIMARY KEY,
    publication_id TEXT NOT NULL REFERENCES publications(id) ON DELETE CASCADE,
    source_key     TEXT NOT NULL,
    title          TEXT NOT NULL,
    number         REAL,
    source_order   INTEGER NOT NULL,
    scanlator      TEXT,
    fetched_at     TEXT NOT NULL,
    published_at   TEXT,

    download_state TEXT NOT NULL DEFAULT 'none'
                   CHECK (download_state IN ('none', 'pending', 'downloading',
                                             'downloaded', 'failed', 'unavailable')),
    downloaded_at  TEXT,
    download_error TEXT,
    page_count     INTEGER,

    UNIQUE (publication_id, source_key)
);
INSERT INTO reading_units_new (id, publication_id, source_key, title, number,
                               source_order, scanlator, fetched_at, published_at,
                               download_state, downloaded_at, download_error,
                               page_count)
SELECT id, publication_id, source_key, title, number,
       source_order, scanlator, fetched_at, published_at,
       download_state, downloaded_at, download_error,
       page_count
FROM reading_units;

-- Dropping the old table fires read_units' ON DELETE CASCADE mid-transaction
-- (deferring FK *checks* does not defer FK *actions*, see 0011), so the read
-- marks are stashed across the swap and put back verbatim.
CREATE TABLE stash_read AS SELECT * FROM read_units;
DROP TABLE reading_units;
ALTER TABLE reading_units_new RENAME TO reading_units;
INSERT INTO read_units SELECT * FROM stash_read;
DROP TABLE stash_read;

CREATE INDEX idx_units_publication ON reading_units(publication_id);
CREATE INDEX idx_units_pending ON reading_units(download_state)
    WHERE download_state = 'pending';
