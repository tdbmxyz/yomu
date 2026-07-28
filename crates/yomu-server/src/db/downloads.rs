use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;
use yomu_domain::ReadingUnit;

use super::*;

/// Why a download produced no pages.
#[derive(Debug, Clone)]
pub enum DownloadFailure {
    /// Something broke on the way — a retry may well work.
    Failed(String),
    /// The source served the chapter but does not offer it (premium,
    /// locked). Not a fault: retrying accomplishes nothing until the source
    /// frees the chapter, so the bulk and automatic paths pass it over.
    Unavailable(String),
}

/// So the many call sites that only ever produce a plain failure keep
/// reading as `Err(reason)`. Deliberately two narrow impls rather than a
/// blanket `From<S: Into<String>>`: the blanket one converts *anything*
/// string-like, so an error type that already distinguishes unavailable from
/// broken — [`yomu_source::SourceError`] does — could be stringified into
/// `Failed` at a future call site with nothing to flag it. Reaching this
/// conversion should require having thrown the distinction away on purpose.
impl From<String> for DownloadFailure {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl From<&str> for DownloadFailure {
    fn from(reason: &str) -> Self {
        Self::Failed(reason.to_owned())
    }
}

impl Db {
    /// Queue chapters the user asked for; already queued/downloaded ones are
    /// left alone. An unavailable chapter is included on purpose: it can
    /// stop being premium, and asking for it explicitly is the user's call.
    /// Returns how many were actually (re)queued.
    pub async fn mark_pending(&self, unit_ids: &[Uuid]) -> Result<u32> {
        self.queue(unit_ids, true).await
    }

    /// Queue chapters nobody asked for (the auto-download sweep). Skips
    /// unavailable ones, which would otherwise be re-attempted every sweep
    /// for as long as the source keeps them locked.
    pub async fn mark_pending_new(&self, unit_ids: &[Uuid]) -> Result<u32> {
        self.queue(unit_ids, false).await
    }

    async fn queue(&self, unit_ids: &[Uuid], include_unavailable: bool) -> Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut queued = 0;
        for id in unit_ids {
            let result = sqlx::query(
                "UPDATE reading_units SET download_state = 'pending', download_error = NULL
                 WHERE id = ?
                   AND (download_state IN ('none', 'failed')
                        OR (? AND download_state = 'unavailable'))",
            )
            .bind(id.to_string())
            .bind(include_unavailable)
            .execute(&mut *tx)
            .await?;
            queued += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(queued)
    }

    pub async fn next_pending_download(&self) -> Result<Option<ReadingUnit>> {
        let row = sqlx::query_as::<_, UnitRow>(
            "SELECT * FROM reading_units WHERE download_state = 'pending'
             ORDER BY fetched_at, number IS NULL, number LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(ReadingUnit::try_from).transpose()
    }

    pub async fn set_downloading(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE reading_units SET download_state = 'downloading' WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record a download outcome. Returns `false` when the chapter row no
    /// longer exists (publication deleted mid-download) so the caller can discard
    /// the files it just wrote.
    pub async fn finish_download(
        &self,
        id: Uuid,
        outcome: std::result::Result<u32, DownloadFailure>,
    ) -> Result<bool> {
        let now = Utc::now();
        let result = match outcome {
            Ok(page_count) => {
                sqlx::query(
                    "UPDATE reading_units SET download_state = 'downloaded', downloaded_at = ?,
                                         page_count = ?, download_error = NULL
                     WHERE id = ?",
                )
                .bind(now)
                .bind(page_count)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?
            }
            Err(failure) => {
                let (state, reason) = match failure {
                    DownloadFailure::Failed(reason) => ("failed", reason),
                    DownloadFailure::Unavailable(reason) => ("unavailable", reason),
                };
                sqlx::query(
                    "UPDATE reading_units SET download_state = ?, downloaded_at = ?,
                                         download_error = ?
                     WHERE id = ?",
                )
                .bind(state)
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
    pub async fn remove_downloads(&self, unit_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let mut removed = Vec::new();
        for id in unit_ids {
            let result = sqlx::query(
                "UPDATE reading_units SET download_state = 'none', downloaded_at = NULL,
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

    /// Chapters currently in the download queue (downloading, then pending,
    /// then failed, then unavailable), oldest-first within each state — for
    /// the Downloads view, which groups the unavailable ones separately.
    /// The tiebreak matches [`Self::next_pending_download`] exactly: one sync
    /// stamps a whole listing with the same `fetched_at`, so without it the
    /// list falls back to insertion order — newest first, the reverse of the
    /// order the worker takes.
    pub async fn download_queue(&self) -> Result<Vec<ReadingUnit>> {
        let rows = sqlx::query_as::<_, UnitRow>(
            "SELECT * FROM reading_units
             WHERE download_state IN ('downloading', 'pending', 'failed', 'unavailable')
             ORDER BY CASE download_state
                          WHEN 'downloading' THEN 0
                          WHEN 'pending' THEN 1
                          WHEN 'failed' THEN 2
                          ELSE 3
                      END,
                      fetched_at, number IS NULL, number",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ReadingUnit::try_from).collect()
    }

    /// Titles for the given publication ids (for labelling queue entries).
    pub async fn publication_titles(
        &self,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>> {
        let mut out = std::collections::HashMap::new();
        for id in ids {
            if let Some(title) =
                sqlx::query_scalar::<_, String>("SELECT title FROM publications WHERE id = ?")
                    .bind(id.to_string())
                    .fetch_optional(&self.pool)
                    .await?
            {
                out.insert(*id, title);
            }
        }
        Ok(out)
    }

    /// (downloaded chapter count, total downloaded pages) across the library.
    pub async fn downloaded_summary(&self) -> Result<(u32, u32)> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS chapters, COALESCE(SUM(page_count), 0) AS pages
             FROM reading_units WHERE download_state = 'downloaded'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((
            row.get::<i64, _>("chapters") as u32,
            row.get::<i64, _>("pages") as u32,
        ))
    }

    /// Re-queue failed chapters (failed → pending). Unavailable chapters are
    /// deliberately not touched: they are not failures, and retrying one in
    /// bulk changes nothing until the source frees it. Returns rows changed.
    pub async fn retry_failed(&self, unit_ids: &[Uuid]) -> Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut affected = 0;
        for id in unit_ids {
            let result = sqlx::query(
                "UPDATE reading_units SET download_state = 'pending', download_error = NULL
                 WHERE id = ? AND download_state = 'failed'",
            )
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            affected += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(affected)
    }

    /// Drop chapters from the queue (pending, failed or unavailable → none).
    /// Downloading and downloaded chapters are untouched. Dismissing is how
    /// the user clears an unavailable chapter they have seen and accepted.
    /// Returns rows changed.
    pub async fn dismiss_downloads(&self, unit_ids: &[Uuid]) -> Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut affected = 0;
        for id in unit_ids {
            let result = sqlx::query(
                "UPDATE reading_units SET download_state = 'none', download_error = NULL
                 WHERE id = ? AND download_state IN ('pending', 'failed', 'unavailable')",
            )
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            affected += result.rows_affected() as u32;
        }
        tx.commit().await?;
        Ok(affected)
    }
}
