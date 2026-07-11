use chrono::Utc;
use uuid::Uuid;
use yomu_domain::Chapter;

use super::*;

impl Db {
    /// Every chapter row, for a backup. Read state is per-user and travels
    /// separately (see `read_all_ids`); `read` is left false here.
    pub async fn export_chapters(&self) -> Result<Vec<Chapter>> {
        let rows = sqlx::query_as::<_, ChapterRow>("SELECT * FROM chapters ORDER BY manga_id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Chapter::try_from).collect()
    }

    pub async fn import_backup(
        &self,
        user_id: Uuid,
        backup: &yomu_domain::Backup,
    ) -> Result<yomu_domain::RestoreSummary> {
        let mut tx = self.pool.begin().await?;
        let now = Utc::now();
        let mut summary = yomu_domain::RestoreSummary {
            manga: 0,
            chapters: 0,
            categories: 0,
            read_marks: 0,
            progress_events: 0,
        };

        for category in &backup.categories {
            let r = sqlx::query(
                "INSERT INTO categories (id, name, position, update_enabled)
                 VALUES (?, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
            )
            .bind(&category.id)
            .bind(&category.name)
            .bind(category.position)
            .bind(category.update_enabled)
            .execute(&mut *tx)
            .await?;
            summary.categories += r.rows_affected() as u32;
        }

        for manga in &backup.manga {
            let r = sqlx::query(
                "INSERT INTO manga (id, source_id, source_key, title, description, cover_url,
                                    auto_download, category, added_at, last_checked_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(manga.id.to_string())
            .bind(&manga.source_id)
            .bind(&manga.source_key)
            .bind(&manga.title)
            .bind(&manga.description)
            .bind(manga.cover_url.as_ref().map(|u| u.as_str()))
            .bind(manga.auto_download)
            .bind(&manga.category)
            .bind(manga.added_at)
            .bind(manga.last_checked_at)
            .execute(&mut *tx)
            .await?;
            // Genres ride along whether or not the manga row was new, so a
            // restore refreshes tags on manga already present.
            write_genres(&mut tx, manga.id, &manga.genres).await?;
            summary.manga += r.rows_affected() as u32;
        }

        for chapter in &backup.chapters {
            // Download state is intentionally dropped: the pages aren't in
            // the backup, so a restored chapter reads live until re-downloaded.
            // page_count is kept — it's true knowledge, not a local artifact.
            let r = sqlx::query(
                "INSERT INTO chapters (id, manga_id, source_key, title, number, source_order,
                                       scanlator, fetched_at, published_at, page_count)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(chapter.id.to_string())
            .bind(chapter.manga_id.to_string())
            .bind(&chapter.source_key)
            .bind(&chapter.title)
            .bind(chapter.number)
            .bind(chapter.source_order)
            .bind(&chapter.scanlator)
            .bind(chapter.fetched_at)
            .bind(chapter.published_at)
            .bind(chapter.page_count)
            .execute(&mut *tx)
            .await?;
            summary.chapters += r.rows_affected() as u32;
        }

        for chapter_id in &backup.read_chapter_ids {
            // Skip marks whose chapter didn't come along — the FK would reject
            // them and one stale id must not fail the whole restore.
            let known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chapters WHERE id = ?)")
                    .bind(chapter_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !known {
                continue;
            }
            let r = sqlx::query(
                "INSERT INTO read_chapters (user_id, chapter_id, at) VALUES (?, ?, ?)
                 ON CONFLICT (user_id, chapter_id) DO NOTHING",
            )
            .bind(user_id.to_string())
            .bind(chapter_id.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await?;
            summary.read_marks += r.rows_affected() as u32;
        }

        for event in &backup.progress {
            let manga_known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM manga WHERE id = ?)")
                    .bind(event.manga_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            let chapter_known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chapters WHERE id = ?)")
                    .bind(event.chapter_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !manga_known || !chapter_known {
                continue;
            }
            let r = sqlx::query(
                "INSERT INTO progress_events (id, user_id, manga_id, chapter_id, page, device, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
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
            summary.progress_events += r.rows_affected() as u32;
        }

        tx.commit().await?;
        Ok(summary)
    }
}
