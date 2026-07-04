//! What a scan-site source exposes, before anything enters the library.
//! Keys are source-scoped opaque identifiers (usually the page URL path).

use serde::{Deserialize, Serialize};
use url::Url;

/// A source registered on the server (defined in `sources.d/*.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Stable identifier (slug), e.g. `"example-scans"`.
    pub id: String,
    pub name: String,
    pub base_url: Url,
}

/// A search hit on a source; enough to display and to add to the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MangaSummary {
    /// Source-scoped key of the manga (opaque outside the source).
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<Url>,
}

/// Full details as scraped from the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MangaDetails {
    pub summary: MangaSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// In source order (as listed on the site), oldest or newest first —
    /// `ChapterRef::source_order` records the position either way.
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
    /// Position in the source listing (0 = first listed).
    pub source_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanlator: Option<String>,
}
