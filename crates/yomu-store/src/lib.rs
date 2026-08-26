//! Durable native-client state.
//!
//! The PWA keeps using Web Storage and the Service Worker Cache API. Native
//! shells use this SQLite store and mirror selected values into WebView
//! localStorage at boot because the shared Leptos UI requires synchronous
//! reads. SQLite is the durable authority; the mirror is a runtime adapter.

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(test)]
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use uuid::Uuid;
use yomu_domain::ProgressEvent;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid stored state: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDownload {
    pub unit_id: Uuid,
    pub publication_id: Uuid,
    pub pages: u32,
    pub bytes: u64,
    pub updated_at: DateTime<Utc>,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Invalid(format!("creating {}: {e}", parent.display())))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        Self::connect(options).await
    }

    #[cfg(test)]
    async fn in_memory() -> Result<Self> {
        Self::connect(SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true)).await
    }

    async fn connect(options: SqliteConnectOptions) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;
        for statement in MIGRATION
            .split(";\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sqlx::query(statement).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    pub async fn append_event(&self, event: &ProgressEvent) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO journal (id, event_json, at) VALUES (?, ?, ?)")
            .bind(event.id.to_string())
            .bind(serde_json::to_string(event)?)
            .bind(event.at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn journal(&self) -> Result<Vec<ProgressEvent>> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT event_json FROM journal ORDER BY at, id")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .collect()
    }

    pub async fn remove_events(&self, ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for id in ids {
            sqlx::query("DELETE FROM journal WHERE id = ?")
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn upsert_download(&self, download: &DeviceDownload) -> Result<()> {
        sqlx::query(
            "INSERT INTO device_downloads
               (unit_id, publication_id, pages, bytes, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(unit_id) DO UPDATE SET
               publication_id=excluded.publication_id, pages=excluded.pages,
               bytes=excluded.bytes, updated_at=excluded.updated_at",
        )
        .bind(download.unit_id.to_string())
        .bind(download.publication_id.to_string())
        .bind(download.pages)
        .bind(download.bytes as i64)
        .bind(download.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn downloads(&self) -> Result<Vec<DeviceDownload>> {
        let rows: Vec<(String, String, i64, i64, DateTime<Utc>)> = sqlx::query_as(
            "SELECT unit_id, publication_id, pages, bytes, updated_at
             FROM device_downloads ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(unit, publication, pages, bytes, updated_at)| {
                Ok(DeviceDownload {
                    unit_id: Uuid::parse_str(&unit)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                    publication_id: Uuid::parse_str(&publication)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                    pages: pages as u32,
                    bytes: bytes as u64,
                    updated_at,
                })
            })
            .collect()
    }

    pub async fn set_cursor(&self, scope: &str, cursor: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_cursors (scope, cursor) VALUES (?, ?)
             ON CONFLICT(scope) DO UPDATE SET cursor=excluded.cursor",
        )
        .bind(scope)
        .bind(cursor)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cursor(&self, scope: &str) -> Result<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT cursor FROM sync_cursors WHERE scope = ?")
                .bind(scope)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Durable WebView adapter values (cached device library, download marks,
    /// pull queue, and outboxes). The shell bridge only accepts yomu-owned
    /// keys, so arbitrary page localStorage cannot fill the database.
    pub async fn put_state(&self, key: &str, value: &str) -> Result<()> {
        validate_key(key)?;
        sqlx::query(
            "INSERT INTO client_state (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_state(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        sqlx::query("DELETE FROM client_state WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn state_snapshot(&self) -> Result<BTreeMap<String, String>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM client_state ORDER BY key")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }
}

pub fn is_durable_key(key: &str) -> bool {
    matches!(
        key,
        "yomu-outbox"
            | "yomu-marks-outbox"
            | "yomu-device-chapters"
            | "yomu-pull-queue"
            | "yomu-updates-seen"
    ) || key.starts_with("yomu-cache:")
}

fn validate_key(key: &str) -> Result<()> {
    if is_durable_key(key) {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!(
            "state key {key:?} is not durable"
        )))
    }
}

const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS journal (
  id TEXT PRIMARY KEY,
  event_json TEXT NOT NULL,
  at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS device_downloads (
  unit_id TEXT PRIMARY KEY,
  publication_id TEXT NOT NULL,
  pages INTEGER NOT NULL,
  bytes INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_cursors (
  scope TEXT PRIMARY KEY,
  cursor INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS client_state (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use yomu_domain::ProgressEvent;

    #[tokio::test]
    async fn journal_download_cursor_and_adapter_state_round_trip() {
        let store = Store::in_memory().await.unwrap();
        let event = ProgressEvent {
            id: Uuid::now_v7(),
            publication_id: Uuid::new_v4(),
            unit_id: Uuid::new_v4(),
            page: 7,
            device: "test".into(),
            at: Utc::now(),
        };
        store.append_event(&event).await.unwrap();
        store.append_event(&event).await.unwrap();
        assert_eq!(store.journal().await.unwrap(), vec![event.clone()]);
        store.remove_events(&[event.id]).await.unwrap();
        assert!(store.journal().await.unwrap().is_empty());

        let download = DeviceDownload {
            unit_id: event.unit_id,
            publication_id: event.publication_id,
            pages: 8,
            bytes: 1234,
            updated_at: Utc::now(),
        };
        store.upsert_download(&download).await.unwrap();
        assert_eq!(store.downloads().await.unwrap(), vec![download]);
        store.set_cursor("server:user", 42).await.unwrap();
        assert_eq!(store.cursor("server:user").await.unwrap(), Some(42));

        store.put_state("yomu-device-chapters", "{}").await.unwrap();
        assert_eq!(
            store
                .state_snapshot()
                .await
                .unwrap()
                .get("yomu-device-chapters"),
            Some(&"{}".to_string())
        );
        assert!(store.put_state("untrusted-key", "x").await.is_err());
    }
}
