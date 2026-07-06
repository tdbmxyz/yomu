-- Per-user, per-chapter read marks. The progress journal stays the resume
-- pointer ("chapter C, page P"); read marks are the unread accounting,
-- written by explicit bulk mark actions and auto-marked by the server as
-- position events arrive.

CREATE TABLE read_chapters (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    at         TEXT NOT NULL,
    PRIMARY KEY (user_id, chapter_id)
);
CREATE INDEX idx_read_chapters_chapter ON read_chapters(chapter_id);

-- Backfill: unread was previously derived from the current position
-- ("everything up to the position chapter counts as read"); materialize
-- that so nothing flips back to unread on upgrade. `pos` replicates the
-- reading order of Db::list_chapters (number, then reversed source order).
WITH ranked AS (
    SELECT id, manga_id,
           ROW_NUMBER() OVER (
               PARTITION BY manga_id
               ORDER BY number IS NULL, number ASC, source_order DESC
           ) AS pos
    FROM chapters
),
latest AS (
    SELECT user_id, manga_id, chapter_id, at,
           ROW_NUMBER() OVER (
               PARTITION BY user_id, manga_id
               ORDER BY at DESC, id DESC
           ) AS rn
    FROM progress_events
)
INSERT INTO read_chapters (user_id, chapter_id, at)
SELECT l.user_id, r.id, l.at
FROM latest l
JOIN ranked p ON p.id = l.chapter_id
JOIN ranked r ON r.manga_id = p.manga_id AND r.pos <= p.pos
WHERE l.rn = 1;
