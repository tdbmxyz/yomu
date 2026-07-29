-- 2.2: record the files a books-dir scan had to pass over because it cannot
-- read their format (.cbr/.pdf/.epub and friends). A folder of unreadable
-- volumes used to either vanish from the library or, when tachidesk had left
-- a cover.jpg behind, import as a one-chapter publication whose only page was
-- the cover. Both now import with zero units and this flag instead.
--
-- Plain ALTERs: the columns are additive, nullable-free with defaults, and no
-- constraint changes, so no table rebuild (and none of 0011's CASCADE trap).
-- Existing rows default to "nothing unsupported", which is already correct for
-- every source-origin publication; local ones are corrected by the next scan.
ALTER TABLE publications ADD COLUMN unsupported_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE publications ADD COLUMN unsupported_formats TEXT NOT NULL DEFAULT '';
