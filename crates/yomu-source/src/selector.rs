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
use yomu_domain::{ChapterRef, MangaDetails, MangaSummary};

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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MangaSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    /// Selector matching one chapter entry on the manga page.
    pub chapter_item: String,
    #[serde(default)]
    pub chapter_title: Option<String>,
    /// Relative to chapter item; yields the chapter page URL (chapter key).
    pub chapter_link: String,
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
    r"(?i)ch(?:apter)?\.?\s*(\d+(?:\.\d+)?)".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PagesSpec {
    /// Selector matching every page image on the chapter page.
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

struct CompiledSpec {
    search_item: Selector,
    search_title: Option<Rule>,
    search_link: Rule,
    search_cover: Option<Rule>,
    manga_title: Option<Rule>,
    manga_description: Option<Rule>,
    manga_cover: Option<Rule>,
    chapter_item: Selector,
    chapter_title: Option<Rule>,
    chapter_link: Rule,
    chapter_number: Regex,
    page_image: Rule,
}

pub struct SelectorSource {
    spec: SelectorSpec,
    compiled: CompiledSpec,
    client: reqwest::Client,
    /// Serializes requests and enforces `min_delay_ms` between them.
    throttle: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl SelectorSource {
    pub fn new(spec: SelectorSpec) -> Result<Self> {
        let sel = |raw: &str| {
            Selector::parse(raw)
                .map_err(|e| SourceError::Definition(format!("selector {raw:?}: {e}")))
        };
        let rule_opt = |raw: &Option<String>| raw.as_deref().map(Rule::parse).transpose();

        let compiled = CompiledSpec {
            search_item: sel(&spec.search.item)?,
            search_title: rule_opt(&spec.search.title)?,
            search_link: Rule::parse(&spec.search.link)?,
            search_cover: rule_opt(&spec.search.cover)?,
            manga_title: rule_opt(&spec.manga.title)?,
            manga_description: rule_opt(&spec.manga.description)?,
            manga_cover: rule_opt(&spec.manga.cover)?,
            chapter_item: sel(&spec.manga.chapter_item)?,
            chapter_title: rule_opt(&spec.manga.chapter_title)?,
            chapter_link: Rule::parse(&spec.manga.chapter_link)?,
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
            .build()
            .map_err(|e| SourceError::Http(e.to_string()))?;

        Ok(Self {
            spec,
            compiled,
            client,
            throttle: tokio::sync::Mutex::new(None),
        })
    }

    // ---- pure parsing (unit-tested against fixtures) ----

    pub fn parse_search(&self, html: &str, page_url: &Url) -> Result<Vec<MangaSummary>> {
        let doc = Html::parse_document(html);
        let mut out = Vec::new();
        for item in doc.select(&self.compiled.search_item) {
            let Some(link) = self.compiled.search_link.extract(item) else {
                continue;
            };
            let Ok(url) = page_url.join(&link) else {
                continue;
            };
            let title = self
                .compiled
                .search_title
                .as_ref()
                .and_then(|r| r.extract(item))
                .or_else(|| text_of(item))
                .unwrap_or_else(|| url.to_string());
            let cover_url = self
                .compiled
                .search_cover
                .as_ref()
                .and_then(|r| r.extract(item))
                .and_then(|c| page_url.join(&c).ok());
            out.push(MangaSummary {
                key: url.to_string(),
                title,
                cover_url,
            });
        }
        Ok(out)
    }

    pub fn parse_manga(&self, html: &str, page_url: &Url) -> Result<MangaDetails> {
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
            .and_then(|c| page_url.join(&c).ok());

        let mut chapters = Vec::new();
        for (index, item) in doc.select(&self.compiled.chapter_item).enumerate() {
            let Some(link) = self.compiled.chapter_link.extract(item) else {
                continue;
            };
            let Ok(url) = page_url.join(&link) else {
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
            chapters.push(ChapterRef {
                key: url.to_string(),
                title,
                number,
                source_order: index as u32,
                scanlator: None,
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
                "no chapters matched {:?} on {page_url}",
                self.spec.manga.chapter_item
            )));
        }

        Ok(MangaDetails {
            summary: MangaSummary {
                key: page_url.to_string(),
                title,
                cover_url,
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
        // Politeness: one request at a time per source, spaced by min_delay.
        let mut last = self.throttle.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            let min = Duration::from_millis(self.spec.min_delay_ms);
            if elapsed < min {
                tokio::time::sleep(min - elapsed).await;
            }
        }
        let result = self.client.get(url.clone()).send().await;
        // Stamp even on transport errors: a struggling site must not be
        // retried with zero spacing.
        *last = Some(tokio::time::Instant::now());
        let resp = result.map_err(|e| SourceError::Http(e.to_string()))?;

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

    async fn manga(&self, key: &str) -> Result<MangaDetails> {
        let url = self.key_url(key)?;
        let html = self.get_html(&url).await?;
        self.parse_manga(&html, &url)
    }

    async fn pages(&self, chapter_key: &str) -> Result<Vec<Url>> {
        let url = self.key_url(chapter_key)?;
        let html = self.get_html(&url).await?;
        self.parse_pages(&html, &url)
    }

    async fn image(&self, url: &Url) -> Result<ImageData> {
        let resp = self.get(url).await?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        Ok(ImageData {
            bytes,
            content_type,
        })
    }
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
