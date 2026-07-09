//! The library: manga the user tracks, and their chapters as known to the
//! server.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manga {
    pub id: Uuid,
    /// Which source this manga is tracked on.
    pub source_id: String,
    /// Source-scoped key (see `MangaSummary::key`).
    pub source_key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<Url>,
    /// Download new chapters automatically when the updater finds them.
    pub auto_download: bool,
    /// [`Category`] id ("reading" by default).
    #[serde(default = "default_category")]
    pub category: String,
    pub added_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<DateTime<Utc>>,
}

pub fn default_category() -> String {
    "reading".into()
}

/// A library category — a reading status (Reading / Paused / Finished by
/// default). Every manga belongs to exactly one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Stable slug, e.g. `"reading"`.
    pub id: String,
    pub name: String,
    /// Display order.
    pub position: u32,
    /// Whether the periodic updater checks this category's manga for new
    /// chapters (paused/finished series shouldn't hammer their sources).
    pub update_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub id: Uuid,
    pub manga_id: Uuid,
    pub source_key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<f64>,
    pub source_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanlator: Option<String>,
    pub fetched_at: DateTime<Utc>,
    /// Release date scraped from the source listing, when it prints one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    pub download: DownloadState,
    /// Known once pages have been listed (on download or first live read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    /// Read mark for the requesting user (bulk-marked or auto-marked as the
    /// reading position moves past the chapter).
    #[serde(default)]
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadState {
    /// Chapter is read live from the source when opened; nothing stored.
    None,
    /// Queued for the download worker.
    Pending,
    Downloading,
    Downloaded {
        at: DateTime<Utc>,
    },
    Failed {
        at: DateTime<Utc>,
        reason: String,
    },
}
