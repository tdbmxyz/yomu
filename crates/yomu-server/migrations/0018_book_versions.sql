-- A publication remains a format-specific version. No IDs, units or progress
-- are merged: EPUB progression and PDF pages cannot be translated reliably.
ALTER TABLE publications ADD COLUMN work_id TEXT;
CREATE INDEX publications_work ON publications(work_id) WHERE work_id IS NOT NULL;
-- 'novels' is the backwards-compatible wire name of the unified Books shelf.
UPDATE publications SET kind = 'novels' WHERE kind = 'pdf';
