-- 2.0: manga generalizes to publications, chapters to reading units.
-- Renames + an origin split: scraped rows keep source_id/source_key,
-- streamer-managed files live in file_path (exactly one side set).
-- The old built-in "local" source's rows (source_id='local', source_key =
-- path relative to its dir) convert to the file origin, ids untouched.
-- sqlx wraps this file in one transaction; deferring FK checks lets the
-- publications rebuild drop/recreate the parent table mid-transaction.
PRAGMA defer_foreign_keys = ON;

ALTER TABLE manga RENAME TO publications;
ALTER TABLE chapters RENAME TO reading_units;
ALTER TABLE reading_units RENAME COLUMN manga_id TO publication_id;
ALTER TABLE progress_events RENAME COLUMN manga_id TO publication_id;
ALTER TABLE progress_events RENAME COLUMN chapter_id TO unit_id;
ALTER TABLE read_chapters RENAME TO read_units;
ALTER TABLE read_units RENAME COLUMN chapter_id TO unit_id;
ALTER TABLE manga_genres RENAME TO publication_genres;
ALTER TABLE publication_genres RENAME COLUMN manga_id TO publication_id;
ALTER TABLE updates RENAME COLUMN manga_id TO publication_id;

-- Rebuild publications: nullable source columns, the three new columns,
-- and the exactly-one-origin CHECK (SQLite can't ALTER those in).
CREATE TABLE publications_new (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL DEFAULT 'comics',
    source_id       TEXT,
    source_key      TEXT,
    file_path       TEXT,
    title           TEXT NOT NULL,
    description     TEXT,
    cover_url       TEXT,
    auto_download   INTEGER NOT NULL DEFAULT 0,
    category        TEXT NOT NULL DEFAULT 'reading',
    added_at        TEXT NOT NULL,
    last_checked_at TEXT,
    missing_since   TEXT,
    CHECK (
        (source_id IS NOT NULL AND source_key IS NOT NULL AND file_path IS NULL)
        OR (source_id IS NULL AND source_key IS NULL AND file_path IS NOT NULL)
    ),
    UNIQUE (source_id, source_key),
    UNIQUE (file_path)
);
INSERT INTO publications_new (id, kind, source_id, source_key, file_path, title,
                              description, cover_url, auto_download, category,
                              added_at, last_checked_at)
SELECT id, 'comics',
       CASE WHEN source_id = 'local' THEN NULL ELSE source_id END,
       CASE WHEN source_id = 'local' THEN NULL ELSE source_key END,
       CASE WHEN source_id = 'local' THEN source_key ELSE NULL END,
       title, description, cover_url, auto_download, category,
       added_at, last_checked_at
FROM publications;

-- Deferring FK checks does NOT defer FK *actions*: dropping the old parent
-- fires the children's ON DELETE CASCADE mid-transaction and would wipe
-- reading_units / progress_events / publication_genres (and read_units
-- through reading_units). PRAGMA foreign_keys = OFF can't help — it is a
-- no-op inside the migrator's transaction. So the children are stashed
-- before the drop and restored right after the rebuilt table takes the
-- publications name; ids (and progress_events.seq) are preserved verbatim.
CREATE TABLE stash_units AS SELECT * FROM reading_units;
CREATE TABLE stash_read AS SELECT * FROM read_units;
CREATE TABLE stash_progress AS SELECT * FROM progress_events;
CREATE TABLE stash_genres AS SELECT * FROM publication_genres;

DROP TABLE publications;
ALTER TABLE publications_new RENAME TO publications;

INSERT INTO reading_units SELECT * FROM stash_units;
INSERT INTO read_units SELECT * FROM stash_read;
INSERT INTO progress_events SELECT * FROM stash_progress;
INSERT INTO publication_genres SELECT * FROM stash_genres;
DROP TABLE stash_units;
DROP TABLE stash_read;
DROP TABLE stash_progress;
DROP TABLE stash_genres;

-- Index names carry the old words; recreate under the new ones.
DROP INDEX idx_chapters_manga;
CREATE INDEX idx_units_publication ON reading_units(publication_id);
DROP INDEX idx_chapters_pending;
CREATE INDEX idx_units_pending ON reading_units(download_state)
    WHERE download_state = 'pending';
DROP INDEX idx_read_chapters_chapter;
CREATE INDEX idx_read_units_unit ON read_units(unit_id);
DROP INDEX idx_progress_manga;
CREATE INDEX idx_progress_publication ON progress_events(publication_id, at DESC, id DESC);
DROP INDEX idx_manga_genres_genre;
CREATE INDEX idx_publication_genres_genre ON publication_genres(genre);

-- The catalog cache only serves scraper search/browse; drop stale rows the
-- removed built-in local source left behind.
DELETE FROM catalog_entries WHERE source_id = 'local';
DELETE FROM catalog_pages WHERE source_id = 'local';
