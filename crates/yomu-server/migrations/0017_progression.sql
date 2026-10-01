-- Reflowable resource locations are not page numbers. Preserve the append-only
-- journal and its legacy page field; EPUB adds a normalized progression.
ALTER TABLE progress_events ADD COLUMN progression REAL
    CHECK (progression IS NULL OR (progression >= 0 AND progression <= 1));
