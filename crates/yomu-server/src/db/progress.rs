use sqlx::Row;
use uuid::Uuid;
use yomu_domain::{Position, ProgressEvent};

use super::*;

impl Db {
    /// Append an event for a user. Idempotent on id: replaying a batch is
    /// harmless, which makes offline sync retries safe.
    pub async fn append_event(&self, user_id: Uuid, event: &ProgressEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO progress_events (id, user_id, manga_id, chapter_id, page, device, at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(event.id.to_string())
        .bind(user_id.to_string())
        .bind(event.manga_id.to_string())
        .bind(event.chapter_id.to_string())
        .bind(event.page)
        .bind(&event.device)
        .bind(event.at)
        .execute(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                DbError::Constraint("unknown manga".into())
            }
            _ => DbError::Sqlx(e),
        })?;
        Ok(())
    }

    /// Append a whole offline journal in one transaction. Events for manga
    /// the server no longer knows (deleted meanwhile) are *skipped*, not
    /// errors: they can never apply, and one of them must not wedge the
    /// client's outbox behind an eternally failing batch.
    /// Returns (accepted, skipped).
    pub async fn append_events(
        &self,
        user_id: Uuid,
        events: &[ProgressEvent],
    ) -> Result<(u32, u32)> {
        let mut tx = self.pool.begin().await?;
        let (mut accepted, mut skipped) = (0, 0);
        for event in events {
            let known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM manga WHERE id = ?)")
                    .bind(event.manga_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            // The single-event path validates the chapter via get_chapter; the
            // offline batch must too, or a client desync stores a position
            // pointing at a chapter that resolves to nothing.
            let chapter_known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chapters WHERE id = ?)")
                    .bind(event.chapter_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !known || !chapter_known {
                skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO progress_events (id, user_id, manga_id, chapter_id, page, device, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(event.id.to_string())
            .bind(user_id.to_string())
            .bind(event.manga_id.to_string())
            .bind(event.chapter_id.to_string())
            .bind(event.page)
            .bind(&event.device)
            .bind(event.at)
            .execute(&mut *tx)
            .await?;
            // Replays (id conflict) also count as accepted: the event is in.
            accepted += 1;
        }
        tx.commit().await?;
        Ok((accepted, skipped))
    }

    /// A user's merged current position (max at, id tie-break — same rule
    /// as `yomu_domain::merge_position`).
    pub async fn latest_position(&self, user_id: Uuid, manga_id: Uuid) -> Result<Option<Position>> {
        let row = sqlx::query(
            "SELECT chapter_id, page, at FROM progress_events
             WHERE manga_id = ? AND user_id = ? ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(manga_id.to_string())
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(Position {
                chapter_id: parse_uuid(row.get::<String, _>("chapter_id"))?,
                page: row.get::<i64, _>("page") as u32,
                at: row.get("at"),
            })
        })
        .transpose()
    }

    /// Journal slice for incremental sync. The cursor is the row's
    /// server-assigned arrival sequence — event ids are stamped by the
    /// observing device and would make a cursor skip late offline pushes.
    /// Returns the events plus the cursor for the next page (`None` when
    /// nothing was returned).
    pub async fn events_since(
        &self,
        user_id: Uuid,
        since: Option<i64>,
    ) -> Result<(Vec<ProgressEvent>, Option<i64>)> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT * FROM progress_events WHERE user_id = ? AND seq > ?
             ORDER BY seq LIMIT 1000",
        )
        .bind(user_id.to_string())
        .bind(since.unwrap_or(0))
        .fetch_all(&self.pool)
        .await?;
        let next = rows.last().map(|row| row.seq);
        let events = rows
            .into_iter()
            .map(ProgressEvent::try_from)
            .collect::<Result<_>>()?;
        Ok((events, next))
    }

    /// This user's entire progress journal, for a backup.
    pub async fn export_events(&self, user_id: Uuid) -> Result<Vec<ProgressEvent>> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT * FROM progress_events WHERE user_id = ? ORDER BY seq",
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ProgressEvent::try_from).collect()
    }
}
