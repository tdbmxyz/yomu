//! The library: publications the user tracks — scraped series and local
//! files alike — and their reading units as known to the server.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// What a publication is, for the library's kind switcher. Only `Comics`
/// exists in this slice; the others are reserved for later slices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Comics,
    Novels,
    Pdf,
}

/// Where a publication's content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Scraped from a configured source.
    Source {
        source_id: String,
        source_key: String,
    },
    /// Streamer-managed file/dir, path relative to the server's books dir.
    LocalFile { path: String },
}

/// A tracked publication (was `Manga`). The wire shape is frozen at 1.x —
/// see [`PublicationWire`]: `source_id`/`source_key` are always emitted
/// (`"local"` + the path for [`Origin::LocalFile`]), new fields are additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PublicationWire", into = "PublicationWire")]
pub struct Publication {
    pub id: Uuid,
    pub kind: Kind,
    pub origin: Origin,
    pub title: String,
    pub description: Option<String>,
    pub cover_url: Option<Url>,
    /// Download new units automatically (Source origin only).
    pub auto_download: bool,
    /// [`Category`] id ("reading" by default).
    pub category: String,
    pub genres: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    /// Set while a LocalFile publication's file has vanished from disk.
    pub missing_since: Option<DateTime<Utc>>,
}

/// The frozen 1.x JSON shape of a publication. One place defines the wire:
/// old clients read `source_id`/`source_key` as required strings, so
/// LocalFile serializes them as `"local"` + the path; deserializing a 1.x
/// payload with `source_id == "local"` heals into a LocalFile origin.
#[derive(Serialize, Deserialize)]
struct PublicationWire {
    id: Uuid,
    #[serde(default)]
    kind: Kind,
    source_id: String,
    source_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cover_url: Option<Url>,
    auto_download: bool,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    genres: Vec<String>,
    added_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing_since: Option<DateTime<Utc>>,
}

impl From<Publication> for PublicationWire {
    fn from(p: Publication) -> Self {
        let (source_id, source_key, file_path) = match p.origin {
            Origin::Source {
                source_id,
                source_key,
            } => (source_id, source_key, None),
            Origin::LocalFile { path } => ("local".into(), path.clone(), Some(path)),
        };
        PublicationWire {
            id: p.id,
            kind: p.kind,
            source_id,
            source_key,
            file_path,
            title: p.title,
            description: p.description,
            cover_url: p.cover_url,
            auto_download: p.auto_download,
            category: p.category,
            genres: p.genres,
            added_at: p.added_at,
            last_checked_at: p.last_checked_at,
            missing_since: p.missing_since,
        }
    }
}

impl TryFrom<PublicationWire> for Publication {
    type Error = String;

    fn try_from(w: PublicationWire) -> Result<Self, String> {
        let origin = match w.file_path {
            Some(path) => Origin::LocalFile { path },
            None if w.source_id == "local" => Origin::LocalFile { path: w.source_key },
            None => Origin::Source {
                source_id: w.source_id,
                source_key: w.source_key,
            },
        };
        Ok(Publication {
            id: w.id,
            kind: w.kind,
            origin,
            title: w.title,
            description: w.description,
            cover_url: w.cover_url,
            auto_download: w.auto_download,
            category: w.category,
            genres: w.genres,
            added_at: w.added_at,
            last_checked_at: w.last_checked_at,
            missing_since: w.missing_since,
        })
    }
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
pub struct ReadingUnit {
    pub id: Uuid,
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
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

#[cfg(test)]
mod wire {
    use super::*;

    /// A 1.x client's manga JSON must keep round-tripping unchanged.
    #[test]
    fn scraped_publication_keeps_1x_field_names() {
        let json = r#"{
            "id":"018f4a70-0000-7000-8000-000000000001",
            "source_id":"fixture","source_key":"solo-farming",
            "title":"Solo Farming","auto_download":true,
            "category":"reading","added_at":"2026-01-01T00:00:00Z"
        }"#;
        let p: Publication = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.origin,
            Origin::Source {
                source_id: "fixture".into(),
                source_key: "solo-farming".into()
            }
        );
        assert_eq!(p.kind, Kind::Comics); // additive default
        let out: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(out["source_id"], "fixture");
        assert_eq!(out["source_key"], "solo-farming");
        assert_eq!(out["kind"], "comics");
        assert!(out.get("file_path").is_none());
        assert!(
            out.get("origin").is_none(),
            "no nested origin object on the wire"
        );
    }

    /// 1.x rows/backups carried local files as source_id="local"; they must
    /// deserialize into a LocalFile origin, and LocalFile must serialize the
    /// same required fields back so old clients keep parsing responses.
    #[test]
    fn local_publication_wire_compat() {
        let json = r#"{
            "id":"018f4a70-0000-7000-8000-000000000002",
            "source_id":"local","source_key":"Solo Farming",
            "title":"Solo Farming","auto_download":false,
            "category":"reading","added_at":"2026-01-01T00:00:00Z"
        }"#;
        let p: Publication = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.origin,
            Origin::LocalFile {
                path: "Solo Farming".into()
            }
        );

        let out: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(out["source_id"], "local", "old clients require source_id");
        assert_eq!(out["source_key"], "Solo Farming");
        assert_eq!(out["file_path"], "Solo Farming");
    }

    #[test]
    fn reading_unit_serializes_manga_id() {
        let unit = ReadingUnit {
            id: uuid::Uuid::from_u128(1),
            publication_id: uuid::Uuid::from_u128(2),
            source_key: "c1".into(),
            title: "Chapter 1".into(),
            number: Some(1.0),
            source_order: 0,
            scanlator: None,
            fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            published_at: None,
            download: DownloadState::None,
            page_count: None,
            read: false,
        };
        let out: serde_json::Value = serde_json::to_value(&unit).unwrap();
        assert!(out.get("manga_id").is_some());
        assert!(out.get("publication_id").is_none());
    }
}
