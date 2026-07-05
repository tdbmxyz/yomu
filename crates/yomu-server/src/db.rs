//! SQLite persistence. Same conventions as chaos: UUIDs as hyphenated TEXT,
//! timestamps RFC3339, all row↔domain mapping in this module only.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;
use yomu_domain::{
    Chapter, ChapterRef, DownloadState, Manga, MangaDetails, Position, ProgressEvent,
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
    ) -> Result<Vec<Chapter>> {
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
        let mut seen = std::collections::HashSet::new();
        let listing = listing.iter().filter(|c| seen.insert(c.key.as_str()));

        let mut new_ids = Vec::new();
        for chapter in listing {
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
        tx.commit().await?;

        let mut new_chapters = Vec::new();
        for id in new_ids {
            new_chapters.push(self.get_chapter(id).await?);
        }
        Ok(new_chapters)
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

    pub async fn count_chapters(&self, manga_id: Uuid) -> Result<u32> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapters WHERE manga_id = ?")
            .bind(manga_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u32)
    }

    pub async fn mark_pending(&self, chapter_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for id in chapter_ids {
            sqlx::query(
                "UPDATE chapters SET download_state = 'pending', download_error = NULL
                 WHERE id = ? AND download_state IN ('none', 'failed')",
            )
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
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

    pub async fn set_page_count(&self, id: Uuid, page_count: u32) -> Result<()> {
        sqlx::query("UPDATE chapters SET page_count = ? WHERE id = ?")
            .bind(page_count)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- progress journal ----

    /// Append an event. Idempotent on id: replaying a batch is harmless,
    /// which makes offline sync retries safe.
    pub async fn append_event(&self, event: &ProgressEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO progress_events (id, manga_id, chapter_id, page, device, at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(event.id.to_string())
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
    pub async fn append_events(&self, events: &[ProgressEvent]) -> Result<(u32, u32)> {
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
                "INSERT INTO progress_events (id, manga_id, chapter_id, page, device, at)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(event.id.to_string())
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

    /// Merged current position (max at, id tie-break — same rule as
    /// `yomu_domain::merge_position`).
    pub async fn latest_position(&self, manga_id: Uuid) -> Result<Option<Position>> {
        let row = sqlx::query(
            "SELECT chapter_id, page, at FROM progress_events
             WHERE manga_id = ? ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(manga_id.to_string())
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
        since: Option<i64>,
    ) -> Result<(Vec<ProgressEvent>, Option<i64>)> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT * FROM progress_events WHERE seq > ? ORDER BY seq LIMIT 1000",
        )
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
            added_at: row.added_at,
            last_checked_at: row.last_checked_at,
        })
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
        assert_eq!(db.count_chapters(manga.id).await.unwrap(), 2);

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
        assert_eq!(db.count_chapters(manga.id).await.unwrap(), 0);
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
            db.append_event(e).await.unwrap();
        }
        // Replay (offline sync retry) must be a no-op.
        db.append_event(&events[0]).await.unwrap();

        let position = db.latest_position(manga.id).await.unwrap().unwrap();
        // Same winner as the in-memory merge rule.
        let expected = merge_position(&events).unwrap();
        assert_eq!(position.page, expected.page);
        assert_eq!(position.page, 8);

        // Cursor pages by arrival order, not event id: replaying doesn't
        // move it, later inserts extend it.
        let (all, cursor) = db.events_since(None).await.unwrap();
        assert_eq!(all.len(), 3);
        let cursor = cursor.unwrap();
        let (tail, _) = db.events_since(Some(cursor)).await.unwrap();
        assert!(tail.is_empty());
        // An old-id event arriving late (offline device reconnects) is
        // still visible past the cursor — the bug an id cursor would have.
        db.append_event(&event(0, 50, 1)).await.unwrap();
        let (late, _) = db.events_since(Some(cursor)).await.unwrap();
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
            db.append_event(&bad).await,
            Err(DbError::Constraint(_))
        ));

        // Batch append skips events for deleted manga instead of failing:
        // one stale event must not wedge an offline outbox forever.
        let batch = [bad.clone(), event(4, 300, 9)];
        let (accepted, skipped) = db.append_events(&batch).await.unwrap();
        assert_eq!((accepted, skipped), (1, 1));
        let position = db.latest_position(manga.id).await.unwrap().unwrap();
        assert_eq!(position.page, 9);
    }

    #[tokio::test]
    async fn duplicate_chapter_keys_in_one_listing_are_deduped() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();

        // The same chapter listed twice (scraped page quirk): one row, one
        // "new chapter", and the sync must not error after commit.
        let new = db
            .sync_chapters(
                manga.id,
                &details("m1", &[("c2", Some(2.0)), ("c2", Some(2.0))]).chapters,
            )
            .await
            .unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(db.count_chapters(manga.id).await.unwrap(), 2);
    }
}
