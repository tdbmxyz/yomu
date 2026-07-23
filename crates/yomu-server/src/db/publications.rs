use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
use yomu_domain::{Locations, Locator, MangaDetails, Publication};

use super::*;

impl Db {
    /// Insert a publication with its units as freshly fetched from the source.
    pub async fn insert_publication(
        &self,
        source_id: &str,
        details: &MangaDetails,
        auto_download: bool,
    ) -> Result<Publication> {
        let id = Uuid::now_v7();
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO publications (id, source_id, source_key, title, description, cover_url,
                                       auto_download, added_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(source_id)
        .bind(&details.summary.key)
        .bind(&details.summary.title)
        .bind(&details.description)
        .bind(details.summary.cover_url.as_deref())
        .bind(auto_download)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DbError::Constraint("publication already in library".into())
            }
            _ => DbError::Sqlx(e),
        })?;
        insert_units(&mut tx, id, &details.chapters, now).await?;
        write_genres(&mut tx, id, &details.genres).await?;
        tx.commit().await?;
        self.get_publication(id).await
    }

    pub async fn get_publication(&self, id: Uuid) -> Result<Publication> {
        let row = sqlx::query_as::<_, PublicationRow>("SELECT * FROM publications WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        let mut publication = Publication::try_from(row)?;
        publication.genres = self.genres_for(id).await?;
        Ok(publication)
    }

    pub async fn list_publications(&self) -> Result<Vec<Publication>> {
        let rows = sqlx::query_as::<_, PublicationRow>(
            "SELECT * FROM publications ORDER BY title COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut genres = self.genres_by_publication().await?;
        rows.into_iter()
            .map(|row| {
                let mut publication = Publication::try_from(row)?;
                publication.genres = genres.remove(&publication.id).unwrap_or_default();
                Ok(publication)
            })
            .collect()
    }

    /// LocalFile publications, keyed for the streamer's upsert.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "streamer (2.x) entry points; test-only until then"
        )
    )]
    pub async fn list_local_publications(&self) -> Result<Vec<Publication>> {
        let rows = sqlx::query_as::<_, PublicationRow>(
            "SELECT * FROM publications WHERE file_path IS NOT NULL ORDER BY title COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Publication::try_from).collect()
    }

    /// Insert a streamer-discovered publication with its units.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "streamer (2.x) entry points; test-only until then"
        )
    )]
    pub async fn insert_local_publication(
        &self,
        path: &str,
        details: &MangaDetails,
    ) -> Result<Publication> {
        let id = Uuid::now_v7();
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO publications (id, kind, file_path, title, description, cover_url,
                                       auto_download, added_at)
             VALUES (?, 'comics', ?, ?, ?, ?, 0, ?)",
        )
        .bind(id.to_string())
        .bind(path)
        .bind(&details.summary.title)
        .bind(&details.description)
        .bind(details.summary.cover_url.as_deref())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DbError::Constraint("file already in library".into())
            }
            _ => DbError::Sqlx(e),
        })?;
        insert_units(&mut tx, id, &details.chapters, now).await?;
        write_genres(&mut tx, id, &details.genres).await?;
        tx.commit().await?;
        self.get_publication(id).await
    }

    /// Re-point a missing LocalFile publication at a renamed path (self-heal).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "streamer (2.x) entry points; test-only until then"
        )
    )]
    pub async fn repoint_local_publication(&self, id: Uuid, path: &str) -> Result<()> {
        sqlx::query("UPDATE publications SET file_path = ?, missing_since = NULL WHERE id = ?")
            .bind(path)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Flag (Some) or clear (None) a vanished LocalFile publication.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "streamer (2.x) entry points; test-only until then"
        )
    )]
    pub async fn set_missing_since(&self, id: Uuid, at: Option<DateTime<Utc>>) -> Result<()> {
        sqlx::query("UPDATE publications SET missing_since = ? WHERE id = ?")
            .bind(at)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Refresh scan-derived metadata (cover/description) without touching title.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "streamer (2.x) entry points; test-only until then"
        )
    )]
    pub async fn update_local_metadata(
        &self,
        id: Uuid,
        description: Option<&str>,
        cover_url: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE publications SET description = ?, cover_url = ? WHERE id = ?")
            .bind(description)
            .bind(cover_url)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Genres of one publication, alphabetically.
    pub async fn genres_for(&self, publication_id: Uuid) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT genre FROM publication_genres WHERE publication_id = ?
             ORDER BY genre COLLATE NOCASE",
        )
        .bind(publication_id.to_string())
        .fetch_all(&self.pool)
        .await?)
    }

    /// All genres grouped by publication, for the library list (one query).
    pub async fn genres_by_publication(
        &self,
    ) -> Result<std::collections::HashMap<Uuid, Vec<String>>> {
        let rows = sqlx::query(
            "SELECT publication_id, genre FROM publication_genres ORDER BY genre COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out: std::collections::HashMap<Uuid, Vec<String>> =
            std::collections::HashMap::new();
        for row in rows {
            let publication_id = parse_uuid(row.get::<String, _>("publication_id"))?;
            out.entry(publication_id)
                .or_default()
                .push(row.get("genre"));
        }
        Ok(out)
    }

    /// Replace a publication's genres (used on add and on refresh).
    pub async fn set_genres(&self, publication_id: Uuid, genres: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        write_genres(&mut tx, publication_id, genres).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Per-publication unit rollups for the library list, in one grouped
    /// query instead of a `list_units` + `read_ids` pair per publication.
    /// `user_id` scopes the unread count; pass the shared user (or, when
    /// signed out, any id that matches no reader — an empty string — so
    /// nothing counts as read).
    pub async fn library_rollups(
        &self,
        user_id: &str,
    ) -> Result<std::collections::HashMap<Uuid, LibraryRollup>> {
        let rows = sqlx::query(
            "SELECT c.publication_id AS publication_id,
                    COUNT(*) AS unit_count,
                    SUM(CASE WHEN c.download_state = 'downloaded' THEN 1 ELSE 0 END)
                        AS downloaded_count,
                    SUM(CASE WHEN r.unit_id IS NULL THEN 1 ELSE 0 END) AS unread_count,
                    MAX(c.fetched_at) AS latest_unit_at
             FROM reading_units c
             LEFT JOIN read_units r ON r.unit_id = c.id AND r.user_id = ?
             GROUP BY c.publication_id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    parse_uuid(row.get::<String, _>("publication_id"))?,
                    LibraryRollup {
                        unit_count: row.get::<i64, _>("unit_count") as u32,
                        downloaded_count: row.get::<i64, _>("downloaded_count") as u32,
                        unread_count: row.get::<i64, _>("unread_count") as u32,
                        latest_unit_at: row.get("latest_unit_at"),
                    },
                ))
            })
            .collect()
    }

    /// Every publication's merged current locator for one user, plus the
    /// locator unit's title, in a single window-function query (the same
    /// max-at/id-tie-break as `latest_position`).
    pub async fn latest_positions(
        &self,
        user_id: Uuid,
    ) -> Result<std::collections::HashMap<Uuid, (Locator, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT p.publication_id AS publication_id, p.unit_id AS unit_id, p.page AS page,
                    p.at AS at, ch.title AS title
             FROM (
                 SELECT publication_id, unit_id, page, at,
                        ROW_NUMBER() OVER (
                            PARTITION BY publication_id ORDER BY at DESC, id DESC
                        ) AS rn
                 FROM progress_events WHERE user_id = ?
             ) p
             LEFT JOIN reading_units ch ON ch.id = p.unit_id
             WHERE p.rn = 1",
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let publication_id = parse_uuid(row.get::<String, _>("publication_id"))?;
                let locator = Locator {
                    unit_id: parse_uuid(row.get::<String, _>("unit_id"))?,
                    locations: Locations::Page {
                        page: row.get::<i64, _>("page") as u32,
                    },
                    at: row.get("at"),
                };
                let title: Option<String> = row.get("title");
                Ok((publication_id, (locator, title)))
            })
            .collect()
    }

    pub async fn set_auto_download(&self, id: Uuid, auto_download: bool) -> Result<Publication> {
        let result = sqlx::query("UPDATE publications SET auto_download = ? WHERE id = ?")
            .bind(auto_download)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        self.get_publication(id).await
    }

    /// Move a publication to a category. The category must exist
    /// (publications.category has no FK — SQLite can't ALTER-ADD one — so
    /// membership is enforced here).
    pub async fn set_category(&self, id: Uuid, category: &str) -> Result<Publication> {
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM categories WHERE id = ?)")
                .bind(category)
                .fetch_one(&self.pool)
                .await?;
        if !known {
            return Err(DbError::Constraint(format!(
                "unknown category {category:?}"
            )));
        }
        let result = sqlx::query("UPDATE publications SET category = ? WHERE id = ?")
            .bind(category)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        self.get_publication(id).await
    }

    /// Publications the periodic updater should check: only categories with
    /// `update_enabled`, and never LocalFile publications (nothing to
    /// scrape; the streamer owns their refresh).
    pub async fn list_publications_for_update(&self) -> Result<Vec<Publication>> {
        let rows = sqlx::query_as::<_, PublicationRow>(
            "SELECT m.* FROM publications m
             JOIN categories c ON c.id = m.category
             WHERE c.update_enabled = 1 AND m.file_path IS NULL
             ORDER BY m.title COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Publication::try_from).collect()
    }

    /// Repoint a publication at its current source URL. Some sites rotate a
    /// volatile suffix on slug URLs, so the key stored at add-time drifts
    /// from the live listing; the browse annotation heals it in place.
    pub async fn update_source_key(&self, id: Uuid, source_key: &str) -> Result<()> {
        sqlx::query("UPDATE publications SET source_key = ? WHERE id = ?")
            .bind(source_key)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_last_checked(&self, id: Uuid, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE publications SET last_checked_at = ? WHERE id = ?")
            .bind(at)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_publication(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM publications WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    /// source_key → publication id for one source; backs the browse/search
    /// "already in library" annotation.
    pub async fn library_keys(
        &self,
        source_id: &str,
    ) -> Result<std::collections::HashMap<String, Uuid>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT source_key, id FROM publications WHERE source_id = ?",
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(key, id)| Ok((key, parse_uuid(id)?)))
            .collect()
    }
}
