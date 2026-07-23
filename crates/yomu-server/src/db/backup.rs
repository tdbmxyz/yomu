use chrono::Utc;
use uuid::Uuid;
use yomu_domain::{Origin, ReadingUnit};

use super::*;

impl Db {
    /// Every unit row, for a backup. Read state is per-user and travels
    /// separately (see `read_all_ids`); `read` is left false here.
    pub async fn export_units(&self) -> Result<Vec<ReadingUnit>> {
        let rows =
            sqlx::query_as::<_, UnitRow>("SELECT * FROM reading_units ORDER BY publication_id")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter().map(ReadingUnit::try_from).collect()
    }

    pub async fn import_backup(
        &self,
        user_id: Uuid,
        backup: &yomu_domain::Backup,
    ) -> Result<yomu_domain::RestoreSummary> {
        let mut tx = self.pool.begin().await?;
        let now = Utc::now();
        let mut summary = yomu_domain::RestoreSummary {
            publications: 0,
            units: 0,
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

        for publication in &backup.publications {
            let (source_id, source_key, file_path) = match &publication.origin {
                Origin::Source {
                    source_id,
                    source_key,
                } => (Some(source_id.as_str()), Some(source_key.as_str()), None),
                Origin::LocalFile { path } => (None, None, Some(path.as_str())),
            };
            let r = sqlx::query(
                "INSERT INTO publications (id, kind, source_id, source_key, file_path, title,
                                           description, cover_url, auto_download, category,
                                           added_at, last_checked_at, missing_since)
                 VALUES (?, 'comics', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(publication.id.to_string())
            .bind(source_id)
            .bind(source_key)
            .bind(file_path)
            .bind(&publication.title)
            .bind(&publication.description)
            .bind(publication.cover_url.as_ref().map(|u| u.as_str()))
            .bind(publication.auto_download)
            .bind(&publication.category)
            .bind(publication.added_at)
            .bind(publication.last_checked_at)
            .bind(publication.missing_since)
            .execute(&mut *tx)
            .await?;
            // Genres ride along whether or not the publication row was new,
            // so a restore refreshes tags on publications already present.
            write_genres(&mut tx, publication.id, &publication.genres).await?;
            summary.publications += r.rows_affected() as u32;
        }

        for unit in &backup.units {
            // Download state is intentionally dropped: the pages aren't in
            // the backup, so a restored unit reads live until re-downloaded.
            // page_count is kept — it's true knowledge, not a local artifact.
            let r = sqlx::query(
                "INSERT INTO reading_units (id, publication_id, source_key, title, number,
                                            source_order, scanlator, fetched_at, published_at,
                                            page_count)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(unit.id.to_string())
            .bind(unit.publication_id.to_string())
            .bind(&unit.source_key)
            .bind(&unit.title)
            .bind(unit.number)
            .bind(unit.source_order)
            .bind(&unit.scanlator)
            .bind(unit.fetched_at)
            .bind(unit.published_at)
            .bind(unit.page_count)
            .execute(&mut *tx)
            .await?;
            summary.units += r.rows_affected() as u32;
        }

        for unit_id in &backup.read_unit_ids {
            // Skip marks whose unit didn't come along — the FK would reject
            // them and one stale id must not fail the whole restore.
            let known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM reading_units WHERE id = ?)")
                    .bind(unit_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !known {
                continue;
            }
            let r = sqlx::query(
                "INSERT INTO read_units (user_id, unit_id, at) VALUES (?, ?, ?)
                 ON CONFLICT (user_id, unit_id) DO NOTHING",
            )
            .bind(user_id.to_string())
            .bind(unit_id.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await?;
            summary.read_marks += r.rows_affected() as u32;
        }

        for event in &backup.progress {
            let publication_known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM publications WHERE id = ?)")
                    .bind(event.publication_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            let unit_known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM reading_units WHERE id = ?)")
                    .bind(event.unit_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !publication_known || !unit_known {
                continue;
            }
            let r = sqlx::query(
                "INSERT INTO progress_events (id, user_id, publication_id, unit_id, page,
                                              device, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
            )
            .bind(event.id.to_string())
            .bind(user_id.to_string())
            .bind(event.publication_id.to_string())
            .bind(event.unit_id.to_string())
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
