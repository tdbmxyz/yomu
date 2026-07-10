//! Declarative source driven by CSS selectors — the "easy way to add a scan
//! site". A site is described by a TOML file (see `sources.d/example.toml`
//! in yomu-server); no code required. The selector mini-syntax is
//! `css selector[@attribute]`: without `@attr` the element's text is taken,
//! with it the attribute's value. Relative URLs resolve against the page.
//!
//! Parsing is split from fetching so every selector rule is unit-testable
//! against fixture HTML.

use std::time::Duration;

use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use url::Url;
use yomu_domain::{BrowseSort, ChapterRef, MangaDetails, MangaSummary};

use crate::{ImageData, Result, Source, SourceError};

/// TOML definition of a selector source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectorSpec {
    pub id: String,
    pub name: String,
    pub base_url: Url,
    /// Milliseconds to wait between two requests to the site.
    #[serde(default = "default_min_delay_ms")]
    pub min_delay_ms: u64,
    /// Extra headers, e.g. a Referer some sites require for images.
    #[serde(default)]
    pub referer: Option<String>,

    pub search: SearchSpec,
    #[serde(default)]
    pub browse: BrowseSpec,
    pub manga: MangaSpec,
    pub pages: PagesSpec,
}

fn default_min_delay_ms() -> u64 {
    500
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    /// URL template; `{base}` and `{query}` (url-encoded) are substituted.
    pub url: String,
    /// Selector matching one search result.
    pub item: String,
    /// Relative to item; defaults to the item's own text.
    #[serde(default)]
    pub title: Option<String>,
    /// Relative to item; must yield the manga page URL (the manga key).
    pub link: String,
    #[serde(default)]
    pub cover: Option<String>,
}

/// Query-less catalog listings (`[browse.popular]`, `[browse.latest]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowseSpec {
    #[serde(default)]
    pub popular: Option<ListingSpec>,
    #[serde(default)]
    pub latest: Option<ListingSpec>,
}

/// One listing. Result selectors default to the `[search]` ones — most
/// sites render listings and search results with the same cards.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListingSpec {
    /// URL template; `{base}`, `{page}` (1-based) and `{offset}`
    /// ((page − 1) × `page_size`, for offset-paginated endpoints) are
    /// substituted. A template with neither `{page}` nor `{offset}` only
    /// ever yields its first page.
    pub url: String,
    /// Items per page; required when the template uses `{offset}`.
    #[serde(default)]
    pub page_size: Option<u32>,
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MangaSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    /// Some sites load the chapter list as an HTML fragment from a
    /// separate endpoint (htmx-style) instead of rendering it into the
    /// manga page. Template substituting `{url}` (the manga page URL) and
    /// `{url_parent}` (the same URL minus its last path segment).
    #[serde(default)]
    pub chapters_url: Option<String>,
    /// Selector matching one chapter entry on the manga page (or on the
    /// `chapters_url` fragment when that is set).
    pub chapter_item: String,
    #[serde(default)]
    pub chapter_title: Option<String>,
    /// Relative to chapter item; yields the chapter page URL (chapter key).
    pub chapter_link: String,
    /// Relative to chapter item; yields the chapter's release date as the
    /// site prints it. Parsed as RFC 3339, then `chapter_date_format`,
    /// then English relative phrases ("2 days ago"). Optional;
    /// unparseable text is ignored.
    #[serde(default)]
    pub chapter_date: Option<String>,
    /// chrono format string for sites printing a local absolute
    /// convention (e.g. "%Y/%m/%d").
    #[serde(default)]
    pub chapter_date_format: Option<String>,
    /// Regex with one capture group extracting the chapter number from the
    /// chapter title (fallback: from the chapter URL).
    #[serde(default = "default_number_regex")]
    pub chapter_number_regex: String,
    /// Which end of the site's chapter list is the latest chapter. Only
    /// matters for chapters whose number can't be parsed: it decides their
    /// fallback reading order.
    #[serde(default)]
    pub chapter_order: ChapterOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterOrder {
    /// The common scan-site layout: latest chapter on top.
    #[default]
    NewestFirst,
    OldestFirst,
}

