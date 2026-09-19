//! Synthetic, file-backed upgrade and snapshot drills. Never opens operator data.
use super::*;
use sqlx::migrate::Migrator;
use std::borrow::Cow;

const PUBLICATION: &str = "00000000-0000-0000-0000-00000000000a";
const UNIT: &str = "00000000-0000-0000-0000-00000000000b";
const ALICE: &str = "00000000-0000-0000-0000-00000000000c";
const DUPLICATE: &str = "00000000-0000-0000-0000-00000000000d";
const SHARED: &str = "00000000-0000-0000-0000-000000000000";

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("yomu-recovery-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn historical(path: &Path, version: i64, accounts: usize) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .unwrap();
    let all = sqlx::migrate!("./migrations");
    let old = Migrator {
        migrations: Cow::Owned(
            all.iter()
                .filter(|m| m.version <= version)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    let (publications, units, publication_id, unit_id, marks) = if version < 11 {
        (
            "manga",
            "chapters",
            "manga_id",
            "chapter_id",
            "read_chapters",
        )
    } else {
        (
            "publications",
            "reading_units",
            "publication_id",
            "unit_id",
            "read_units",
        )
    };
    sqlx::query(&format!("INSERT INTO {publications} (id,source_id,source_key,title,added_at) VALUES (?, 'fixture','m1','Synthetic book','2026-01-01T00:00:00Z')"))
        .bind(PUBLICATION).execute(&pool).await.unwrap();
    sqlx::query(&format!("INSERT INTO {units} (id,{publication_id},source_key,title,source_order,fetched_at,download_state,downloaded_at,page_count) VALUES (?,?,'c1','One',0,'2026-01-01T00:00:00Z','downloaded','2026-01-02T00:00:00Z',12)"))
        .bind(UNIT).bind(PUBLICATION).execute(&pool).await.unwrap();
    for (id, subject, name) in [
        (ALICE, "oidc-alice", "alice"),
        (DUPLICATE, "proxy-alice", "alice-proxy-alice"),
    ]
    .into_iter()
    .take(accounts)
    {
        sqlx::query("INSERT INTO users (id,subject,username,display_name,created_at) VALUES (?,?,?,?,'2026-01-01T00:00:00Z')")
            .bind(id).bind(subject).bind(name).bind(name).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (token_hash,user_id,created_at,expires_at) VALUES (?,?,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
            .bind(id).bind(id).execute(&pool).await.unwrap();
    }
    // For an identity collision, history exists on the duplicate too.
    for owner in [SHARED]
        .into_iter()
        .chain((accounts == 2).then_some(DUPLICATE))
    {
        sqlx::query(&format!("INSERT INTO progress_events (id,user_id,{publication_id},{unit_id},page,device,at) VALUES (?,?,?,?,7,'fixture','2026-01-03T00:00:00Z')"))
            .bind(Uuid::new_v4().to_string()).bind(owner).bind(PUBLICATION).bind(UNIT).execute(&pool).await.unwrap();
        sqlx::query(&format!(
            "INSERT INTO {marks} (user_id,{unit_id},at) VALUES (?,?,'2026-01-03T00:00:00Z')"
        ))
        .bind(owner)
        .bind(UNIT)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;
}

async fn assert_restored(db: &Db, accounts: usize) -> Vec<ProgressEvent> {
    let owner = Uuid::parse_str(if accounts == 0 { SHARED } else { ALICE }).unwrap();
    assert_eq!(db.integrity_check().await.unwrap(), "ok");
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&db.pool)
            .await
            .unwrap()
            .is_empty()
    );
    let unit = db.get_unit(Uuid::parse_str(UNIT).unwrap()).await.unwrap();
    assert_eq!(unit.page_count, Some(12));
    assert!(matches!(unit.download, DownloadState::Downloaded { .. }));
    let events = db.export_events(owner).await.unwrap();
    assert_eq!(events.len(), if accounts == 2 { 2 } else { 1 });
    assert!(
        events
            .iter()
            .all(|e| e.page == 7 && e.unit_id.to_string() == UNIT)
    );
    let marks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM read_units WHERE user_id = ?")
        .bind(owner.to_string())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(marks, 1);
    if accounts > 0 {
        assert_eq!(db.user_by_session(ALICE).await.unwrap().id, owner);
        assert!(db.export_events(Uuid::nil()).await.unwrap().is_empty());
        if accounts == 2 {
            assert_eq!(db.user_by_session(DUPLICATE).await.unwrap().id, owner);
            let aliases: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM user_identities WHERE user_id = ?")
                    .bind(ALICE)
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
            assert_eq!(aliases, 2);
        }
    }
    events
}

#[tokio::test]
async fn historical_upgrade_snapshot_and_reopen_preserve_history() {
    for (version, accounts) in [(10, 0), (11, 0), (14, 1), (15, 2)] {
        let scratch = Scratch::new();
        let path = scratch.0.join("original.db");
        historical(&path, version, accounts).await;
        let db = Db::connect(&path).await.unwrap();
        let expected = assert_restored(&db, accounts).await;
        let snapshot = scratch.0.join("snapshot.db");
        // SQLite creates a consistent standalone copy while the WAL-backed
        // source is open; copying the main .db file alone is not a backup.
        sqlx::query("VACUUM INTO ?")
            .bind(snapshot.to_str().unwrap())
            .execute(&db.pool)
            .await
            .unwrap();
        // Prove the backup is independent from later source changes.
        sqlx::query("DELETE FROM progress_events")
            .execute(&db.pool)
            .await
            .unwrap();
        db.pool.close().await;
        for _ in 0..2 {
            let restored = Db::connect(&snapshot).await.unwrap();
            assert_eq!(assert_restored(&restored, accounts).await, expected);
            restored.pool.close().await;
        }
    }
}
