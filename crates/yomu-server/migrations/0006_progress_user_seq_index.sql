-- events_since() filters `WHERE user_id = ? AND seq > ? ORDER BY seq`, but
-- the only indexes on progress_events are the `seq` primary key and
-- idx_progress_manga(manga_id, ...). With more than one user the query walks
-- the seq PK past other users' rows on every incremental sync. This composite
-- index makes it a direct range scan per user.
CREATE INDEX idx_progress_user_seq ON progress_events(user_id, seq);
