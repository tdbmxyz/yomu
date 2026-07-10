//! Request/response envelopes of the HTTP API (`/api/v1`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Chapter, Manga, Position};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    /// Short commit hash the server was built from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Uniform error body returned by the API for non-2xx responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub message: String,
}

/// Add a manga found via source search to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddMangaRequest {
    pub source_id: String,
    pub source_key: String,
    #[serde(default)]
    pub auto_download: bool,
}

/// Per-manga settings. `category` is optional so clients toggling one
/// setting don't have to know the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMangaRequest {
    pub auto_download: bool,
    /// Move to this [`crate::Category`] id; `None` keeps the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Per-category settings (`PUT /categories/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCategoryRequest {
    /// Include this category's manga in the periodic new-chapter check.
    pub update_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MangaWithPosition {
    #[serde(flatten)]
    pub manga: Manga,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    pub chapter_count: u32,
    /// Chapters the user hasn't read (no read mark).
    #[serde(default)]
    pub unread_count: u32,
    /// Chapters fully downloaded on the server.
    #[serde(default)]
    pub downloaded_count: u32,
    /// When the most recently fetched chapter arrived (drives the client's
    /// "new chapters" ordering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_chapter_at: Option<DateTime<Utc>>,
    /// Title of the chapter the position points at, for "resume" labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_chapter_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MangaDetailResponse {
    pub manga: Manga,
    /// Ordered for reading: number ascending, source_order as fallback.
    pub chapters: Vec<Chapter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// Set the current reading position (the server wraps it into a journal
/// event; `device` identifies the writer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetPositionRequest {
    pub chapter_id: Uuid,
    pub page: u32,
    #[serde(default = "default_device")]
    pub device: String,
}

fn default_device() -> String {
    "web".into()
}

/// Batch journal upload from an offline client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEventsRequest {
    pub events: Vec<crate::ProgressEvent>,
}

/// Outcome of a journal push. Events referencing manga the server no longer
/// knows are *skipped*, not errors: the client may clear them from its
/// outbox (they can never apply) instead of retrying forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEventsResponse {
    pub accepted: u32,
    pub skipped: u32,
}

/// Journal page for incremental sync (`?since=<cursor>`). The cursor is the
/// server-assigned arrival sequence — not the event id, which is stamped by
/// the observing device and would skip late-arriving offline pushes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<crate::ProgressEvent>,
    /// Pass as `?since=` on the next poll; `None` when the journal is empty
    /// up to this page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_since: Option<i64>,
}

/// One source's slice of a cross-source search: every configured source
/// gets an entry, and a failing source reports its error instead of
/// silently vanishing from the results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSearchResults {
    pub source_id: String,
    pub source_name: String,
    #[serde(default)]
    pub results: Vec<crate::MangaSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Queue several chapters for server download (`POST /chapters/download`).
/// The download worker drains them one by one with the source's politeness
/// delay, so a large batch is slow by design, not hammering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadChaptersRequest {
    pub chapter_ids: Vec<Uuid>,
}

/// Mark chapters read or unread for the current user
/// (`POST /chapters/mark`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkChaptersRequest {
    pub chapter_ids: Vec<Uuid>,
    pub read: bool,
}

/// Outcome of a bulk chapter action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkChaptersResponse {
    pub affected: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagesResponse {
    pub chapter_id: Uuid,
    pub page_count: u32,
    /// Whether pages are served from disk (downloaded) or proxied live.
    pub downloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshResponse {
    pub new_chapters: u32,
    pub checked_at: DateTime<Utc>,
}
