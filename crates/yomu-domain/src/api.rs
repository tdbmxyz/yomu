//! Request/response envelopes of the HTTP API (`/api/v1`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Locator, Publication, ReadingUnit};

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

/// Add a publication found via source search to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddPublicationRequest {
    pub source_id: String,
    pub source_key: String,
    #[serde(default)]
    pub auto_download: bool,
}

/// Per-publication settings. `category` is optional so clients toggling one
/// setting don't have to know the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePublicationRequest {
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
pub struct PublicationWithLocator {
    #[serde(flatten)]
    pub publication: Publication,
    #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
    #[serde(rename = "chapter_count")]
    pub unit_count: u32,
    /// Units the user hasn't read (no read mark).
    #[serde(default)]
    pub unread_count: u32,
    /// Units fully downloaded on the server.
    #[serde(default)]
    pub downloaded_count: u32,
    /// When the most recently fetched unit arrived (drives the client's
    /// "new chapters" ordering).
    #[serde(
        rename = "latest_chapter_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_unit_at: Option<DateTime<Utc>>,
    /// Title of the unit the locator points at, for "resume" labels.
    #[serde(
        rename = "position_chapter_title",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub locator_unit_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationDetailResponse {
    #[serde(rename = "manga")]
    pub publication: Publication,
    /// Ordered for reading: number ascending, source_order as fallback.
    #[serde(rename = "chapters")]
    pub units: Vec<ReadingUnit>,
    #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
}

/// Set the current locator (the server wraps it into a journal
/// event; `device` identifies the writer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetLocatorRequest {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
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

/// Queue several units for server download (`POST /chapters/download`).
/// The download worker drains them one by one with the source's politeness
/// delay, so a large batch is slow by design, not hammering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadUnitsRequest {
    #[serde(rename = "chapter_ids")]
    pub unit_ids: Vec<Uuid>,
}

/// Mark units read or unread for the current user
/// (`POST /chapters/mark`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkUnitsRequest {
    #[serde(rename = "chapter_ids")]
    pub unit_ids: Vec<Uuid>,
    pub read: bool,
}

/// Outcome of a bulk unit action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkUnitsResponse {
    pub affected: u32,
}

/// Live page progress of the chapter the download worker is fetching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Pages written so far (1-based).
    pub page: u32,
    pub total: u32,
}

/// One chapter in the download queue (pending / downloading / failed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadQueueEntry {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    #[serde(rename = "manga_title")]
    pub publication_title: String,
    #[serde(rename = "chapter_title")]
    pub unit_title: String,
    pub state: crate::DownloadState,
    /// Present only for the chapter currently downloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<DownloadProgress>,
}

/// `GET /downloads`: the queue plus a server-storage summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadsResponse {
    pub queue: Vec<DownloadQueueEntry>,
    pub server_downloaded_chapters: u32,
    pub server_downloaded_pages: u32,
}

/// One updater round's find for one manga (`GET /updates`): what shell
/// notifications announce. Mirrors the ntfy message content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEvent {
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    #[serde(rename = "manga_title")]
    pub publication_title: String,
    #[serde(rename = "chapter_count")]
    pub unit_count: u32,
    pub first_title: String,
    pub last_title: String,
    pub created_at: DateTime<Utc>,
}

/// `GET /updates?since=`: updater-found chapters, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatesResponse {
    pub updates: Vec<UpdateEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagesResponse {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    pub page_count: u32,
    /// Whether pages are served from disk (downloaded) or proxied live.
    pub downloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshResponse {
    #[serde(rename = "new_chapters")]
    pub new_units: u32,
    pub checked_at: DateTime<Utc>,
}

/// `POST /library/rescan` outcome: what the streamer scan changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RescanResponse {
    pub added: u32,
    pub updated: u32,
    /// LocalFile publications newly flagged missing by this scan.
    pub missing: u32,
}

/// Golden wire tests: the renamed envelopes must keep emitting and accepting
/// the frozen 1.x JSON field names.
#[cfg(test)]
mod wire {
    use super::*;
    use crate::{Kind, Locations, Origin};

