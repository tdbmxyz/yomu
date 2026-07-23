use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
use yomu_domain::{ReadingUnit, UpdateEvent};

use super::*;

impl Db {
    /// Record one updater find: `units` are the round's new units for this
    /// publication, in listing order (see `sync::refresh_publication`).
    /// The `chapter_count` column name is 1.x legacy, kept by migration 0011.
    pub async fn add_update(&self, publication_id: Uuid, units: &[ReadingUnit]) -> Result<()> {
        let (Some(first), Some(last)) = (units.first(), units.last()) else {
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO updates (publication_id, chapter_count, first_title, last_title, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(publication_id.to_string())
        .bind(units.len() as u32)
        .bind(&first.title)
        .bind(&last.title)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Events strictly newer than `since`, newest first, joined with the
    /// publication title (events for since-removed publications are dropped).
    pub async fn updates_since(
        &self,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<UpdateEvent>> {
        let rows = sqlx::query(
            "SELECT u.publication_id, u.chapter_count, u.first_title, u.last_title, u.created_at,
                    m.title AS publication_title
             FROM updates u JOIN publications m ON m.id = u.publication_id
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
                let publication_id: String = row.get("publication_id");
                let created_at: String = row.get("created_at");
                Ok(UpdateEvent {
                    publication_id: publication_id
                        .parse()
                        .map_err(|e| DbError::Corrupt(format!("updates.publication_id: {e}")))?,
                    publication_title: row.get("publication_title"),
                    unit_count: row.get("chapter_count"),
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
