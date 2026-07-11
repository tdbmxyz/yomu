use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;
use yomu_domain::{Manga, MangaDetails, Position};

use super::*;

impl Db {
    /// Insert a manga with its chapters as freshly fetched from the source.
    pub async fn insert_manga(
        &self,
        source_id: &str,
        details: &MangaDetails,
        auto_download: bool,
    ) -> Result<Manga> {
        let id = Uuid::now_v7();
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO manga (id, source_id, source_key, title, description, cover_url,
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
                DbError::Constraint("manga already in library".into())
            }
            _ => DbError::Sqlx(e),
        })?;
        insert_chapters(&mut tx, id, &details.chapters, now).await?;
        tx.commit().await?;
        self.get_manga(id).await
    }

    pub async fn get_manga(&self, id: Uuid) -> Result<Manga> {
        let row = sqlx::query_as::<_, MangaRow>("SELECT * FROM manga WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        Manga::try_from(row)
    }

    pub async fn list_manga(&self) -> Result<Vec<Manga>> {
        let rows =
            sqlx::query_as::<_, MangaRow>("SELECT * FROM manga ORDER BY title COLLATE NOCASE")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter().map(Manga::try_from).collect()
    }

    /// Per-manga chapter rollups for the library list, in one grouped query
    /// instead of a `list_chapters` + `read_ids` pair per manga. `user_id`
    /// scopes the unread count; pass the shared user (or, when signed out,
    /// any id that matches no reader — an empty string — so nothing counts
    /// as read).
    pub async fn library_rollups(
        &self,
        user_id: &str,
    ) -> Result<std::collections::HashMap<Uuid, LibraryRollup>> {
        let rows = sqlx::query(
            "SELECT c.manga_id AS manga_id,
                    COUNT(*) AS chapter_count,
                    SUM(CASE WHEN c.download_state = 'downloaded' THEN 1 ELSE 0 END)
                        AS downloaded_count,
                    SUM(CASE WHEN r.chapter_id IS NULL THEN 1 ELSE 0 END) AS unread_count,
                    MAX(c.fetched_at) AS latest_chapter_at
             FROM chapters c
             LEFT JOIN read_chapters r ON r.chapter_id = c.id AND r.user_id = ?
             GROUP BY c.manga_id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    parse_uuid(row.get::<String, _>("manga_id"))?,
                    LibraryRollup {
                        chapter_count: row.get::<i64, _>("chapter_count") as u32,
                        downloaded_count: row.get::<i64, _>("downloaded_count") as u32,
                        unread_count: row.get::<i64, _>("unread_count") as u32,
                        latest_chapter_at: row.get("latest_chapter_at"),
                    },
                ))
            })
            .collect()
    }

    /// Every manga's merged current position for one user, plus the position
    /// chapter's title, in a single window-function query (the same
    /// max-at/id-tie-break as `latest_position`).
    pub async fn latest_positions(
        &self,
        user_id: Uuid,
    ) -> Result<std::collections::HashMap<Uuid, (Position, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT p.manga_id AS manga_id, p.chapter_id AS chapter_id, p.page AS page,
                    p.at AS at, ch.title AS title
             FROM (
                 SELECT manga_id, chapter_id, page, at,
                        ROW_NUMBER() OVER (
                            PARTITION BY manga_id ORDER BY at DESC, id DESC
                        ) AS rn
                 FROM progress_events WHERE user_id = ?
             ) p
             LEFT JOIN chapters ch ON ch.id = p.chapter_id
             WHERE p.rn = 1",
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let manga_id = parse_uuid(row.get::<String, _>("manga_id"))?;
                let position = Position {
                    chapter_id: parse_uuid(row.get::<String, _>("chapter_id"))?,
                    page: row.get::<i64, _>("page") as u32,
                    at: row.get("at"),
                };
                let title: Option<String> = row.get("title");
                Ok((manga_id, (position, title)))
            })
            .collect()
    }

    pub async fn set_auto_download(&self, id: Uuid, auto_download: bool) -> Result<Manga> {
        let result = sqlx::query("UPDATE manga SET auto_download = ? WHERE id = ?")
            .bind(auto_download)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        self.get_manga(id).await
    }

    /// Move a manga to a category. The category must exist (manga.category
    /// has no FK — SQLite can't ALTER-ADD one — so membership is enforced
    /// here).
    pub async fn set_category(&self, id: Uuid, category: &str) -> Result<Manga> {
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
        let result = sqlx::query("UPDATE manga SET category = ? WHERE id = ?")
            .bind(category)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        self.get_manga(id).await
    }

    /// Manga the periodic updater should check: only categories with
    /// `update_enabled`.
    pub async fn list_manga_for_update(&self) -> Result<Vec<Manga>> {
        let rows = sqlx::query_as::<_, MangaRow>(
            "SELECT m.* FROM manga m
             JOIN categories c ON c.id = m.category
             WHERE c.update_enabled = 1
             ORDER BY m.title COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Manga::try_from).collect()
    }

    pub async fn set_last_checked(&self, id: Uuid, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE manga SET last_checked_at = ? WHERE id = ?")
            .bind(at)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_manga(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM manga WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    /// source_key → manga id for one source; backs the browse/search
    /// "already in library" annotation.
    pub async fn library_keys(
        &self,
        source_id: &str,
    ) -> Result<std::collections::HashMap<String, Uuid>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT source_key, id FROM manga WHERE source_id = ?",
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(key, id)| Ok((key, parse_uuid(id)?)))
            .collect()
    }
}