    fn publication() -> Publication {
        Publication {
            id: Uuid::from_u128(1),
            kind: Kind::Comics,
            origin: Origin::Source {
                source_id: "fixture".into(),
                source_key: "solo-farming".into(),
            },
            title: "Solo Farming".into(),
            description: Some("A farm, solo.".into()),
            cover_url: Some("https://example.com/cover.png".parse().unwrap()),
            auto_download: true,
            category: "reading".into(),
            genres: vec!["fantasy".into()],
            added_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            last_checked_at: Some("2026-01-02T00:00:00Z".parse().unwrap()),
            missing_since: None,
        }
    }

    fn locator() -> Locator {
        Locator {
            unit_id: Uuid::from_u128(9),
            locations: Locations::Page { page: 3 },
            at: "2026-01-03T00:00:00Z".parse().unwrap(),
        }
    }

    /// The library-list entry: publication fields flattened at the top level
    /// (through the `PublicationWire` mirror) plus the renamed counters.
    #[test]
    fn publication_with_locator_keeps_1x_field_names() {
        let entry = PublicationWithLocator {
            publication: publication(),
            locator: Some(locator()),
            unit_count: 12,
            unread_count: 4,
            downloaded_count: 2,
            latest_unit_at: Some("2026-01-02T00:00:00Z".parse().unwrap()),
            locator_unit_title: Some("Chapter 12".into()),
        };
        let out = serde_json::to_value(&entry).unwrap();
        // Flatten + the try_from/into mirror: publication fields sit at the
        // top level under their wire names.
        assert_eq!(out["source_id"], "fixture");
        assert_eq!(out["title"], "Solo Farming");
        assert!(out.get("publication").is_none());
        // Renamed envelope fields.
        assert_eq!(out["position"]["page"], 3);
        assert!(out.get("locator").is_none());
        assert_eq!(out["chapter_count"], 12);
        assert!(out.get("unit_count").is_none());
        assert_eq!(out["latest_chapter_at"], "2026-01-02T00:00:00Z");
        assert_eq!(out["position_chapter_title"], "Chapter 12");

        let back: PublicationWithLocator = serde_json::from_value(out).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn publication_detail_response_keeps_1x_keys() {
        let detail = PublicationDetailResponse {
            publication: publication(),
            units: vec![],
            locator: Some(locator()),
        };
        let out = serde_json::to_value(&detail).unwrap();
        assert!(out.get("manga").is_some());
        assert!(out.get("chapters").is_some());
        assert!(out.get("position").is_some());
        assert!(out.get("publication").is_none());
        assert!(out.get("units").is_none());
        assert!(out.get("locator").is_none());
    }

    #[test]
    fn set_locator_request_reads_1x_body() {
        let req: SetLocatorRequest = serde_json::from_str(
            r#"{"chapter_id":"00000000-0000-0000-0000-000000000009","page":3}"#,
        )
        .unwrap();
        assert_eq!(req.unit_id, Uuid::from_u128(9));
        assert_eq!(req.page, 3);
        assert_eq!(req.device, "web");
    }

    #[test]
    fn download_units_request_reads_chapter_ids() {
        let req: DownloadUnitsRequest =
            serde_json::from_str(r#"{"chapter_ids":["00000000-0000-0000-0000-000000000009"]}"#)
                .unwrap();
        assert_eq!(req.unit_ids, vec![Uuid::from_u128(9)]);
    }

    #[test]
    fn update_event_serializes_1x_names() {
        let event = UpdateEvent {
            publication_id: Uuid::from_u128(1),
            publication_title: "Solo Farming".into(),
            unit_count: 3,
            first_title: "Chapter 10".into(),
            last_title: "Chapter 12".into(),
            created_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        };
        let out = serde_json::to_value(&event).unwrap();
        assert_eq!(out["manga_id"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(out["manga_title"], "Solo Farming");
        assert_eq!(out["chapter_count"], 3);
        assert!(out.get("publication_id").is_none());
        assert!(out.get("unit_count").is_none());
    }
}