fn default_number_regex() -> String {
    // Recognise the common chapter-word spellings, including "Episode"/"Ep"
    // used by most manhwa (their titles are "Episode 12", not "Chapter 12",
    // and without this every chapter parses to a null number and sorts by
    // listing order only). Still keyword-anchored — a bare number in a noisy
    // title is too ambiguous to trust; a site with an unusual scheme overrides
    // `chapter_number_regex` per source.
    r"(?i)(?:chapter|chap|episode|ep|ch)\.?\s*(\d+(?:\.\d+)?)".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PagesSpec {
    /// Like `manga.chapters_url`: separate fragment endpoint for the page
    /// images, when the chapter page doesn't render them itself. Same
    /// placeholders, relative to the chapter page URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Selector matching every page image on the chapter page (or on the
    /// `url` fragment when that is set).
    pub image: String,
}

/// One `selector[@attr]` rule, compiled. An empty selector part (`"@href"`)
/// addresses the matched element itself.
struct Rule {
    selector: Option<Selector>,
    attr: Option<String>,
}

impl Rule {
    fn parse(raw: &str) -> Result<Self> {
        let (css, attr) = match raw.rsplit_once('@') {
            Some((css, attr)) if !attr.contains(']') => (css.trim(), Some(attr.to_string())),
            _ => (raw.trim(), None),
        };
        if attr.as_deref().is_some_and(|a| a.trim().is_empty()) {
            return Err(SourceError::Definition(format!(
                "rule {raw:?}: empty attribute after '@'"
            )));
        }
        let selector = if css.is_empty() {
            None
        } else {
            Some(
                Selector::parse(css)
                    .map_err(|e| SourceError::Definition(format!("selector {raw:?}: {e}")))?,
            )
        };
        Ok(Self {
            selector,
            attr: attr.map(|a| a.trim().to_string()),
        })
    }

    /// Extract from the first match under `el` (or `el` itself).
    fn extract(&self, el: ElementRef) -> Option<String> {
        let target = match &self.selector {
            Some(selector) => el.select(selector).next()?,
            None => el,
        };
        let value = match &self.attr {
            Some(attr) => target.value().attr(attr)?.to_string(),
            None => target.text().collect::<String>(),
        };
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        (!value.is_empty()).then_some(value)
    }
}

/// Whitespace-normalized text of an element (default when no title rule).
fn text_of(el: ElementRef) -> Option<String> {
    let text = el.text().collect::<String>();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// A compiled browse listing (URL template + result-card rules).
struct CompiledListing {
    url: String,
    page_size: Option<u32>,
    item: Selector,
    title: Option<Rule>,
    link: Rule,
    cover: Option<Rule>,
}

struct CompiledSpec {
    search_item: Selector,
    search_title: Option<Rule>,
    search_link: Rule,
    search_cover: Option<Rule>,
    listings: Vec<(BrowseSort, CompiledListing)>,
    manga_title: Option<Rule>,
    manga_description: Option<Rule>,
    manga_cover: Option<Rule>,
    chapter_item: Selector,
    chapter_title: Option<Rule>,
    chapter_link: Rule,
    chapter_date: Option<Rule>,
    chapter_number: Regex,
    page_image: Rule,
}

pub struct SelectorSource {
    spec: SelectorSpec,
    compiled: CompiledSpec,
    client: reqwest::Client,
    /// Earliest instant the next request may *start*, advanced by
    /// `min_delay_ms` per reserved request. `None` until the first request.
    /// Only the reservation is under the lock — see `get`.
    next_slot: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl SelectorSource {
    pub fn new(spec: SelectorSpec) -> Result<Self> {
        let sel = |raw: &str| {
            Selector::parse(raw)
                .map_err(|e| SourceError::Definition(format!("selector {raw:?}: {e}")))
        };
        let rule_opt = |raw: &Option<String>| raw.as_deref().map(Rule::parse).transpose();

        // Listing selectors default to the search ones: most sites render
        // search results and catalog listings with the same cards.
        let compile_listing = |listing: &ListingSpec| -> Result<CompiledListing> {
            Ok(CompiledListing {
                url: listing.url.clone(),
                page_size: listing.page_size,
                item: sel(listing.item.as_deref().unwrap_or(&spec.search.item))?,
                title: rule_opt(&listing.title.clone().or_else(|| spec.search.title.clone()))?,
                link: Rule::parse(listing.link.as_deref().unwrap_or(&spec.search.link))?,
                cover: rule_opt(&listing.cover.clone().or_else(|| spec.search.cover.clone()))?,
            })
        };
        let mut listings = Vec::new();
        if let Some(listing) = &spec.browse.popular {
            listings.push((BrowseSort::Popular, compile_listing(listing)?));
        }
        if let Some(listing) = &spec.browse.latest {
            listings.push((BrowseSort::Latest, compile_listing(listing)?));
        }

        let compiled = CompiledSpec {
            search_item: sel(&spec.search.item)?,
            search_title: rule_opt(&spec.search.title)?,
            search_link: Rule::parse(&spec.search.link)?,
            search_cover: rule_opt(&spec.search.cover)?,
            listings,
            manga_title: rule_opt(&spec.manga.title)?,
            manga_description: rule_opt(&spec.manga.description)?,
            manga_cover: rule_opt(&spec.manga.cover)?,
            chapter_item: sel(&spec.manga.chapter_item)?,
            chapter_title: rule_opt(&spec.manga.chapter_title)?,
            chapter_link: Rule::parse(&spec.manga.chapter_link)?,
            chapter_date: rule_opt(&spec.manga.chapter_date)?,
            chapter_number: Regex::new(&spec.manga.chapter_number_regex)
                .map_err(|e| SourceError::Definition(format!("chapter_number_regex: {e}")))?,
            page_image: Rule::parse(&spec.pages.image)?,
        };
        if compiled.page_image.selector.is_none() {
            return Err(SourceError::Definition(
                "pages.image needs a selector (not just an attribute)".into(),
            ));
        }
        // A broken search template must fail at startup, not on first use.
        if !spec.search.url.contains("{query}") {
            return Err(SourceError::Definition(format!(
                "search url {:?} has no {{query}} placeholder",
                spec.search.url
            )));
        }
        search_url(&spec, "probe").map_err(|_| {
            SourceError::Definition(format!(
                "search url {:?} does not substitute into a valid URL",
                spec.search.url
            ))
        })?;
        // Same startup validation for the browse listing templates.
        for (sort, listing) in &compiled.listings {
            if listing.url.contains("{offset}") && listing.page_size.is_none() {
                return Err(SourceError::Definition(format!(
                    "browse.{} url uses {{offset}} but has no page_size",
                    sort.key()
                )));
            }
            listing_url(&spec, listing, 1).map_err(|_| {
                SourceError::Definition(format!(
                    "browse.{} url {:?} does not substitute into a valid URL",
                    sort.key(),
                    listing.url
                ))
            })?;
        }
        // And for the optional fragment-endpoint templates.
        for (name, template) in [
            ("manga.chapters_url", &spec.manga.chapters_url),
            ("pages.url", &spec.pages.url),
        ] {
            if let Some(template) = template {
                entity_url(template, &spec.base_url).map_err(|_| {
                    SourceError::Definition(format!(
                        "{name} {template:?} does not substitute into a valid URL"
                    ))
                })?;
            }
        }

        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(referer) = &spec.referer
            && let Ok(value) = referer.parse()
        {
            headers.insert(reqwest::header::REFERER, value);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) yomu/0.1")
            .default_headers(headers)
            // Cover/page image URLs come from parsed site HTML, so a redirect
            // could aim the server at an internal address. Cap the hops and
            // refuse any that land on a private/loopback target (SSRF guard).
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if is_private_target(attempt.url()) {
                    attempt.error("redirect to a private address blocked")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| SourceError::Http(e.to_string()))?;

        Ok(Self {
            spec,
            compiled,
            client,
            next_slot: tokio::sync::Mutex::new(None),
        })
    }

    // ---- pure parsing (unit-tested against fixtures) ----

    pub fn parse_search(&self, html: &str, page_url: &Url) -> Result<Vec<MangaSummary>> {
        Ok(parse_cards(
            html,
            page_url,
            &self.compiled.search_item,
            &self.compiled.search_title,
            &self.compiled.search_link,
            &self.compiled.search_cover,
        ))
    }

    pub fn parse_listing(
        &self,
        sort: BrowseSort,
        html: &str,
        page_url: &Url,
    ) -> Result<Vec<MangaSummary>> {
        let listing = self.listing(sort)?;
        Ok(parse_cards(
            html,
            page_url,
            &listing.item,
            &listing.title,
            &listing.link,
            &listing.cover,
        ))
    }

    fn listing(&self, sort: BrowseSort) -> Result<&CompiledListing> {
        self.compiled
            .listings
            .iter()
            .find(|(s, _)| *s == sort)
            .map(|(_, listing)| listing)
            .ok_or_else(|| {
                SourceError::Definition(format!("no browse.{} listing configured", sort.key()))
            })
    }

    pub fn parse_manga(&self, html: &str, page_url: &Url) -> Result<MangaDetails> {
        self.parse_manga_parts(html, html, page_url, page_url)
    }

    /// Manga page and chapter list parsed from separate documents — the
    /// same one unless `manga.chapters_url` points the chapter list at its
    /// own fragment endpoint.
    pub fn parse_manga_parts(
        &self,
        html: &str,
        chapters_html: &str,
        page_url: &Url,
        chapters_url: &Url,
    ) -> Result<MangaDetails> {
        let doc = Html::parse_document(html);
        let root = doc.root_element();

        let title = self
            .compiled
            .manga_title
            .as_ref()
            .and_then(|r| r.extract(root))
            .unwrap_or_else(|| page_url.to_string());
        let description = self
            .compiled
            .manga_description
            .as_ref()
            .and_then(|r| r.extract(root));
        let cover_url = self
            .compiled
            .manga_cover
            .as_ref()
            .and_then(|r| r.extract(root))
            .and_then(|c| page_url.join(&c).ok())
            .map(|u| u.to_string());

        let chapters_doc = Html::parse_document(chapters_html);
        let mut chapters = Vec::new();
        for (index, item) in chapters_doc.select(&self.compiled.chapter_item).enumerate() {
            let Some(link) = self.compiled.chapter_link.extract(item) else {
                continue;
            };
            let Ok(url) = chapters_url.join(&link) else {
                continue;
            };
            let title = self
                .compiled
                .chapter_title
                .as_ref()
                .and_then(|r| r.extract(item))
                .or_else(|| text_of(item))
                .unwrap_or_else(|| url.to_string());
            let number = self
                .extract_number(&title)
                .or_else(|| self.extract_number(url.as_str()));
            let published_at = self
                .compiled
                .chapter_date
                .as_ref()
                .and_then(|r| r.extract(item))
                .and_then(|text| {
                    crate::dates::parse_chapter_date(
                        &text,
                        self.spec.manga.chapter_date_format.as_deref(),
                        chrono::Utc::now(),
                    )
                });
            chapters.push(ChapterRef {
                key: url.to_string(),
                title,
                number,
                source_order: index as u32,
                scanlator: None,
                published_at,
            });
        }
        // source_order is a recency rank (0 = newest); flip it for sites
        // that list oldest-first so number-less chapters still read in order.
        if self.spec.manga.chapter_order == ChapterOrder::OldestFirst {
            let last = chapters.len().saturating_sub(1) as u32;
            for chapter in &mut chapters {
                chapter.source_order = last - chapter.source_order;
            }
        }
        if chapters.is_empty() {
            return Err(SourceError::Parse(format!(
                "no chapters matched {:?} on {chapters_url}",
                self.spec.manga.chapter_item
            )));
        }

        Ok(MangaDetails {
            summary: MangaSummary {
                key: page_url.to_string(),
                title,
                cover_url,
                in_library: None,
            },
            description,
            chapters,
        })
    }

    pub fn parse_pages(&self, html: &str, page_url: &Url) -> Result<Vec<Url>> {
        let doc = Html::parse_document(html);
        let selector = self
            .compiled
            .page_image
            .selector
            .as_ref()
            .ok_or_else(|| SourceError::Definition("pages.image needs a selector".into()))?;
        let attr = self.compiled.page_image.attr.as_deref().unwrap_or("src");
        let mut urls = Vec::new();
        for el in doc.select(selector) {
            if let Some(value) = el.value().attr(attr).map(str::trim)
                && let Ok(url) = page_url.join(value)
            {
                urls.push(url);
            }
        }
        if urls.is_empty() {
            return Err(SourceError::Parse(format!(
                "no page images matched {:?} on {page_url}",
                self.spec.pages.image
            )));
        }
        Ok(urls)
    }

    fn extract_number(&self, text: &str) -> Option<f64> {
        self.compiled
            .chapter_number
            .captures(text)?
            .get(1)?
            .as_str()
            .parse()
            .ok()
    }

    // ---- fetching ----

    async fn get(&self, url: &Url) -> Result<reqwest::Response> {
        // Politeness: space request *starts* by min_delay, but don't hold the
        // lock across the request itself. The old code slept and sent while
        // holding the mutex, so every fetch ran strictly end-to-end — the N
        // page images of a live-read chapter loaded one at a time, each after
        // the previous fully completed plus the delay. Instead, reserve this
        // request's slot (advancing the next allowed start by min_delay) and
        // release the lock before sending, so requests are rate-limited at the
        // origin yet still overlap in flight. The reserved slot is kept even
        // on transport error: a struggling site must not be retried with zero
        // spacing.
        let min = Duration::from_millis(self.spec.min_delay_ms);
        let wait = {
            let mut next = self.next_slot.lock().await;
            let now = tokio::time::Instant::now();
            let slot = match *next {
                Some(t) => t.max(now),
                None => now,
            };
            *next = Some(slot + min);
            slot.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }

        let resp = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(SourceError::Http(format!("{} on {url}", resp.status())));
        }
        Ok(resp)
    }

    async fn get_html(&self, url: &Url) -> Result<String> {
        self.get(url)
            .await?
            .text()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))
    }

    fn key_url(&self, key: &str) -> Result<Url> {
        let url: Url = key
            .parse()
            .map_err(|_| SourceError::Parse(format!("invalid key {key:?}")))?;
        // Keys are URLs produced by this source; never follow one elsewhere.
        // Full origin (scheme + host + port), not just the host: keys are
        // client input, and "same host, other port" is a different server.
        let base = &self.spec.base_url;
        if url.scheme() != base.scheme()
            || url.host() != base.host()
            || url.port_or_known_default() != base.port_or_known_default()
        {
            return Err(SourceError::Parse(format!(
                "key {key:?} not on this source"
            )));
        }
        Ok(url)
    }
}

