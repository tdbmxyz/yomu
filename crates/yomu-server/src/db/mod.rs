//! SQLite persistence. Same conventions as chaos: UUIDs as hyphenated TEXT,
//! timestamps RFC3339, all row↔domain mapping in this module only.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use uuid::Uuid;
use yomu_domain::{
    Category, ChapterRef, DownloadState, Kind, Origin, ProgressEvent, Publication, ReadingUnit,
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

/// What a unit sync did, beyond upserting rows. `file_ops` are the
/// filesystem follow-ups the caller must apply: this module only owns rows,
/// the downloaded pages live under `data_dir/<publication>/<unit>/` (see
/// `AppState::unit_dir`).
pub struct UnitSync {
    /// Units that weren't known before, in listing order. Twins that
    /// merely replaced a re-uploaded unit the user already had are not
    /// included (they must not re-notify or re-trigger auto-download).
    pub new_units: Vec<ReadingUnit>,
    pub file_ops: Vec<UnitFileOp>,
}

/// Per-publication unit aggregates for the library list (see
/// `library_rollups`).
#[derive(Debug, Clone, Default)]
pub struct LibraryRollup {
    pub unit_count: u32,
    pub downloaded_count: u32,
    pub unread_count: u32,
    pub latest_unit_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitFileOp {
    /// The unit row was deleted; drop its page directory.
    Remove { unit: Uuid },
    /// A downloaded unit was merged into its re-uploaded twin; move the
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
        // Recover from a crash mid-download: those units are re-queued.
        sqlx::query(
            "UPDATE reading_units SET download_state = 'pending' WHERE download_state = 'downloading'",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }
}

mod backup;
mod catalog;
mod categories;
mod downloads;
mod progress;
mod publications;
mod read_marks;
mod units;
mod updates;
mod users;

async fn insert_units(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    publication_id: Uuid,
    chapters: &[ChapterRef],
    now: DateTime<Utc>,
) -> Result<()> {
    for chapter in chapters {
        sqlx::query(
            "INSERT INTO reading_units (id, publication_id, source_key, title, number, source_order,
                                   scanlator, fetched_at, published_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (publication_id, source_key) DO NOTHING",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(publication_id.to_string())
        .bind(&chapter.key)
        .bind(&chapter.title)
        .bind(chapter.number)
        .bind(chapter.source_order)
        .bind(&chapter.scanlator)
        .bind(now)
        .bind(chapter.published_at)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Replace a publication's genre rows within a transaction.
async fn write_genres(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    publication_id: Uuid,
    genres: &[String],
) -> Result<()> {
    sqlx::query("DELETE FROM publication_genres WHERE publication_id = ?")
        .bind(publication_id.to_string())
        .execute(&mut **tx)
        .await?;
    for genre in genres {
        sqlx::query(
            "INSERT INTO publication_genres (publication_id, genre) VALUES (?, ?)
             ON CONFLICT (publication_id, genre) DO NOTHING",
        )
        .bind(publication_id.to_string())
        .bind(genre)
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
struct PublicationRow {
    id: String,
    kind: String,
    source_id: Option<String>,
    source_key: Option<String>,
    file_path: Option<String>,
    title: String,
    description: Option<String>,
    cover_url: Option<String>,
    auto_download: bool,
    category: String,
    added_at: DateTime<Utc>,
    last_checked_at: Option<DateTime<Utc>>,
    missing_since: Option<DateTime<Utc>>,
}

impl TryFrom<PublicationRow> for Publication {
    type Error = DbError;

    fn try_from(row: PublicationRow) -> Result<Self> {
        let kind = match row.kind.as_str() {
            "comics" => Kind::Comics,
            "novels" => Kind::Novels,
            "pdf" => Kind::Pdf,
            other => return Err(DbError::Corrupt(format!("kind {other:?}"))),
        };
        let origin = match (row.source_id, row.source_key, row.file_path) {
            (Some(source_id), Some(source_key), None) => Origin::Source {
                source_id,
                source_key,
            },
            (None, None, Some(path)) => Origin::LocalFile { path },
            _ => return Err(DbError::Corrupt(format!("publication {} origin", row.id))),
        };
        Ok(Publication {
            id: parse_uuid(row.id)?,
            kind,
            origin,
            title: row.title,
            description: row.description,
            cover_url: parse_url_opt(row.cover_url)?,
            auto_download: row.auto_download,
            category: row.category,
            // Genres live in publication_genres; accessors attach them.
            genres: Vec::new(),
            added_at: row.added_at,
            last_checked_at: row.last_checked_at,
            missing_since: row.missing_since,
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
struct UnitRow {
    id: String,
    publication_id: String,
    source_key: String,
    title: String,
    number: Option<f64>,
    source_order: i64,
    scanlator: Option<String>,
    fetched_at: DateTime<Utc>,
    published_at: Option<DateTime<Utc>>,
    download_state: String,
    downloaded_at: Option<DateTime<Utc>>,
    download_error: Option<String>,
    page_count: Option<i64>,
}

impl TryFrom<UnitRow> for ReadingUnit {
    type Error = DbError;

    fn try_from(row: UnitRow) -> Result<Self> {
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
        Ok(ReadingUnit {
            id: parse_uuid(row.id)?,
            publication_id: parse_uuid(row.publication_id)?,
            source_key: row.source_key,
            title: row.title,
            number: row.number,
            source_order: row.source_order as u32,
            scanlator: row.scanlator,
            fetched_at: row.fetched_at,
            published_at: row.published_at,
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
    publication_id: String,
    unit_id: String,
    page: i64,
    device: String,
    at: DateTime<Utc>,
}

impl TryFrom<EventRow> for ProgressEvent {
    type Error = DbError;

    fn try_from(row: EventRow) -> Result<Self> {
        Ok(ProgressEvent {
            id: parse_uuid(row.id)?,
            publication_id: parse_uuid(row.publication_id)?,
            unit_id: parse_uuid(row.unit_id)?,
            page: row.page as u32,
            device: row.device,
            at: row.at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yomu_domain::{MangaDetails, MangaSummary, merge_position};

    /// The seeded single-account user (see migration 0004).
    const SHARED: Uuid = Uuid::nil();

    fn details(key: &str, chapters: &[(&str, Option<f64>)]) -> MangaDetails {
        MangaDetails {
            summary: MangaSummary {
                key: key.into(),
                title: format!("Publication {key}"),
                cover_url: None,
                in_library: None,
            },
            description: Some("desc".into()),
            genres: Vec::new(),
            chapters: chapters
                .iter()
                .enumerate()
                .map(|(i, (ck, number))| ChapterRef {
                    key: (*ck).into(),
                    title: format!("Chapter {ck}"),
                    number: *number,
                    source_order: i as u32,
                    scanlator: None,
                    published_at: None,
                })
                .collect(),
        }
    }

    fn details_with_genres(
        key: &str,
        chapters: &[(&str, Option<f64>)],
        genres: &[&str],
    ) -> MangaDetails {
        MangaDetails {
            genres: genres.iter().map(|g| g.to_string()).collect(),
            ..details(key, chapters)
        }
    }

    #[tokio::test]
    async fn remove_downloads_resets_only_downloaded_rows() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        db.mark_pending(&[units[0].id]).await.unwrap();
        db.finish_download(units[0].id, Ok(9)).await.unwrap();

        let removed = db
            .remove_downloads(&[units[0].id, units[1].id])
            .await
            .unwrap();
        assert_eq!(removed, vec![units[0].id]); // the 'none' row is skipped

        let after = db.list_units(publication.id).await.unwrap();
        assert!(matches!(after[0].download, DownloadState::None));
        // page_count survives: still true knowledge about the chapter
        assert_eq!(after[0].page_count, Some(9));
    }

    #[tokio::test]
    async fn library_rollups_and_positions_are_batched_per_manga() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                false,
            )
            .await
            .unwrap();
        // list_units order: number asc → c1 then c2.
        let units = db.list_units(publication.id).await.unwrap();
        db.mark_pending(&[units[0].id]).await.unwrap();
        db.finish_download(units[0].id, Ok(9)).await.unwrap();
        db.mark_read(SHARED, &[units[0].id]).await.unwrap();
        db.append_event(
            SHARED,
            &ProgressEvent {
                id: Uuid::from_u128(1),
                publication_id: publication.id,
                unit_id: units[1].id,
                page: 3,
                device: "test".into(),
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

        let rollups = db.library_rollups(&SHARED.to_string()).await.unwrap();
        let rollup = rollups.get(&publication.id).unwrap();
        assert_eq!(rollup.unit_count, 2);
        assert_eq!(rollup.downloaded_count, 1);
        assert_eq!(rollup.unread_count, 1); // one of two marked read
        assert!(rollup.latest_unit_at.is_some());

        let positions = db.latest_positions(SHARED).await.unwrap();
        let (position, title) = positions.get(&publication.id).unwrap();
        assert_eq!(position.unit_id, units[1].id);
        assert_eq!(position.page(), 3);
        assert_eq!(title.as_deref(), Some(units[1].title.as_str()));

        // Signed-out scope (no matching user) counts nothing as read.
        let anon = db.library_rollups("").await.unwrap();
        assert_eq!(anon.get(&publication.id).unwrap().unread_count, 2);
    }

    #[tokio::test]
    async fn backup_round_trips_into_a_fresh_instance() {
        use yomu_domain::Backup;

        let source = Db::in_memory().await.unwrap();
        let publication = source
            .insert_publication(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                true,
            )
            .await
            .unwrap();
        let units = source.list_units(publication.id).await.unwrap();
        source.mark_read(SHARED, &[units[0].id]).await.unwrap();
        source
            .append_event(
                SHARED,
                &ProgressEvent {
                    id: Uuid::from_u128(7),
                    publication_id: publication.id,
                    unit_id: units[1].id,
                    page: 5,
                    device: "test".into(),
                    at: Utc::now(),
                },
            )
            .await
            .unwrap();

        let backup = Backup {
            version: yomu_domain::BACKUP_VERSION,
            exported_at: Utc::now(),
            categories: source.list_categories().await.unwrap(),
            publications: source.list_publications().await.unwrap(),
            units: source.export_units().await.unwrap(),
            read_unit_ids: source.read_all_ids(SHARED).await.unwrap(),
            progress: source.export_events(SHARED).await.unwrap(),
        };

        let target = Db::in_memory().await.unwrap();
        let summary = target.import_backup(SHARED, &backup).await.unwrap();
        assert_eq!(summary.publications, 1);
        assert_eq!(summary.units, 2);
        assert_eq!(summary.read_marks, 1);
        assert_eq!(summary.progress_events, 1);

        // The restored instance mirrors the source's library and reading state.
        let restored = target.list_publications().await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, publication.id);
        assert!(restored[0].auto_download);
        let read = target.read_ids(SHARED, publication.id).await.unwrap();
        assert!(read.contains(&units[0].id));
        let position = target
            .latest_position(SHARED, publication.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(position.unit_id, units[1].id);
        assert_eq!(position.page(), 5);
        // Restored units read live (no page files travelled with the backup).
        let restored_units = target.list_units(publication.id).await.unwrap();
        assert!(
            restored_units
                .iter()
                .all(|c| matches!(c.download, DownloadState::None))
        );

        // Re-importing is idempotent: nothing new lands the second time.
        let again = target.import_backup(SHARED, &backup).await.unwrap();
        assert_eq!(
            (
                again.publications,
                again.units,
                again.read_marks,
                again.progress_events
            ),
            (0, 0, 0, 0)
        );
    }

    /// A backup exported by a 1.x server (literal JSON, incl. a local-source
    /// manga) must restore into the 2.0 schema with origins converted and the
    /// user's reading state intact.
    #[tokio::test]
    async fn restore_accepts_a_1x_backup_file() {
        let json = r#"{
            "version": 1,
            "exported_at": "2026-01-01T00:00:00Z",
            "categories": [
                {"id":"reading","name":"Reading","position":0,"update_enabled":true}
            ],
            "manga": [
                {"id":"00000000-0000-0000-0000-00000000000a","source_id":"fixture",
                 "source_key":"m1","title":"Scraped","auto_download":false,
                 "category":"reading","added_at":"2026-01-01T00:00:00Z"},
                {"id":"00000000-0000-0000-0000-00000000000b","source_id":"local",
                 "source_key":"Solo Farming","title":"Solo Farming","auto_download":false,
                 "category":"reading","added_at":"2026-01-01T00:00:00Z"}
            ],
            "chapters": [
                {"id":"00000000-0000-0000-0000-0000000000a1",
                 "manga_id":"00000000-0000-0000-0000-00000000000a","source_key":"c1",
                 "title":"Chapter 1","source_order":0,
                 "fetched_at":"2026-01-01T00:00:00Z","download":{"state":"none"},"read":false},
                {"id":"00000000-0000-0000-0000-0000000000b1",
                 "manga_id":"00000000-0000-0000-0000-00000000000b",
                 "source_key":"Solo Farming/Chapter 1","title":"Chapter 1","source_order":0,
                 "fetched_at":"2026-01-01T00:00:00Z","download":{"state":"none"},"read":false}
            ],
            "read_chapter_ids": ["00000000-0000-0000-0000-0000000000a1"],
            "progress": [
                {"id":"00000000-0000-0000-0000-0000000000e1",
                 "manga_id":"00000000-0000-0000-0000-00000000000b",
                 "chapter_id":"00000000-0000-0000-0000-0000000000b1",
                 "page":4,"device":"phone","at":"2026-01-02T00:00:00Z"}
            ]
        }"#;
        let backup: yomu_domain::Backup = serde_json::from_str(json).unwrap();

        let db = Db::in_memory().await.unwrap();
        let summary = db.import_backup(SHARED, &backup).await.unwrap();
        assert_eq!(
            (
                summary.publications,
                summary.units,
                summary.read_marks,
                summary.progress_events
            ),
            (2, 2, 1, 1)
        );

        let scraped = db
            .get_publication(Uuid::parse_str("00000000-0000-0000-0000-00000000000a").unwrap())
            .await
            .unwrap();
        assert_eq!(
            scraped.origin,
            Origin::Source {
                source_id: "fixture".into(),
                source_key: "m1".into(),
            }
        );
        let local = db
            .get_publication(Uuid::parse_str("00000000-0000-0000-0000-00000000000b").unwrap())
            .await
            .unwrap();
        assert_eq!(
            local.origin,
            Origin::LocalFile {
                path: "Solo Farming".into()
            }
        );
        let position = db.latest_position(SHARED, local.id).await.unwrap().unwrap();
        assert_eq!(position.page(), 4);
    }

    /// The kind column must round-trip through a backup, not be forced to
    /// comics on import — a later-slice novel restored from backup stays one.
    #[tokio::test]
    async fn restore_preserves_publication_kind() {
        let backup = yomu_domain::Backup {
            version: yomu_domain::BACKUP_VERSION,
            exported_at: Utc::now(),
            categories: Vec::new(),
            publications: vec![Publication {
                id: Uuid::from_u128(0xF0),
                kind: Kind::Novels,
                origin: Origin::LocalFile {
                    path: "A Novel".into(),
                },
                title: "A Novel".into(),
                description: None,
                cover_url: None,
                auto_download: false,
                category: "reading".into(),
                genres: Vec::new(),
                added_at: Utc::now(),
                last_checked_at: None,
                missing_since: None,
            }],
            units: Vec::new(),
            read_unit_ids: Vec::new(),
            progress: Vec::new(),
        };

        let db = Db::in_memory().await.unwrap();
        let summary = db.import_backup(SHARED, &backup).await.unwrap();
        assert_eq!(summary.publications, 1);
        let restored = db.get_publication(Uuid::from_u128(0xF0)).await.unwrap();
        assert_eq!(restored.kind, Kind::Novels);
    }

    #[tokio::test]
    async fn genres_are_stored_and_batched() {
        let db = Db::in_memory().await.unwrap();
        let a = db
            .insert_publication(
                "fixture",
                &details_with_genres("m1", &[("c1", Some(1.0))], &["Action", "Fantasy"]),
                false,
            )
            .await
            .unwrap();
        let b = db
            .insert_publication(
                "fixture",
                &details_with_genres("m2", &[("c1", Some(1.0))], &["Fantasy", "Romance"]),
                false,
            )
            .await
            .unwrap();

        // insert_publication returns the genres it wrote; get_publication reloads them.
        assert_eq!(a.genres, vec!["Action", "Fantasy"]);
        assert_eq!(
            db.get_publication(a.id).await.unwrap().genres,
            vec!["Action", "Fantasy"]
        );
        // list_publications attaches genres per row from one grouped query.
        let listed = db.list_publications().await.unwrap();
        let listed_b = listed.iter().find(|m| m.id == b.id).unwrap();
        assert_eq!(listed_b.genres, vec!["Fantasy", "Romance"]);

        // set_genres is replace-all, not additive.
        db.set_genres(a.id, &["Drama".into()]).await.unwrap();
        assert_eq!(db.genres_for(a.id).await.unwrap(), vec!["Drama"]);

        let map = db.genres_by_publication().await.unwrap();
        assert_eq!(map.get(&a.id).unwrap(), &vec!["Drama".to_string()]);
        assert_eq!(
            map.get(&b.id).unwrap(),
            &vec!["Fantasy".to_string(), "Romance".into()]
        );
    }

    #[tokio::test]
    async fn download_queue_lists_and_transitions_states() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details(
                    "m1",
                    &[("c3", Some(3.0)), ("c2", Some(2.0)), ("c1", Some(1.0))],
                ),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        let (pending, downloaded, failed) = (units[0].id, units[1].id, units[2].id);

        db.mark_pending(&[pending]).await.unwrap();
        db.mark_pending(&[downloaded]).await.unwrap();
        db.finish_download(downloaded, Ok(5)).await.unwrap();
        db.mark_pending(&[failed]).await.unwrap();
        db.finish_download(failed, Err("boom".into()))
            .await
            .unwrap();

        // Queue holds pending + failed but not the downloaded chapter.
        let queue = db.download_queue().await.unwrap();
        let ids: Vec<_> = queue.iter().map(|c| c.id).collect();
        assert!(ids.contains(&pending) && ids.contains(&failed));
        assert!(!ids.contains(&downloaded));

        // Server summary counts the one downloaded chapter and its pages.
        assert_eq!(db.downloaded_summary().await.unwrap(), (1, 5));

        // Titles come back for labelling.
        let titles = db.publication_titles(&[publication.id]).await.unwrap();
        assert_eq!(titles.get(&publication.id).unwrap(), &publication.title);

        // dismiss drops pending|failed → none, not downloaded.
        assert_eq!(
            db.dismiss_downloads(&[pending, downloaded]).await.unwrap(),
            1
        );
        assert!(
            !db.download_queue()
                .await
                .unwrap()
                .iter()
                .any(|c| c.id == pending)
        );

        // retry_failed re-queues only failed rows.
        assert_eq!(db.retry_failed(&[failed, downloaded]).await.unwrap(), 1);
        let after = db.list_units(publication.id).await.unwrap();
        let failed_row = after.iter().find(|c| c.id == failed).unwrap();
        assert!(matches!(failed_row.download, DownloadState::Pending));
    }

    #[tokio::test]
    async fn library_keys_maps_source_key_to_id() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let map = db.library_keys("fixture").await.unwrap();
        assert_eq!(map.get("m1"), Some(&publication.id));
        assert!(db.library_keys("other-source").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn catalog_upsert_and_page_roundtrip() {
        let db = Db::in_memory().await.unwrap();
        let sum = |k: &str, t: &str| MangaSummary {
            key: k.into(),
            title: t.into(),
            cover_url: Some(format!("https://c.example/{k}.jpg").parse().unwrap()),
            in_library: None,
        };
        let now = Utc::now();
        db.upsert_catalog_entries("src", &[sum("a", "A"), sum("b", "B")], now)
            .await
            .unwrap();
        // A later sighting updates what changed (title here) in place.
        db.upsert_catalog_entries("src", &[sum("a", "A2")], now)
            .await
            .unwrap();
        db.write_catalog_page("src", "popular", 1, &["a".into(), "b".into()], now)
            .await
            .unwrap();
        let (items, fetched_at) = db
            .read_catalog_page("src", "popular", 1)
            .await
            .unwrap()
            .expect("cached page");
        assert_eq!(fetched_at, now);
        assert_eq!(
            items.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["A2", "B"],
        );
        // Unknown page → None.
        assert!(
            db.read_catalog_page("src", "latest", 1)
                .await
                .unwrap()
                .is_none()
        );
        // Cover ownership lookup for the proxy: known URL yields its
        // source, anything else stays unproxied.
        assert_eq!(
            db.catalog_source_for_cover("https://c.example/a.jpg")
                .await
                .unwrap(),
            Some("src".to_string()),
        );
        assert_eq!(
            db.catalog_source_for_cover("https://evil.example/x")
                .await
                .unwrap(),
            None,
        );
    }

    #[tokio::test]
    async fn published_at_backfills_and_never_clears() {
        use chrono::TimeZone;
        let day = |d: u32| Utc.with_ymd_and_hms(2026, 7, d, 0, 0, 0).unwrap();

        let db = Db::in_memory().await.unwrap();
        // 1. First sync without dates → rows have NULL published_at.
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                false,
            )
            .await
            .unwrap();
        assert!(
            db.list_units(publication.id)
                .await
                .unwrap()
                .iter()
                .all(|c| c.published_at.is_none())
        );

        // 2. Source starts printing dates → the same keys re-synced with
        //    Some(..) backfill the existing rows.
        let mut listing = details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]).chapters;
        listing[0].published_at = Some(day(2));
        listing[1].published_at = Some(day(1));
        db.sync_units(publication.id, &listing).await.unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        assert_eq!(units.iter().filter(|c| c.published_at.is_some()).count(), 2);

        // 3. Source stops printing dates → None must NOT clear stored values.
        listing[0].published_at = None;
        listing[1].published_at = None;
        db.sync_units(publication.id, &listing).await.unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        assert_eq!(units.iter().filter(|c| c.published_at.is_some()).count(), 2);

        // 4. A changed date wins (site-side correction).
        listing[1].published_at = Some(day(5));
        db.sync_units(publication.id, &listing).await.unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        let c1 = units.iter().find(|c| c.source_key == "c1").unwrap();
        assert_eq!(c1.published_at, Some(day(5)));
    }

    #[tokio::test]
    async fn library_lifecycle_and_chapter_sync() {
        let db = Db::in_memory().await.unwrap();

        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]),
                false,
            )
            .await
            .unwrap();
        assert_eq!(db.list_units(publication.id).await.unwrap().len(), 2);

        // Duplicate add is a constraint error, not a second row.
        assert!(matches!(
            db.insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
                .await,
            Err(DbError::Constraint(_))
        ));

        // Re-sync with one new chapter: only the new one is returned, the
        // existing ones keep their ids.
        let before = db.list_units(publication.id).await.unwrap();
        let new = db
            .sync_units(
                publication.id,
                &details(
                    "m1",
                    &[("c3", Some(3.0)), ("c2", Some(2.0)), ("c1", Some(1.0))],
                )
                .chapters,
            )
            .await
            .unwrap();
        let new = new.new_units;
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].number, Some(3.0));
        let after = db.list_units(publication.id).await.unwrap();
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
        let done = db.get_unit(picked.id).await.unwrap();
        assert!(matches!(done.download, DownloadState::Downloaded { .. }));
        assert_eq!(done.page_count, Some(12));

        db.delete_publication(publication.id).await.unwrap();
        assert!(matches!(
            db.get_publication(publication.id).await,
            Err(DbError::NotFound)
        ));
        assert_eq!(db.list_units(publication.id).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn sync_prunes_chapters_that_left_the_listing() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c3", Some(3.0))],
                ),
                false,
            )
            .await
            .unwrap();
        assert_eq!(db.list_units(publication.id).await.unwrap().len(), 3);

        // c3 leaves the listing (re-uploaded as c4). Without reconciliation
        // the old row would linger next to its twin — the duplicate bug.
        db.sync_units(
            publication.id,
            &details(
                "m1",
                &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c4", Some(3.0))],
            )
            .chapters,
        )
        .await
        .unwrap();
        let keys: Vec<String> = db
            .list_units(publication.id)
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
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c1", Some(1.0)), ("c2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        // c2 is downloaded — it must survive falling out of the listing
        // (its saved pages would otherwise be orphaned).
        let c2 = db
            .list_units(publication.id)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.source_key == "c2")
            .unwrap();
        db.mark_pending(&[c2.id]).await.unwrap();
        db.set_downloading(c2.id).await.unwrap();
        db.finish_download(c2.id, Ok(5)).await.unwrap();

        db.sync_units(
            publication.id,
            &details("m1", &[("c1", Some(1.0))]).chapters,
        )
        .await
        .unwrap();
        let keys: Vec<String> = db
            .list_units(publication.id)
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
        db.sync_units(publication.id, &[]).await.unwrap();
        assert_eq!(
            db.list_units(publication.id).await.unwrap().len(),
            2,
            "empty listing left the chapters untouched"
        );
    }

    #[tokio::test]
    async fn reuploaded_series_merges_twins_instead_of_duplicating() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("old/1", Some(1.0)), ("old/2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        let old1 = units.iter().find(|c| c.source_key == "old/1").unwrap();
        let old2 = units.iter().find(|c| c.source_key == "old/2").unwrap();

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
                publication_id: publication.id,
                unit_id: old1.id,
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
            .sync_units(
                publication.id,
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

        let units = db.list_units(publication.id).await.unwrap();
        let keys: Vec<&str> = units.iter().map(|c| c.source_key.as_str()).collect();
        assert_eq!(keys, ["new/1", "new/2", "new/3"], "old twins merged away");

        // Download carried over to the twin (pages moved on disk by the
        // caller via the Rename op).
        let new1 = units.iter().find(|c| c.source_key == "new/1").unwrap();
        let new2 = units.iter().find(|c| c.source_key == "new/2").unwrap();
        assert!(
            matches!(new1.download, DownloadState::Downloaded { .. }),
            "old/1's download transferred to new/1"
        );
        assert_eq!(new1.page_count, Some(9));
        assert!(
            sync.file_ops.contains(&UnitFileOp::Rename {
                from: old1.id,
                to: new1.id
            }),
            "caller told to move old/1's pages: {:?}",
            sync.file_ops
        );

        // Read marks and the reading journal follow the twin.
        let read = db.read_ids(SHARED, publication.id).await.unwrap();
        assert!(read.contains(&new1.id) && read.contains(&new2.id));
        let position = db
            .latest_position(SHARED, publication.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(position.unit_id, new1.id);

        // Only the genuinely new chapter is "new" — a re-upload must not
        // re-notify or re-download the whole series.
        let new_keys: Vec<&str> = sync
            .new_units
            .iter()
            .map(|c| c.source_key.as_str())
            .collect();
        assert_eq!(new_keys, ["new/3"]);
    }

    #[tokio::test]
    async fn progress_journal_merge_and_idempotency() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let chapter = db.list_units(publication.id).await.unwrap().remove(0);

        let event = |id: u128, at: i64, page: u32| ProgressEvent {
            id: Uuid::from_u128(id),
            publication_id: publication.id,
            unit_id: chapter.id,
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

        let position = db
            .latest_position(SHARED, publication.id)
            .await
            .unwrap()
            .unwrap();
        // Same winner as the in-memory merge rule.
        let expected = merge_position(&events).unwrap();
        assert_eq!(position.page(), expected.page);
        assert_eq!(position.page(), 8);

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
            publication_id: Uuid::from_u128(999),
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
        let position = db
            .latest_position(SHARED, publication.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(position.page(), 9);
    }

    #[tokio::test]
    async fn batch_append_skips_events_with_unknown_chapter() {
        // A known manga but a garbage unit_id (client desync) must be
        // skipped, not stored — otherwise latest_position points at a
        // chapter that resolves to nothing.
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let real = db.list_units(publication.id).await.unwrap().remove(0);

        let good = ProgressEvent {
            id: Uuid::from_u128(1),
            publication_id: publication.id,
            unit_id: real.id,
            page: 4,
            device: "test".into(),
            at: DateTime::from_timestamp(100, 0).unwrap(),
        };
        let dangling = ProgressEvent {
            id: Uuid::from_u128(2),
            unit_id: Uuid::from_u128(9999),
            at: DateTime::from_timestamp(200, 0).unwrap(),
            ..good.clone()
        };

        let (accepted, skipped) = db.append_events(SHARED, &[good, dangling]).await.unwrap();
        assert_eq!((accepted, skipped), (1, 1));
        // The surviving position is the good event's chapter, not the
        // later-dated dangling one.
        let position = db
            .latest_position(SHARED, publication.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(position.unit_id, real.id);
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
        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        let chapter = db.list_units(publication.id).await.unwrap().remove(0);
        let event = ProgressEvent {
            id: Uuid::from_u128(1),
            publication_id: publication.id,
            unit_id: chapter.id,
            page: 7,
            device: "test".into(),
            at: Utc::now(),
        };
        db.append_event(alice.id, &event).await.unwrap();
        assert_eq!(
            db.latest_position(alice.id, publication.id)
                .await
                .unwrap()
                .unwrap()
                .page(),
            7
        );
        assert!(
            db.latest_position(SHARED, publication.id)
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

        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();
        assert_eq!(publication.category, "reading");
        assert_eq!(db.list_publications_for_update().await.unwrap().len(), 1);

        // Finished manga drop out of the sweep; unknown categories refuse.
        let publication = db.set_category(publication.id, "finished").await.unwrap();
        assert_eq!(publication.category, "finished");
        assert!(db.list_publications_for_update().await.unwrap().is_empty());
        assert!(matches!(
            db.set_category(publication.id, "dropped").await,
            Err(DbError::Constraint(_))
        ));

        // Re-enabling updates for a category brings its manga back.
        let finished = db.set_category_update("finished", true).await.unwrap();
        assert!(finished.update_enabled);
        assert_eq!(db.list_publications_for_update().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_marks_are_per_user_and_idempotent() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c3", Some(3.0))],
                ),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        let ids: Vec<Uuid> = units.iter().map(|c| c.id).collect();

        assert_eq!(db.mark_read(SHARED, &ids[..2]).await.unwrap(), 2);
        // Re-marking is a no-op, not an error or a double count.
        assert_eq!(db.mark_read(SHARED, &ids[..2]).await.unwrap(), 0);
        let read = db.read_ids(SHARED, publication.id).await.unwrap();
        assert_eq!(read.len(), 2);
        assert!(read.contains(&ids[0]) && read.contains(&ids[1]));

        // Marks are per user.
        let alice = db
            .upsert_oidc_user("sub-1", "alice", "Alice")
            .await
            .unwrap();
        assert!(
            db.read_ids(alice.id, publication.id)
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(db.mark_unread(SHARED, &ids[..1]).await.unwrap(), 1);
        assert_eq!(db.read_ids(SHARED, publication.id).await.unwrap().len(), 1);

        // Unknown chapters are a constraint error, not a silent skip.
        assert!(matches!(
            db.mark_read(SHARED, &[Uuid::from_u128(42)]).await,
            Err(DbError::Constraint(_))
        ));

        // Marks go with the manga.
        db.delete_publication(publication.id).await.unwrap();
        assert!(
            db.read_ids(SHARED, publication.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn duplicate_chapter_keys_in_one_listing_are_deduped() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();

        // The same chapter listed twice (scraped page quirk): one row, one
        // "new chapter", and the sync must not error after commit. c1 is
        // kept in the listing so reconciliation doesn't prune it — this test
        // is about de-duplicating the doubled c2, not about pruning.
        let new = db
            .sync_units(
                publication.id,
                &details(
                    "m1",
                    &[("c1", Some(1.0)), ("c2", Some(2.0)), ("c2", Some(2.0))],
                )
                .chapters,
            )
            .await
            .unwrap();
        let new = new.new_units;
        assert_eq!(new.len(), 1);
        assert_eq!(db.list_units(publication.id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn updates_feed_records_filters_and_prunes() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("c1", Some(1.0)), ("c2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();

        // Empty find: no row.
        db.add_update(publication.id, &[]).await.unwrap();
        let all = db
            .updates_since(DateTime::<Utc>::MIN_UTC, 100)
            .await
            .unwrap();
        assert!(all.is_empty());

        db.add_update(publication.id, &units).await.unwrap();
        let all = db
            .updates_since(DateTime::<Utc>::MIN_UTC, 100)
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].publication_id, publication.id);
        assert_eq!(all[0].publication_title, "Publication m1");
        assert_eq!(all[0].unit_count, 2);
        assert_eq!(all[0].first_title, "Chapter c1");
        assert_eq!(all[0].last_title, "Chapter c2");

        // A watermark at/after the event hides it; strictly-newer filter.
        let seen = all[0].created_at;
        assert!(db.updates_since(seen, 100).await.unwrap().is_empty());

        // Cap.
        db.add_update(publication.id, &units[..1]).await.unwrap();
        let capped = db.updates_since(DateTime::<Utc>::MIN_UTC, 1).await.unwrap();
        assert_eq!(capped.len(), 1);

        // Prune everything older than the far future.
        db.prune_updates(Utc::now() + chrono::Duration::days(1))
            .await
            .unwrap();
        assert!(
            db.updates_since(DateTime::<Utc>::MIN_UTC, 100)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn next_pending_download_is_lowest_number_first() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details(
                    "m1",
                    &[("c3", Some(3.0)), ("c1", Some(1.0)), ("c2", Some(2.0))],
                ),
                false,
            )
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        let ids: Vec<_> = units.iter().map(|c| c.id).collect();
        db.mark_pending(&ids).await.unwrap();
        let next = db.next_pending_download().await.unwrap().unwrap();
        assert_eq!(next.number, Some(1.0));
    }

    #[tokio::test]
    async fn local_publications_lifecycle() {
        use yomu_domain::Origin;

        let db = Db::in_memory().await.unwrap();
        // A scraped publication must never show up in the local listing.
        db.insert_publication("fixture", &details("m1", &[("c1", Some(1.0))]), false)
            .await
            .unwrap();

        let local = db
            .insert_local_publication("Solo Farming", &details("Solo Farming", &[("c1", None)]))
            .await
            .unwrap();
        assert_eq!(
            local.origin,
            Origin::LocalFile {
                path: "Solo Farming".into()
            }
        );
        assert!(local.missing_since.is_none());
        assert_eq!(db.list_units(local.id).await.unwrap().len(), 1);

        // Duplicate path is a constraint error, not a second row.
        assert!(matches!(
            db.insert_local_publication("Solo Farming", &details("Solo Farming", &[]))
                .await,
            Err(DbError::Constraint(_))
        ));

        let listed = db.list_local_publications().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, local.id);
        // ... and the updater sweep skips it (nothing to scrape).
        assert_eq!(db.list_publications_for_update().await.unwrap().len(), 1);
        assert!(matches!(
            &db.list_publications_for_update().await.unwrap()[0].origin,
            Origin::Source { .. }
        ));

        // Vanished file: flagged, then healed by a repoint to the new path.
        let at = Utc::now();
        db.set_missing_since(local.id, Some(at)).await.unwrap();
        assert!(
            db.get_publication(local.id)
                .await
                .unwrap()
                .missing_since
                .is_some()
        );
        db.repoint_local_publication(local.id, "Solo Farming v2")
            .await
            .unwrap();
        let healed = db.get_publication(local.id).await.unwrap();
        assert_eq!(
            healed.origin,
            Origin::LocalFile {
                path: "Solo Farming v2".into()
            }
        );
        assert!(healed.missing_since.is_none());

        // Metadata refresh touches cover/description, never the title.
        db.update_local_metadata(local.id, Some("new desc"), None)
            .await
            .unwrap();
        let updated = db.get_publication(local.id).await.unwrap();
        assert_eq!(updated.description.as_deref(), Some("new desc"));
        assert_eq!(updated.title, "Publication Solo Farming");
    }

    /// Build a 1.x database raw (migrations 0001–0010), seed it like a deployed
    /// instance — a scraped manga and a local-source one, with progress and read
    /// marks — then apply 0011 and assert nothing was lost in the conversion.
    #[tokio::test]
    async fn migration_0011_converts_a_1x_database() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for sql in [
            include_str!("../../migrations/0001_library.sql"),
            include_str!("../../migrations/0002_progress_seq.sql"),
            include_str!("../../migrations/0003_categories.sql"),
            include_str!("../../migrations/0004_auth.sql"),
            include_str!("../../migrations/0005_read_marks.sql"),
            include_str!("../../migrations/0006_progress_user_seq_index.sql"),
            include_str!("../../migrations/0007_chapter_published_at.sql"),
            include_str!("../../migrations/0008_catalog.sql"),
            include_str!("../../migrations/0009_genres.sql"),
            include_str!("../../migrations/0010_updates.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }

        let shared = Uuid::nil().to_string();
        sqlx::raw_sql(
            "INSERT INTO manga (id, source_id, source_key, title, auto_download, added_at)
             VALUES ('00000000-0000-0000-0000-00000000000a', 'fixture', 'm1', 'Scraped', 1,
                     '2026-01-01T00:00:00Z'),
                    ('00000000-0000-0000-0000-00000000000b', 'local', 'Solo Farming', 'Solo Farming',
                     0, '2026-01-01T00:00:00Z');
             INSERT INTO chapters (id, manga_id, source_key, title, source_order, fetched_at)
             VALUES ('00000000-0000-0000-0000-0000000000a1',
                     '00000000-0000-0000-0000-00000000000a', 'c1', 'Chapter 1', 0,
                     '2026-01-01T00:00:00Z'),
                    ('00000000-0000-0000-0000-0000000000b1',
                     '00000000-0000-0000-0000-00000000000b', 'Solo Farming/Chapter 1',
                     'Chapter 1', 0, '2026-01-01T00:00:00Z');
             INSERT INTO manga_genres (manga_id, genre)
             VALUES ('00000000-0000-0000-0000-00000000000a', 'Action');",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO progress_events (id, user_id, manga_id, chapter_id, page, device, at)
             VALUES (?, ?, ?, ?, 4, 'test', '2026-01-02T00:00:00Z')",
        )
        .bind("00000000-0000-0000-0000-0000000000e1")
        .bind(&shared)
        .bind("00000000-0000-0000-0000-00000000000b")
        .bind("00000000-0000-0000-0000-0000000000b1")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO read_chapters (user_id, chapter_id, at)
             VALUES (?, '00000000-0000-0000-0000-0000000000a1', '2026-01-02T00:00:00Z')",
        )
        .bind(&shared)
        .execute(&pool)
        .await
        .unwrap();

        // 0011 runs inside one transaction under the real migrator; replicate
        // that here so defer_foreign_keys spans the publications rebuild.
        let migration = include_str!("../../migrations/0011_publications.sql");
        sqlx::raw_sql(&format!("BEGIN; {migration} COMMIT;"))
            .execute(&pool)
            .await
            .unwrap();

        let (kind, source_id, file_path): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT kind, source_id, file_path FROM publications WHERE id = ?")
                .bind("00000000-0000-0000-0000-00000000000a")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (kind.as_str(), source_id.as_deref(), file_path),
            ("comics", Some("fixture"), None)
        );

        let (source_id, file_path): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT source_id, file_path FROM publications WHERE id = ?")
                .bind("00000000-0000-0000-0000-00000000000b")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (source_id, file_path.as_deref()),
            (None, Some("Solo Farming"))
        );

        // Progress, read marks and genres survived under the renamed columns.
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM progress_events
             WHERE publication_id = '00000000-0000-0000-0000-00000000000b'
               AND unit_id = '00000000-0000-0000-0000-0000000000b1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
        let marks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM read_units")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(marks, 1);
        let genres: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM publication_genres WHERE publication_id = '00000000-0000-0000-0000-00000000000a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(genres, 1);
    }
}
