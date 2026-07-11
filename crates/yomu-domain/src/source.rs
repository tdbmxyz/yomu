//! What a scan-site source exposes, before anything enters the library.
//! Keys are source-scoped opaque identifiers (usually the page URL path).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// A source registered on the server (defined in `sources.d/*.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Stable identifier (slug), e.g. `"example-scans"`.
    pub id: String,
    pub name: String,
    pub base_url: Url,
    /// Catalog listings this source can browse (empty: search only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browse: Vec<BrowseSort>,
}

/// A query-less catalog listing order offered by a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseSort {
    Popular,
    Latest,
}

impl BrowseSort {
    pub fn key(self) -> &'static str {
        match self {
            BrowseSort::Popular => "popular",
            BrowseSort::Latest => "latest",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BrowseSort::Popular => "Popular",
            BrowseSort::Latest => "Latest",
        }
    }
}

/// A search hit on a source; enough to display and to add to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaSummary {
    /// Source-scoped key of the manga (opaque outside the source).
    pub key: String,
    pub title: String,
    /// Cover image address. A plain string (not `Url`): the server
    /// rewrites it to its relative cover-proxy endpoint before results
    /// leave the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Set by the server when this result is already tracked: the
    /// library manga id. Sources never fill it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<Uuid>,
}

/// Full details as scraped from the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MangaDetails {
    pub summary: MangaSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Genres/tags as printed by the source (verbatim, deduplicated). Empty
    /// when the source doesn't expose them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    /// As listed on the site; `ChapterRef::source_order` is normalized so
    /// that ordering by it *descending* gives reading order (see below).
    pub chapters: Vec<ChapterRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterRef {
    /// Source-scoped key of the chapter.
    pub key: String,
    pub title: String,
    /// Parsed chapter number when the source exposes one ("Chapter 12.5").
    /// Ordering falls back to `source_order` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<f64>,
    /// Recency rank: 0 = newest chapter. Sources listing oldest-first must
    /// reverse their listing index so number-less chapters still sort into
    /// reading order (`ORDER BY source_order DESC`).
    pub source_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanlator: Option<String>,
    /// Release date as printed by the site's listing; best-effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
}