#[async_trait::async_trait]
impl Source for SelectorSource {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn name(&self) -> &str {
        &self.spec.name
    }

    fn base_url(&self) -> &Url {
        &self.spec.base_url
    }

    async fn search(&self, query: &str) -> Result<Vec<MangaSummary>> {
        let url = search_url(&self.spec, query)?;
        let html = self.get_html(&url).await?;
        self.parse_search(&html, &url)
    }

    fn browse_sorts(&self) -> Vec<BrowseSort> {
        self.compiled
            .listings
            .iter()
            .map(|(sort, _)| *sort)
            .collect()
    }

    async fn browse(&self, sort: BrowseSort, page: u32) -> Result<Vec<MangaSummary>> {
        let listing = self.listing(sort)?;
        // A template without a pagination placeholder has exactly one page.
        if page > 1 && !listing.url.contains("{page}") && !listing.url.contains("{offset}") {
            return Ok(Vec::new());
        }
        let url = listing_url(&self.spec, listing, page.max(1))?;
        let html = self.get_html(&url).await?;
        self.parse_listing(sort, &html, &url)
    }

    async fn manga(&self, key: &str) -> Result<MangaDetails> {
        let url = self.key_url(key)?;
        let html = self.get_html(&url).await?;
        match &self.spec.manga.chapters_url {
            None => self.parse_manga(&html, &url),
            Some(template) => {
                let chapters_url = entity_url(template, &url)?;
                let chapters_html = self.get_html(&chapters_url).await?;
                self.parse_manga_parts(&html, &chapters_html, &url, &chapters_url)
            }
        }
    }

