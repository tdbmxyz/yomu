use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
use yomu_domain::{Chapter, UpdateEvent};

use super::*;

impl Db {
    /// Record one updater find: `chapters` are the round's new chapters
    /// for this manga, in listing order (see `sync::refresh_manga`).
    pub async fn add_update(&self, manga_id: Uuid, chapters: &[Chapter]) -> Result<()> {
        let (Some(first), Some(last)) = (chapters.first(), chapters.last()) else {
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO updates (manga_id, chapter_count, first_title, last_title, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(manga_id.to_string())
        .bind(chapters.len() as u32)
        .bind(&first.title)
        .bind(&last.title)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Events strictly newer than `since`, newest first, joined with the
    /// manga title (events for since-removed manga are dropped).
    pub async fn updates_since(
        &self,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<UpdateEvent>> {
        let rows = sqlx::query(
            "SELECT u.manga_id, u.chapter_count, u.first_title, u.last_title, u.created_at,
                    m.title AS manga_title
             FROM updates u JOIN manga m ON m.id = u.manga_id
             WHERE u.created_at > ?
             ORDER BY u.created_at DESC, u.id DESC
             LIMIT ?",
        )
        .bind(since.to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let manga_id: String = row.get("manga_id");
                let created_at: String = row.get("created_at");
                Ok(UpdateEvent {
                    manga_id: manga_id
                        .parse()
                        .map_err(|e| DbError::Corrupt(format!("updates.manga_id: {e}")))?,
                    manga_title: row.get("manga_title"),
                    chapter_count: row.get("chapter_count"),
                    first_title: row.get("first_title"),
                    last_title: row.get("last_title"),
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|e| DbError::Corrupt(format!("updates.created_at: {e}")))?
                        .with_timezone(&Utc),
                })
            })
            .collect()
    }

    pub async fn prune_updates(&self, before: DateTime<Utc>) -> Result<()> {
        sqlx::query("DELETE FROM updates WHERE created_at < ?")
            .bind(before.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
