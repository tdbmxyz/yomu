//! Source abstraction for yomu.
//!
//! A [`Source`] knows how to search a site, list a manga's chapters and
//! resolve a chapter's page images. yomu deliberately has no extension
//! system: sources are declarative [`selector::SelectorSource`]s (a TOML
//! file with CSS selectors — enough for most scan sites) or, later, native
//! Rust implementations for API-based sites. Files already on the server's
//! disk are not a source: the server's streamer serves them directly.

mod dates;
pub mod registry;
pub mod selector;

use bytes::Bytes;
use url::Url;
use yomu_domain::{BrowseSort, MangaDetails, MangaSummary};

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("request failed: {0}")]
    Http(String),
    #[error("unexpected page structure: {0}")]
    Parse(String),
    #[error("invalid source definition: {0}")]
    Definition(String),
}

pub type Result<T> = std::result::Result<T, SourceError>;

/// A fetched page image with its content type.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub bytes: Bytes,
    pub content_type: String,
}

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> &Url;

    async fn search(&self, query: &str) -> Result<Vec<MangaSummary>>;

    /// Catalog listings this source can [`Source::browse`]. Empty (the
    /// default) means the source is search-only.
    fn browse_sorts(&self) -> Vec<BrowseSort> {
        Vec::new()
    }

    /// A query-less catalog listing, `page` starting at 1. An empty page
    /// means the listing is exhausted.
    async fn browse(&self, sort: BrowseSort, page: u32) -> Result<Vec<MangaSummary>> {
        let _ = (sort, page);
        Err(SourceError::Definition(
            "this source has no browse listings".into(),
        ))
    }

    /// Details + chapter list for a manga key returned by `search`.
    async fn manga(&self, key: &str) -> Result<MangaDetails>;

    /// Image URLs of a chapter, in reading order.
    async fn pages(&self, chapter_key: &str) -> Result<Vec<Url>>;

    /// Fetch one image (page or cover). Sources may add referer headers etc.
    async fn image(&self, url: &Url) -> Result<ImageData>;
}
