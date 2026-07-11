use chrono::Utc;
use uuid::Uuid;
use yomu_domain::Chapter;

use super::*;

impl Db {
    /// Queue chapters for download; already queued/downloaded ones are left
    /// alone. Returns how many were actually (re)queued.
    pub async fn mark_pending(&self, chapter_ids: &[Uuid]) -> Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut queued = 0;
        for id in chapter_ids {
            let result = sqlx::query(
                "UPDATE chapters SET download_state = 'pending', download_error = NULL
                 WHERE id = ? AND download_state IN ('none', 'failed')",
            )
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            queued += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(queued)
    }

    pub async fn next_pending_download(&self) -> Result<Option<Chapter>> {
        let row = sqlx::query_as::<_, ChapterRow>(
            "SELECT * FROM chapters WHERE download_state = 'pending' ORDER BY fetched_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(Chapter::try_from).transpose()
    }

    pub async fn set_downloading(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE chapters SET download_state = 'downloading' WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record a download outcome. Returns `false` when the chapter row no
    /// longer exists (manga deleted mid-download) so the caller can discard
    /// the files it just wrote.
    pub async fn finish_download(
        &self,
        id: Uuid,
        outcome: std::result::Result<u32, String>,
    ) -> Result<bool> {
        let now = Utc::now();
        let result = match outcome {
            Ok(page_count) => {
                sqlx::query(
                    "UPDATE chapters SET download_state = 'downloaded', downloaded_at = ?,
                                         page_count = ?, download_error = NULL
                     WHERE id = ?",
                )
                .bind(now)
                .bind(page_count)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?
            }
            Err(reason) => {
                sqlx::query(
                    "UPDATE chapters SET download_state = 'failed', downloaded_at = ?,
                                         download_error = ?
                     WHERE id = ?",
                )
                .bind(now)
                .bind(reason)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?
            }
        };
        Ok(result.rows_affected() > 0)
    }

    /// Forget the server copies of these chapters: rows go back to
    /// 'none' (page_count survives — still true knowledge). Returns the
    /// ids that actually were downloaded, so the caller can delete their
    /// page directories.
    pub async fn remove_downloads(&self, chapter_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let mut removed = Vec::new();
        for id in chapter_ids {
            let result = sqlx::query(
                "UPDATE chapters SET download_state = 'none', downloaded_at = NULL,
                                     download_error = NULL
                 WHERE id = ? AND download_state = 'downloaded'",
            )
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                removed.push(*id);
            }
        }
        Ok(removed)
    }
}