    async fn pages(&self, chapter_key: &str) -> Result<Vec<Url>> {
        let url = self.key_url(chapter_key)?;
        let url = match &self.spec.pages.url {
            None => url,
            Some(template) => entity_url(template, &url)?,
        };
        let html = self.get_html(&url).await?;
        self.parse_pages(&html, &url)
    }

    async fn image(&self, url: &Url) -> Result<ImageData> {
        if is_private_target(url) {
            return Err(SourceError::Parse(format!(
                "refusing to fetch a private address: {url}"
            )));
        }
        let mut resp = self.get(url).await?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        // Bounded read: a hostile or broken upstream must not OOM the server
        // by streaming an unbounded body (a lying/absent Content-Length can't
        // get past the running total).
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?
        {
            if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                return Err(SourceError::Http(format!(
                    "image exceeds {MAX_IMAGE_BYTES} byte cap: {url}"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(ImageData {
            bytes: bytes.into(),
            content_type,
        })
    }
}

/// Upper bound on a proxied image, so a hostile/broken upstream can't OOM the
/// server. Generous for manga pages (large webtoon strips run a few MiB).
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Whether a URL targets a private/loopback/link-local address — the cheap
/// SSRF guard for proxied image URLs, which come from parsed site HTML and
/// could otherwise aim the server at cloud metadata (169.254.169.254) or LAN
/// hosts. A *hostname* that resolves to a private address (DNS rebinding) is
/// out of scope here; that needs a connection-pinning resolver.
fn is_private_target(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 0
        }
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (ip.segments()[0] & 0xffc0) == 0xfe80 // link-local  fe80::/10
        }
        _ => false,
    }
}

/// Extract the manga cards matched by `item` on a listing/search page.
fn parse_cards(
    html: &str,
    page_url: &Url,
    item: &Selector,
    title: &Option<Rule>,
    link: &Rule,
    cover: &Option<Rule>,
) -> Vec<MangaSummary> {
    let doc = Html::parse_document(html);
    let mut out = Vec::new();
    for card in doc.select(item) {
        let Some(href) = link.extract(card) else {
            continue;
        };
        let Ok(url) = page_url.join(&href) else {
            continue;
        };
        let title = title
            .as_ref()
            .and_then(|r| r.extract(card))
            .or_else(|| text_of(card))
            .unwrap_or_else(|| url.to_string());
        let cover_url = cover
            .as_ref()
            .and_then(|r| r.extract(card))
            .and_then(|c| page_url.join(&c).ok())
            .map(|u| u.to_string());
        out.push(MangaSummary {
            key: url.to_string(),
            title,
            cover_url,
            in_library: None,
        });
    }
    out
}

/// Substitute `{base}`/`{page}`/`{offset}` in a browse listing URL template.
fn listing_url(spec: &SelectorSpec, listing: &CompiledListing, page: u32) -> Result<Url> {
    let offset = (page - 1) * listing.page_size.unwrap_or(0);
    let url_str = listing
        .url
        .replace("{base}", spec.base_url.as_str().trim_end_matches('/'))
        .replace("{page}", &page.to_string())
        .replace("{offset}", &offset.to_string());
    url_str
        .parse()
        .map_err(|_| SourceError::Definition(format!("browse url {url_str:?}")))
}

/// Substitute `{url}`/`{url_parent}` in a fragment-endpoint template
/// (`manga.chapters_url`, `pages.url`).
fn entity_url(template: &str, url: &Url) -> Result<Url> {
    let mut parent = url.clone();
    if let Ok(mut segments) = parent.path_segments_mut() {
        // A trailing slash leaves an empty last segment; popping that only
        // strips the slash, not the real parent — pop again so
        // `{url_parent}` of `.../series/<id>/<slug>/` is `.../series/<id>`.
        if url.path().ends_with('/') {
            segments.pop();
        }
        segments.pop();
    }
    let url_str = template
        .replace("{url}", url.as_str().trim_end_matches('/'))
        .replace("{url_parent}", parent.as_str().trim_end_matches('/'));
    url_str
        .parse()
        .map_err(|_| SourceError::Definition(format!("fragment url {url_str:?}")))
}

/// Substitute `{base}`/`{query}` in a search URL template. The query is
/// percent-encoded (`%20`, never `+`) so templates work in a path segment
/// (`{base}/search/{query}`) as well as in a query string (`?s={query}`).
fn search_url(spec: &SelectorSpec, query: &str) -> Result<Url> {
    let encoded = percent_encoding::utf8_percent_encode(query, percent_encoding::NON_ALPHANUMERIC)
        .to_string();
    let url_str = spec
        .search
        .url
        .replace("{base}", spec.base_url.as_str().trim_end_matches('/'))
        .replace("{query}", &encoded);
    url_str
        .parse()
        .map_err(|_| SourceError::Definition(format!("search url {url_str:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_url_substitutes_url_and_parent() {
        let url = Url::parse("https://site.test/series/ABC123/some-slug").unwrap();
        assert_eq!(
            entity_url("{url_parent}/full-chapter-list", &url)
                .unwrap()
                .as_str(),
            "https://site.test/series/ABC123/full-chapter-list"
        );
        assert_eq!(
            entity_url("{url}/images?page=1", &url).unwrap().as_str(),
            "https://site.test/series/ABC123/some-slug/images?page=1"
        );
    }

    #[test]
    fn entity_url_parent_handles_trailing_slash() {
        // A key that kept its trailing slash must still resolve to the real
        // parent, not one level too deep.
        let url = Url::parse("https://site.test/series/ABC123/some-slug/").unwrap();
        assert_eq!(
            entity_url("{url_parent}/full-chapter-list", &url)
                .unwrap()
                .as_str(),
            "https://site.test/series/ABC123/full-chapter-list"
        );
    }

    #[test]
    fn default_number_regex_reads_manhwa_episode_titles() {
        let compiled = Regex::new(&default_number_regex()).unwrap();
        let num = |t: &str| {
            compiled
                .captures(t)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<f64>().ok())
        };
        assert_eq!(num("Chapter 12"), Some(12.0));
        assert_eq!(num("Ch. 12.5"), Some(12.5));
        assert_eq!(num("Episode 7"), Some(7.0));
        assert_eq!(num("Ep 3"), Some(3.0));
        // A bare, keyword-less title stays unparsed (sorts by listing order).
        assert_eq!(num("Prologue"), None);
    }

    #[test]
    fn listing_url_offset_pagination() {
        let spec: SelectorSpec = toml::from_str(
            r#"
            id = "t"
            name = "T"
            base_url = "https://site.test"
            [search]
            url = "{base}/search?q={query}"
            item = ".card"
            link = "a@href"
            [browse.popular]
            url = "{base}/list?limit=32&offset={offset}"
            page_size = 32
            [manga]
            chapter_item = "li"
            chapter_link = "a@href"
            [pages]
            image = "img@src"
            "#,
        )
        .unwrap();
        let source = SelectorSource::new(spec).unwrap();
        let listing = &source.compiled.listings[0].1;
        assert_eq!(
            listing_url(&source.spec, listing, 1).unwrap().as_str(),
            "https://site.test/list?limit=32&offset=0"
        );
        assert_eq!(
            listing_url(&source.spec, listing, 3).unwrap().as_str(),
            "https://site.test/list?limit=32&offset=64"
        );
    }

    #[test]
    fn private_targets_are_blocked() {
        let blocked = [
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://127.0.0.1/admin",
            "http://192.168.1.10/x",
            "http://10.0.0.5/x",
            "http://[::1]/x",
            "http://[fd00::1]/x",
        ];
        for u in blocked {
            assert!(
                is_private_target(&Url::parse(u).unwrap()),
                "should block {u}"
            );
        }
        let allowed = ["https://cdn.example.com/a.jpg", "http://93.184.216.34/x"];
        for u in allowed {
            assert!(
                !is_private_target(&Url::parse(u).unwrap()),
                "should allow {u}"
            );
        }
    }

    #[test]
    fn offset_without_page_size_is_rejected() {
        let spec: SelectorSpec = toml::from_str(
            r#"
            id = "t"
            name = "T"
            base_url = "https://site.test"
            [search]
            url = "{base}/search?q={query}"
            item = ".card"
            link = "a@href"
            [browse.popular]
            url = "{base}/list?offset={offset}"
            [manga]
            chapter_item = "li"
            chapter_link = "a@href"
            [pages]
            image = "img@src"
            "#,
        )
        .unwrap();
        assert!(matches!(
            SelectorSource::new(spec),
            Err(SourceError::Definition(_))
        ));
    }

    #[test]
    fn chapter_date_selector_parses_absolute_dates() {
        use chrono::TimeZone;

        let spec: SelectorSpec = toml::from_str(
            r#"
            id = "t"
            name = "T"
            base_url = "https://site.test"
            [search]
            url = "{base}/search?q={query}"
            item = ".card"
            link = "a@href"
            [manga]
            chapter_item = "li"
            chapter_link = "a@href"
            chapter_date = ".cdate"
            chapter_date_format = "%Y/%m/%d"
            [pages]
            image = "img@src"
            "#,
        )
        .unwrap();
        let source = SelectorSource::new(spec).unwrap();
        let html = r#"<html><body><ul>
            <li><a href="/c/2">Chapter 2</a><span class="cdate">2026/05/19</span></li>
            <li><a href="/c/1">Chapter 1</a><span class="cdate">not a date</span></li>
        </ul></body></html>"#;
        let url = Url::parse("https://site.test/series/x").unwrap();
        let details = source.parse_manga_parts(html, html, &url, &url).unwrap();

        assert_eq!(
            details.chapters[0].published_at,
            Some(chrono::Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap()),
        );
        // Unparseable text degrades to no date, never an error.
        assert_eq!(details.chapters[1].published_at, None);
    }
}
