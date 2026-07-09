//! SQLite persistence. Same conventions as chaos: UUIDs as hyphenated TEXT,
//! timestamps RFC3339, all row↔domain mapping in this module only.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;
use yomu_domain::{
    Category, Chapter, ChapterRef, DownloadState, Manga, MangaDetails, Position, ProgressEvent,
    User,
};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Constraint(String),
    #[error("invalid stored data: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

pub type Result<T> = std::result::Result<T, DbError>;

/// What a chapter sync did, beyond upserting rows. `file_ops` are the
/// filesystem follow-ups the caller must apply: this module only owns rows,
/// the downloaded pages live under `data_dir/<manga>/<chapter>/` (see
/// `AppState::chapter_dir`).
pub struct ChapterSync {
    /// Chapters that weren't known before, in listing order. Twins that
    /// merely replaced a re-uploaded chapter the user already had are not
    /// included (they must not re-notify or re-trigger auto-download).
    pub new_chapters: Vec<Chapter>,
    pub file_ops: Vec<ChapterFileOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterFileOp {
    /// The chapter row was deleted; drop its page directory.
    Remove { chapter: Uuid },
    /// A downloaded chapter was merged into its re-uploaded twin; move the
    /// pages so the twin serves them without re-downloading.
    Rename { from: Uuid, to: Uuid },
}

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn connect(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| DbError::Constraint(format!("creating {}: {e}", parent.display())))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        Self::with_options(options).await
    }

    #[cfg(test)]
    pub async fn in_memory() -> Result<Self> {
        use std::str::FromStr;
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("valid memory dsn")
            .foreign_keys(true);
        Self::with_options(options).await
    }

    async fn with_options(options: SqliteConnectOptions) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        // Recover from a crash mid-download: those chapters are re-queued.
        sqlx::query(
            "UPDATE chapters SET download_state = 'pending' WHERE download_state = 'downloading'",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    // ---- manga ----

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
        .bind(details.summary.cover_url.as_ref().map(url::Url::as_str))
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

    // ---- categories ----

    pub async fn list_categories(&self) -> Result<Vec<Category>> {
        let rows =
            sqlx::query_as::<_, CategoryRow>("SELECT * FROM categories ORDER BY position, id")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(Category::from).collect())
    }

    pub async fn set_category_update(&self, id: &str, update_enabled: bool) -> Result<Category> {
        let result = sqlx::query("UPDATE categories SET update_enabled = ? WHERE id = ?")
            .bind(update_enabled)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        let row = sqlx::query_as::<_, CategoryRow>("SELECT * FROM categories WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Category::from(row))
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

    // ---- chapters ----

    /// Merge a fresh chapter listing from the source: new chapters are
    /// inserted, existing ones keep their id and download state. Returns
    /// the newly inserted chapters.
    pub async fn sync_chapters(
        &self,
        manga_id: Uuid,
        listing: &[ChapterRef],
    ) -> Result<ChapterSync> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let existing: std::collections::HashSet<String> =
            sqlx::query_scalar::<_, String>("SELECT source_key FROM chapters WHERE manga_id = ?")
                .bind(manga_id.to_string())
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect();

        // A scraped listing can contain the same chapter twice; keep the
        // first occurrence, otherwise the second upsert discards an id we
        // just recorded as new.
        let mut current_keys = std::collections::HashSet::new();
        let listing: Vec<&ChapterRef> = listing
            .iter()
            .filter(|c| current_keys.insert(c.key.clone()))
            .collect();

        let mut new_ids = Vec::new();
        for chapter in &listing {
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO chapters (id, manga_id, source_key, title, number, source_order,
                                       scanlator, fetched_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (manga_id, source_key)
                 DO UPDATE SET title = excluded.title, number = excluded.number,
                               source_order = excluded.source_order",
            )
            .bind(id.to_string())
            .bind(manga_id.to_string())
            .bind(&chapter.key)
            .bind(&chapter.title)
            .bind(chapter.number)
            .bind(chapter.source_order)
            .bind(&chapter.scanlator)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if !existing.contains(&chapter.key) {
                new_ids.push(id);
            }
        }

        // Reconcile: a source can move a chapter to a new URL (a re-upload),
        // leaving the old row behind next to its twin — a duplicate the user
        // sees ("Episode 45" twice, different keys). The insert/update above
        // never removes it, so it accumulates across updater runs in a
        // long-lived library. A stale row (fell out of the listing) with a
        // recognizable twin still in it — same number, or same title when
        // unnumbered — is merged into the twin: read marks and the reading
        // journal follow, a downloaded copy is handed over, and the row goes
        // away. Stale rows without a twin are dropped only when nothing is
        // saved locally (state 'none'/'failed'), so downloaded content is
        // never discarded. Guarded by a non-empty listing so a transient
        // empty/failed scrape can't wipe a manga (selector sources already
        // error on an empty chapter list, but be defensive).
        let mut file_ops = Vec::new();
        let mut merged_twins = std::collections::HashSet::new();
        if !current_keys.is_empty() {
            type ChapterMergeRow = (String, String, Option<f64>, String, String, Option<i64>);
            let rows: Vec<ChapterMergeRow> = sqlx::query_as(
                "SELECT id, source_key, number, title, download_state, page_count
                 FROM chapters WHERE manga_id = ?",
            )
            .bind(manga_id.to_string())
            .fetch_all(&mut *tx)
            .await?;
            let (stale, mut live): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|(_, key, ..)| !current_keys.contains(key));

            for (id, _, number, title, state, page_count) in stale {
                let id = parse_uuid(id)?;
                let twins: Vec<usize> = live
                    .iter()
                    .enumerate()
                    .filter(|(_, (.., n, t, _, _))| match number {
                        Some(number) => *n == Some(number),
                        None => n.is_none() && *t == title,
                    })
                    .map(|(i, _)| i)
                    .collect();
                match twins.as_slice() {
                    &[i] => {
                        let twin_id = parse_uuid(live[i].0.clone())?;
                        // Read marks and the reading journal follow the twin
                        // (the journal's one exception to append-only: the
                        // chapter it points at is being replaced).
                        sqlx::query(
                            "INSERT OR IGNORE INTO read_chapters (user_id, chapter_id, at)
                             SELECT user_id, ?, at FROM read_chapters WHERE chapter_id = ?",
                        )
                        .bind(twin_id.to_string())
                        .bind(id.to_string())
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE progress_events SET chapter_id = ? WHERE chapter_id = ?",
                        )
                        .bind(twin_id.to_string())
                        .bind(id.to_string())
                        .execute(&mut *tx)
                        .await?;
                        if state == "downloaded" && live[i].4 != "downloaded" {
                            sqlx::query(
                                "UPDATE chapters
                                 SET download_state = 'downloaded', download_error = NULL,
                                     downloaded_at = (SELECT downloaded_at FROM chapters WHERE id = ?),
                                     page_count = ?
                                 WHERE id = ?",
                            )
                            .bind(id.to_string())
                            .bind(page_count)
                            .bind(twin_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                            live[i].4 = "downloaded".into();
                            file_ops.push(ChapterFileOp::Rename {
                                from: id,
                                to: twin_id,
                            });
                        } else {
                            file_ops.push(ChapterFileOp::Remove { chapter: id });
                        }
                        sqlx::query("DELETE FROM chapters WHERE id = ?")
                            .bind(id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        merged_twins.insert(twin_id);
                    }
                    // No twin (or an ambiguous set): keep downloaded/in-flight
                    // rows, drop the rest.
                    _ if state == "none" || state == "failed" => {
                        sqlx::query("DELETE FROM chapters WHERE id = ?")
                            .bind(id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        file_ops.push(ChapterFileOp::Remove { chapter: id });
                    }
                    _ => {}
                }
            }
        }
        tx.commit().await?;

        let mut new_chapters = Vec::new();
        for id in new_ids {
            if !merged_twins.contains(&id) {
                new_chapters.push(self.get_chapter(id).await?);
            }
        }
        Ok(ChapterSync {
            new_chapters,
            file_ops,
        })
    }

    pub async fn get_chapter(&self, id: Uuid) -> Result<Chapter> {
        let row = sqlx::query_as::<_, ChapterRow>("SELECT * FROM chapters WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        Chapter::try_from(row)
    }

    /// Chapters in reading order: by number when present, source listing
    /// order (reversed, sources list newest first) as fallback.
    pub async fn list_chapters(&self, manga_id: Uuid) -> Result<Vec<Chapter>> {
        let rows = sqlx::query_as::<_, ChapterRow>(
            "SELECT * FROM chapters WHERE manga_id = ?
             ORDER BY number IS NULL, number ASC, source_order DESC",
        )
        .bind(manga_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Chapter::try_from).collect()
    }

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

    // ---- read marks ----

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

    pub async fn set_page_count(&self, id: Uuid, page_count: u32) -> Result<()> {
        sqlx::query("UPDATE chapters SET page_count = ? WHERE id = ?")
            .bind(page_count)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- users & sessions ----

    pub async fn user_by_id(&self, id: Uuid) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        User::try_from(row)
    }

    /// User for an OIDC subject, created or refreshed from the provider's
    /// claims. The username falls back to the subject on collision (two
    /// providers' users sharing a preferred_username).
    pub async fn upsert_oidc_user(
        &self,
        subject: &str,
        username: &str,
        display_name: &str,
    ) -> Result<User> {
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(id) = existing {
            let id = parse_uuid(id)?;
            sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
                .bind(display_name)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
            return self.user_by_id(id).await;
        }

        let id = Uuid::now_v7();
        let insert = |username: String| {
            sqlx::query(
                "INSERT INTO users (id, subject, username, display_name, created_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(subject.to_string())
            .bind(username)
            .bind(display_name.to_string())
            .bind(Utc::now())
        };
        let result = insert(username.trim().to_lowercase())
            .execute(&self.pool)
            .await;
        match result {
            Ok(_) => {}
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                insert(format!("{}-{subject}", username.trim().to_lowercase()))
                    .execute(&self.pool)
                    .await?;
            }
            Err(e) => return Err(e.into()),
        }
        self.user_by_id(id).await
    }

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(user_id.to_string())
        .bind(Utc::now())
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        // Opportunistic cleanup; logins are rare enough that this is free.
        sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resolve a session token hash to its (non-expired) user.
    pub async fn user_by_session(&self, token_hash: &str) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            "SELECT u.* FROM users u
             JOIN sessions s ON s.user_id = u.id
             WHERE s.token_hash = ? AND s.expires_at >= ?",
        )
        .bind(token_hash)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DbError::NotFound)?;
        User::try_from(row)
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- progress journal ----

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
            if !known {
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
}

async fn insert_chapters(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    manga_id: Uuid,
    chapters: &[ChapterRef],
    now: DateTime<Utc>,
) -> Result<()> {
    for chapter in chapters {
        sqlx::query(
            "INSERT INTO chapters (id, manga_id, source_key, title, number, source_order,
                                   scanlator, fetched_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (manga_id, source_key) DO NOTHING",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(manga_id.to_string())
        .bind(&chapter.key)
        .bind(&chapter.title)
        .bind(chapter.number)
        .bind(chapter.source_order)
        .bind(&chapter.scanlator)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn parse_uuid(s: String) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| DbError::Corrupt(format!("invalid uuid {s:?}")))
}

fn parse_url_opt(s: Option<String>) -> Result<Option<url::Url>> {
    s.map(|s| {
        s.parse()
            .map_err(|_| DbError::Corrupt(format!("invalid url {s:?}")))
    })
    .transpose()
}

// ---- row types ----

#[derive(sqlx::FromRow)]
struct MangaRow {
    id: String,
    source_id: String,
    source_key: String,
    title: String,
    description: Option<String>,
    cover_url: Option<String>,
    auto_download: bool,
    category: String,
    added_at: DateTime<Utc>,
    last_checked_at: Option<DateTime<Utc>>,
}

impl TryFrom<MangaRow> for Manga {
    type Error = DbError;

    fn try_from(row: MangaRow) -> Result<Self> {
        Ok(Manga {
            id: parse_uuid(row.id)?,
            source_id: row.source_id,
            source_key: row.source_key,
            title: row.title,
            description: row.description,
            cover_url: parse_url_opt(row.cover_url)?,
            auto_download: row.auto_download,
            category: row.category,
            added_at: row.added_at,
            last_checked_at: row.last_checked_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    #[allow(dead_code)]
    subject: Option<String>,
    username: String,
    display_name: String,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = DbError;

    fn try_from(row: UserRow) -> Result<Self> {
        Ok(User {
            id: parse_uuid(row.id)?,
            username: row.username,
            display_name: row.display_name,
        })
    }
}

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: String,
    name: String,
    position: i64,
    update_enabled: bool,
}

impl From<CategoryRow> for Category {
    fn from(row: CategoryRow) -> Self {
        Category {
            id: row.id,
            name: row.name,
            position: row.position as u32,
            update_enabled: row.update_enabled,
        }
    }
}

#[derive(sqlx::FromRow)]
struct ChapterRow {
    id: String,
    manga_id: String,
    source_key: String,
    title: String,
    number: Option<f64>,
    source_order: i64,
    scanlator: Option<String>,
    fetched_at: DateTime<Utc>,
    download_state: String,
    downloaded_at: Option<DateTime<Utc>>,
    download_error: Option<String>,
    page_count: Option<i64>,
}

impl TryFrom<ChapterRow> for Chapter {
    type Error = DbError;

    fn try_from(row: ChapterRow) -> Result<Self> {
        let download = match row.download_state.as_str() {
            "none" => DownloadState::None,
            "pending" => DownloadState::Pending,
            "downloading" => DownloadState::Downloading,
            "downloaded" => DownloadState::Downloaded {
                at: row
                    .downloaded_at
                    .ok_or_else(|| DbError::Corrupt("downloaded without timestamp".into()))?,
            },
            "failed" => DownloadState::Failed {
                at: row
                    .downloaded_at
                    .ok_or_else(|| DbError::Corrupt("failed without timestamp".into()))?,
                reason: row.download_error.unwrap_or_default(),
            },
            other => return Err(DbError::Corrupt(format!("download_state {other:?}"))),
        };
        Ok(Chapter {
            id: parse_uuid(row.id)?,
            manga_id: parse_uuid(row.manga_id)?,
            source_key: row.source_key,
            title: row.title,
            number: row.number,
            source_order: row.source_order as u32,
            scanlator: row.scanlator,
            fetched_at: row.fetched_at,
            download,
            page_count: row.page_count.map(|c| c as u32),
            // Per-user; filled at the API layer from `read_ids`.
            read: false,
        })
    }
}

#[derive(sqlx::FromRow)]
struct EventRow {
    seq: i64,
    id: String,
    manga_id: String,
    chapter_id: String,
    page: i64,
    device: String,
    at: DateTime<Utc>,
}

impl TryFrom<EventRow> for ProgressEvent {
    type Error = DbError;

    fn try_from(row: EventRow) -> Result<Self> {
        Ok(ProgressEvent {
            id: parse_uuid(row.id)?,
            manga_id: parse_uuid(row.manga_id)?,
            chapter_id: parse_uuid(row.chapter_id)?,
            page: row.page as u32,
            device: row.device,
            at: row.at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yomu_domain::{MangaSummary, merge_position};

    /// The seeded single-account user (see migration 0004).
    const SHARED: Uuid = Uuid::nil();

    fn details(key: &str, chapters: &[(&str, Option<f64>)]) -> MangaDetails {
        MangaDetails {
            summary: MangaSummary {
                key: key.into(),
                title: format!("Manga {key}"),
                cover_url: None,
            },
            description: Some("desc".into()),
            chapters: chapters
                .iter()
                .enumerate()
                .map(|(i, (ck, number))| ChapterRef {
                    key: (*ck).into(),
                    title: format!("Chapter {ck}"),
                    number: *number,
                    source_order: i as u32,
                    scanlator: None,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn library_lifecycle_and_chapter_sync() {
        let db = Db::in_memory().await.unwrap();

        let manga = db
            .insert_manga(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                false,
            )
            .await
            .unwrap();
        assert_eq!(db.list_chapters(manga.id).await.unwrap().len(), 2);

        // Duplicate add is a constraint error, not a second row.
        assert!(matches!(
            db.insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
                .await,
            Err(DbError::Constraint(_))
        ));

        // Re-sync with one new chapter: only the new one is returned, the
        // existing ones keep their ids.
        let before = db.list_chapters(manga.id).await.unwrap();
        let new = db
            .sync_chapters(
                manga.id,
                &details(
                    "m1",
                    &[("c3", Some(3.0)), ("c2", Some(2.0)), ("c1", Some(1.0))],
                )
                .chapters,
            )
            .await
            .unwrap();
        let new = new.new_chapters;
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].number, Some(3.0));
        let after = db.list_chapters(manga.id).await.unwrap();
        assert_eq!(after.len(), 3);
        // Reading order: 1, 2, 3.
        assert_eq!(
            after.iter().map(|c| c.number.unwrap()).collect::<Vec<_>>(),
            [1.0, 2.0, 3.0]
        );
        let old_c1 = before.iter().find(|c| c.number == Some(1.0)).unwrap();
        let new_c1 = after.iter().find(|c| c.number == Some(1.0)).unwrap();
        assert_eq!(old_c1.id, new_c1.id);

        // Download queue lifecycle.
        db.mark_pending(&[new[0].id]).await.unwrap();
        let picked = db.next_pending_download().await.unwrap().unwrap();
        assert_eq!(picked.id, new[0].id);
        db.set_downloading(picked.id).await.unwrap();
        db.finish_download(picked.id, Ok(12)).await.unwrap();
        assert!(db.next_pending_download().await.unwrap().is_none());
        let done = db.get_chapter(picked.id).await.unwrap();
        assert!(matches!(done.download, DownloadState::Downloaded { .. }));
        assert_eq!(done.page_count, Some(12));

        db.delete_manga(manga.id).await.unwrap();
        assert!(matches!(
            db.get_manga(manga.id).await,
            Err(DbError::NotFound)
        ));
        assert_eq!(db.list_chapters(manga.id).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn sync_prunes_chapters_that_left_the_listing() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga(
                "fixture",
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c3", Some(3.0))],
                ),
                false,
            )
            .await
            .unwrap();
        assert_eq!(db.list_chapters(manga.id).await.unwrap().len(), 3);

        // c3 leaves the listing (re-uploaded as c4). Without reconciliation
        // the old row would linger next to its twin — the duplicate bug.
        db.sync_chapters(
            manga.id,
            &details(
                "m1",
                &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c4", Some(3.0))],
            )
            .chapters,
        )
        .await
        .unwrap();
        let keys: Vec<String> = db
            .list_chapters(manga.id)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.source_key)
            .collect();
        assert_eq!(keys, ["c1", "c2", "c4"], "c3 pruned, c4 kept");
    }

    #[tokio::test]
    async fn sync_keeps_downloaded_chapters_and_never_wipes_on_empty() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga(
                "fixture",
                &details("m1", &[("c1", Some(1.0)), ("c2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        // c2 is downloaded — it must survive falling out of the listing
        // (its saved pages would otherwise be orphaned).
        let c2 = db
            .list_chapters(manga.id)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.source_key == "c2")
            .unwrap();
        db.mark_pending(&[c2.id]).await.unwrap();
        db.set_downloading(c2.id).await.unwrap();
        db.finish_download(c2.id, Ok(5)).await.unwrap();

        db.sync_chapters(manga.id, &details("m1", &[("c1", Some(1.0))]).chapters)
            .await
            .unwrap();
        let keys: Vec<String> = db
            .list_chapters(manga.id)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.source_key)
            .collect();
        assert_eq!(
            keys,
            ["c1", "c2"],
            "downloaded c2 kept despite leaving listing"
        );

        // An empty listing must never wipe the library (bad/blocked scrape).
        db.sync_chapters(manga.id, &[]).await.unwrap();
        assert_eq!(
            db.list_chapters(manga.id).await.unwrap().len(),
            2,
            "empty listing left the chapters untouched"
        );
    }

    #[tokio::test]
    async fn reuploaded_series_merges_twins_instead_of_duplicating() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga(
                "fixture",
                &details("m1", &[("old/1", Some(1.0)), ("old/2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        let chapters = db.list_chapters(manga.id).await.unwrap();
        let old1 = chapters.iter().find(|c| c.source_key == "old/1").unwrap();
        let old2 = chapters.iter().find(|c| c.source_key == "old/2").unwrap();

        // old/1 is downloaded and read, old/2 only read: both kinds of user
        // state must survive the re-upload.
        db.mark_pending(&[old1.id]).await.unwrap();
        db.set_downloading(old1.id).await.unwrap();
        db.finish_download(old1.id, Ok(9)).await.unwrap();
        db.mark_read(SHARED, &[old1.id, old2.id]).await.unwrap();
        db.append_event(
            SHARED,
            &ProgressEvent {
                id: Uuid::now_v7(),
                manga_id: manga.id,
                chapter_id: old1.id,
                page: 4,
                device: "test".into(),
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

        // The site re-uploads the whole series under new URLs (same chapter
        // numbers) and adds one genuinely new chapter.
        let sync = db
            .sync_chapters(
                manga.id,
                &details(
                    "m1",
                    &[
                        ("new/1", Some(1.0)),
                        ("new/2", Some(2.0)),
                        ("new/3", Some(3.0)),
                    ],
                )
                .chapters,
            )
            .await
            .unwrap();

        let chapters = db.list_chapters(manga.id).await.unwrap();
        let keys: Vec<&str> = chapters.iter().map(|c| c.source_key.as_str()).collect();
        assert_eq!(keys, ["new/1", "new/2", "new/3"], "old twins merged away");

        // Download carried over to the twin (pages moved on disk by the
        // caller via the Rename op).
        let new1 = chapters.iter().find(|c| c.source_key == "new/1").unwrap();
        let new2 = chapters.iter().find(|c| c.source_key == "new/2").unwrap();
        assert!(
            matches!(new1.download, DownloadState::Downloaded { .. }),
            "old/1's download transferred to new/1"
        );
        assert_eq!(new1.page_count, Some(9));
        assert!(
            sync.file_ops.contains(&ChapterFileOp::Rename {
                from: old1.id,
                to: new1.id
            }),
            "caller told to move old/1's pages: {:?}",
            sync.file_ops
        );

        // Read marks and the reading journal follow the twin.
        let read = db.read_ids(SHARED, manga.id).await.unwrap();
        assert!(read.contains(&new1.id) && read.contains(&new2.id));
        let position = db.latest_position(SHARED, manga.id).await.unwrap().unwrap();
        assert_eq!(position.chapter_id, new1.id);

        // Only the genuinely new chapter is "new" — a re-upload must not
        // re-notify or re-download the whole series.
        let new_keys: Vec<&str> = sync
            .new_chapters
            .iter()
            .map(|c| c.source_key.as_str())
            .collect();
        assert_eq!(new_keys, ["new/3"]);
    }

    #[tokio::test]
    async fn progress_journal_merge_and_idempotency() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let chapter = db.list_chapters(manga.id).await.unwrap().remove(0);

        let event = |id: u128, at: i64, page: u32| ProgressEvent {
            id: Uuid::from_u128(id),
            manga_id: manga.id,
            chapter_id: chapter.id,
            page,
            device: "test".into(),
            at: DateTime::from_timestamp(at, 0).unwrap(),
        };

        let events = [event(1, 100, 3), event(3, 200, 8), event(2, 200, 5)];
        for e in &events {
            db.append_event(SHARED, e).await.unwrap();
        }
        // Replay (offline sync retry) must be a no-op.
        db.append_event(SHARED, &events[0]).await.unwrap();

        let position = db.latest_position(SHARED, manga.id).await.unwrap().unwrap();
        // Same winner as the in-memory merge rule.
        let expected = merge_position(&events).unwrap();
        assert_eq!(position.page, expected.page);
        assert_eq!(position.page, 8);

        // Cursor pages by arrival order, not event id: replaying doesn't
        // move it, later inserts extend it.
        let (all, cursor) = db.events_since(SHARED, None).await.unwrap();
        assert_eq!(all.len(), 3);
        let cursor = cursor.unwrap();
        let (tail, _) = db.events_since(SHARED, Some(cursor)).await.unwrap();
        assert!(tail.is_empty());
        // An old-id event arriving late (offline device reconnects) is
        // still visible past the cursor — the bug an id cursor would have.
        db.append_event(SHARED, &event(0, 50, 1)).await.unwrap();
        let (late, _) = db.events_since(SHARED, Some(cursor)).await.unwrap();
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].id, Uuid::from_u128(0));

        // Unknown manga is a constraint error (client sent garbage).
        let bad = ProgressEvent {
            manga_id: Uuid::from_u128(999),
            ..events[0].clone()
        };
        let bad = ProgressEvent {
            id: Uuid::from_u128(99),
            ..bad
        };
        assert!(matches!(
            db.append_event(SHARED, &bad).await,
            Err(DbError::Constraint(_))
        ));

        // Batch append skips events for deleted manga instead of failing:
        // one stale event must not wedge an offline outbox forever.
        let batch = [bad.clone(), event(4, 300, 9)];
        let (accepted, skipped) = db.append_events(SHARED, &batch).await.unwrap();
        assert_eq!((accepted, skipped), (1, 1));
        let position = db.latest_position(SHARED, manga.id).await.unwrap().unwrap();
        assert_eq!(position.page, 9);
    }

    #[tokio::test]
    async fn users_sessions_and_per_user_positions() {
        let db = Db::in_memory().await.unwrap();

        // The shared account is seeded by the migration.
        let shared = db.user_by_id(SHARED).await.unwrap();
        assert_eq!(shared.username, "everyone");

        // OIDC upsert: created once, refreshed on later logins; a username
        // collision falls back to a subject-suffixed one.
        let alice = db
            .upsert_oidc_user("sub-1", "Alice", "Alice")
            .await
            .unwrap();
        assert_eq!(alice.username, "alice");
        let again = db
            .upsert_oidc_user("sub-1", "Alice", "Alice Renamed")
            .await
            .unwrap();
        assert_eq!(again.id, alice.id);
        assert_eq!(again.display_name, "Alice Renamed");
        let clash = db
            .upsert_oidc_user("sub-2", "alice", "Other Alice")
            .await
            .unwrap();
        assert_ne!(clash.id, alice.id);
        assert_eq!(clash.username, "alice-sub-2");

        // Sessions resolve until deleted or expired.
        db.create_session("h1", alice.id, Utc::now() + chrono::Duration::days(1))
            .await
            .unwrap();
        assert_eq!(db.user_by_session("h1").await.unwrap().id, alice.id);
        db.create_session("h2", alice.id, Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();
        assert!(matches!(
            db.user_by_session("h2").await,
            Err(DbError::NotFound)
        ));
        db.delete_session("h1").await.unwrap();
        assert!(matches!(
            db.user_by_session("h1").await,
            Err(DbError::NotFound)
        ));

        // Positions are per user: Alice's reading doesn't move the shared
        // account's position.
        let manga = db
            .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let chapter = db.list_chapters(manga.id).await.unwrap().remove(0);
        let event = ProgressEvent {
            id: Uuid::from_u128(1),
            manga_id: manga.id,
            chapter_id: chapter.id,
            page: 7,
            device: "test".into(),
            at: Utc::now(),
        };
        db.append_event(alice.id, &event).await.unwrap();
        assert_eq!(
            db.latest_position(alice.id, manga.id)
                .await
                .unwrap()
                .unwrap()
                .page,
            7
        );
        assert!(
            db.latest_position(SHARED, manga.id)
                .await
                .unwrap()
                .is_none()
        );
        let (alice_events, _) = db.events_since(alice.id, None).await.unwrap();
        assert_eq!(alice_events.len(), 1);
        let (shared_events, _) = db.events_since(SHARED, None).await.unwrap();
        assert!(shared_events.is_empty());
    }

    #[tokio::test]
    async fn categories_gate_the_update_sweep() {
        let db = Db::in_memory().await.unwrap();

        // Seeded categories, in display order, with reading the only one
        // checked by the updater.
        let categories = db.list_categories().await.unwrap();
        assert_eq!(
            categories.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["reading", "paused", "finished"]
        );
        assert_eq!(
            categories
                .iter()
                .map(|c| c.update_enabled)
                .collect::<Vec<_>>(),
            [true, false, false]
        );

        let manga = db
            .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        assert_eq!(manga.category, "reading");
        assert_eq!(db.list_manga_for_update().await.unwrap().len(), 1);

        // Finished manga drop out of the sweep; unknown categories refuse.
        let manga = db.set_category(manga.id, "finished").await.unwrap();
        assert_eq!(manga.category, "finished");
        assert!(db.list_manga_for_update().await.unwrap().is_empty());
        assert!(matches!(
            db.set_category(manga.id, "dropped").await,
            Err(DbError::Constraint(_))
        ));

        // Re-enabling updates for a category brings its manga back.
        let finished = db.set_category_update("finished", true).await.unwrap();
        assert!(finished.update_enabled);
        assert_eq!(db.list_manga_for_update().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_marks_are_per_user_and_idempotent() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga(
                "fixture",
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c3", Some(3.0))],
                ),
                false,
            )
            .await
            .unwrap();
        let chapters = db.list_chapters(manga.id).await.unwrap();
        let ids: Vec<Uuid> = chapters.iter().map(|c| c.id).collect();

        assert_eq!(db.mark_read(SHARED, &ids[..2]).await.unwrap(), 2);
        // Re-marking is a no-op, not an error or a double count.
        assert_eq!(db.mark_read(SHARED, &ids[..2]).await.unwrap(), 0);
        let read = db.read_ids(SHARED, manga.id).await.unwrap();
        assert_eq!(read.len(), 2);
        assert!(read.contains(&ids[0]) && read.contains(&ids[1]));

        // Marks are per user.
        let alice = db
            .upsert_oidc_user("sub-1", "alice", "Alice")
            .await
            .unwrap();
        assert!(db.read_ids(alice.id, manga.id).await.unwrap().is_empty());

        assert_eq!(db.mark_unread(SHARED, &ids[..1]).await.unwrap(), 1);
        assert_eq!(db.read_ids(SHARED, manga.id).await.unwrap().len(), 1);

        // Unknown chapters are a constraint error, not a silent skip.
        assert!(matches!(
            db.mark_read(SHARED, &[Uuid::from_u128(42)]).await,
            Err(DbError::Constraint(_))
        ));

        // Marks go with the manga.
        db.delete_manga(manga.id).await.unwrap();
        assert!(db.read_ids(SHARED, manga.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_chapter_keys_in_one_listing_are_deduped() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();

        // The same chapter listed twice (scraped page quirk): one row, one
        // "new chapter", and the sync must not error after commit. c1 is
        // kept in the listing so reconciliation doesn't prune it — this test
        // is about de-duplicating the doubled c2, not about pruning.
        let new = db
            .sync_chapters(
                manga.id,
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c2", Some(2.0))],
                )
                .chapters,
            )
            .await
            .unwrap();
        let new = new.new_chapters;
        assert_eq!(new.len(), 1);
        assert_eq!(db.list_chapters(manga.id).await.unwrap().len(), 2);
    }
}
