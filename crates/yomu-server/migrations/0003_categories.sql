-- Categories: one per manga, a reading status (Reading / Paused / Finished
-- to start). `update_enabled` decides whether the periodic updater checks
-- that category's manga for new chapters — finished/paused series don't
-- need to hammer their sources.

CREATE TABLE categories (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    position       INTEGER NOT NULL,
    update_enabled INTEGER NOT NULL DEFAULT 1
);

INSERT INTO categories (id, name, position, update_enabled) VALUES
    ('reading',  'Reading',  0, 1),
    ('paused',   'Paused',   1, 0),
    ('finished', 'Finished', 2, 0);

-- No REFERENCES clause: SQLite cannot ALTER-ADD a foreign-key column with a
-- non-NULL default; membership is validated in code (db::set_category).
ALTER TABLE manga ADD COLUMN category TEXT NOT NULL DEFAULT 'reading';
