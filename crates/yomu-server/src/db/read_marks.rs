use chrono::Utc;
use uuid::Uuid;

use super::*;

impl Db {
    /// Mark chapters read for a user. Idempotent; unknown chapter ids are a
    /// constraint error (the FK catches stale client state).
    pub async fn mark_read(&self, user_id: Uuid, chapter_ids: &[Uuid]) -> Result<u32> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let mut affected = 0;
        for id in chapter_ids {
            let result = sqlx::query(
                "INSERT INTO read_chapters (user_id, chapter_id, at) VALUES (?, ?, ?)
                 ON CONFLICT (user_id, chapter_id) DO NOTHING",
            )
            .bind(user_id.to_string())
            .bind(id.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                    DbError::Constraint(format!("unknown chapter {id}"))
                }
                _ => DbError::Sqlx(e),
            })?;
            affected += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(affected)
    }

    pub async fn mark_unread(&self, user_id: Uuid, chapter_ids: &[Uuid]) -> Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut affected = 0;
        for id in chapter_ids {
            let result =
                sqlx::query("DELETE FROM read_chapters WHERE user_id = ? AND chapter_id = ?")
                    .bind(user_id.to_string())
                    .bind(id.to_string())
                    .execute(&mut *tx)
                    .await?;
            affected += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(affected)
    }

    /// Ids of a manga's chapters the user has read.
    pub async fn read_ids(
        &self,
        user_id: Uuid,
        manga_id: Uuid,
    ) -> Result<std::collections::HashSet<Uuid>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT r.chapter_id FROM read_chapters r
             JOIN chapters c ON c.id = r.chapter_id
             WHERE r.user_id = ? AND c.manga_id = ?",
        )
        .bind(user_id.to_string())
        .bind(manga_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(parse_uuid).collect()
    }

    /// Every chapter id this user has marked read (across all manga).
    pub async fn read_all_ids(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT chapter_id FROM read_chapters WHERE user_id = ?",
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(parse_uuid).collect()
    }
}
