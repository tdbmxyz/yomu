//! Local source, à la Suwayomi: series that already live on the server's
//! disk, no site involved. Layout under the configured directory:
//!
//! ```text
//! local/
//!   Solo Farming in the Tower/
//!     cover.jpg                (optional; else first page of first chapter)
//!     details.json             (optional: {"title": ..., "description": ...})
//!     Chapter 1/
//!       001.png 002.png ...
//!     Chapter 2.cbz            (zip archive of images)
//! ```
//!
//! Keys are directory-relative paths (`"Series"`, `"Series/Chapter 1"`),
//! opaque to the rest of yomu like any source key. Page/cover URLs use the
//! `local:` scheme and are only ever resolved back by this source, which
//! confines them to the local directory.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use url::Url;
use yomu_domain::{ChapterRef, MangaDetails, MangaSummary};

use crate::{ImageData, Result, Source, SourceError};

const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "gif", "avif"];
const COVER_STEMS: [&str; 2] = ["cover", "folder"];

/// Optional per-series metadata, Suwayomi-compatible subset.
#[derive(Debug, Default, Deserialize)]
struct Details {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

pub struct LocalSource {
    id: String,
    name: String,
    dir: PathBuf,
    base: Url,
}

impl LocalSource {
    pub fn new(id: impl Into<String>, name: impl Into<String>, dir: PathBuf) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            dir,
            base: Url::parse("local:///").expect("valid local base url"),
        }
    }

    /// Resolve a directory-relative key, refusing anything that could
    /// escape the local directory. The path must already exist.
    fn resolve(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty()
            || Path::new(key).components().any(|c| {
                !matches!(c, std::path::Component::Normal(part)
                          if !part.to_string_lossy().starts_with('.'))
            })
        {
            return Err(SourceError::Parse(format!("invalid local key {key:?}")));
        }
        let path = self.dir.join(key);
        if !path.exists() {
            return Err(SourceError::Parse(format!("local key {key:?} not found")));
        }
        Ok(path)
    }

    /// `local:` URL addressing a file (or `.cbz` entry) under the local dir.
    fn local_url(&self, relative: &str, entry: Option<&str>) -> Url {
        let mut url = self.base.clone();
        {
            let mut segments = url.path_segments_mut().expect("local base is not opaque");
            segments.clear();
            for part in relative.split('/') {
                segments.push(part);
            }
        }
        if let Some(entry) = entry {
            url.query_pairs_mut().append_pair("entry", entry);
        }
        url
    }

    /// Series directories matching `query` (empty query lists everything).
    async fn list_series(&self, query: &str) -> Result<Vec<String>> {
        let needle = query.to_lowercase();
        let mut names = Vec::new();
        let mut reader = match tokio::fs::read_dir(&self.dir).await {
            Ok(reader) => reader,
            // Missing dir = no local series yet, not an error.
            Err(_) => return Ok(names),
        };
        while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                && name.to_lowercase().contains(&needle)
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    async fn series_details(&self, series: &str) -> Result<MangaDetails> {
        let series_dir = self.resolve(series)?;

        let details: Details =
            match tokio::fs::read_to_string(series_dir.join("details.json")).await {
                Ok(raw) => serde_json::from_str(&raw)
                    .map_err(|e| SourceError::Definition(format!("{series}/details.json: {e}")))?,
                Err(_) => Details::default(),
            };

        // Chapters: subdirectories and .cbz archives, in reading order
        // (parsed number, name as fallback), source_order = recency rank.
        let mut chapters = Vec::new();
        let mut reader = tokio::fs::read_dir(&series_dir).await.map_err(io_err)?;
        while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            let is_cbz = !is_dir && name.to_lowercase().ends_with(".cbz");
            if !is_dir && !is_cbz {
                continue;
            }
            let title = name.trim_end_matches(".cbz").trim_end_matches(".CBZ");
            chapters.push(ChapterRef {
                key: format!("{series}/{name}"),
                title: title.to_string(),
                number: chapter_number(title),
                source_order: 0, // recency rank, assigned after sorting
                scanlator: None,
            });
        }
        if chapters.is_empty() {
            return Err(SourceError::Parse(format!(
                "no chapters (subdirectories or .cbz) in {series:?}"
            )));
        }
        chapters.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.title.cmp(&b.title),
        });
        let last = chapters.len() as u32 - 1;
        for (index, chapter) in chapters.iter_mut().enumerate() {
            chapter.source_order = last - index as u32;
        }

        let cover_url = self.find_cover(series, &series_dir, &chapters).await;

        Ok(MangaDetails {
            summary: MangaSummary {
                key: series.to_string(),
                title: details.title.unwrap_or_else(|| series.to_string()),
                cover_url,
            },
            description: details.description,
            chapters,
        })
    }

    /// `cover.<ext>` in the series directory, else the first page of the
    /// first chapter.
    async fn find_cover(
        &self,
        series: &str,
        series_dir: &Path,
        chapters: &[ChapterRef],
    ) -> Option<Url> {
        for stem in COVER_STEMS {
            for ext in IMAGE_EXTENSIONS {
                let name = format!("{stem}.{ext}");
                if tokio::fs::try_exists(series_dir.join(&name))
                    .await
                    .unwrap_or(false)
                {
                    return Some(self.local_url(&format!("{series}/{name}"), None));
                }
            }
        }
        let first = chapters.first()?;
        self.chapter_pages(&first.key)
            .await
            .ok()?
            .into_iter()
            .next()
    }

    /// Page image URLs of one chapter (directory of images or .cbz).
    async fn chapter_pages(&self, chapter_key: &str) -> Result<Vec<Url>> {
        let path = self.resolve(chapter_key)?;

        if path.is_dir() {
            let mut names = Vec::new();
            let mut reader = tokio::fs::read_dir(&path).await.map_err(io_err)?;
            while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_image_name(&name) {
                    names.push(name);
                }
            }
            sort_pages(&mut names);
            if names.is_empty() {
                return Err(SourceError::Parse(format!(
                    "no page images in {chapter_key:?}"
                )));
            }
            return Ok(names
                .iter()
                .map(|name| self.local_url(&format!("{chapter_key}/{name}"), None))
                .collect());
        }

        // .cbz: list image entries off the async runtime.
        let mut names = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let file = std::fs::File::open(&path).map_err(io_err)?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| SourceError::Parse(format!("not a readable cbz: {e}")))?;
            let mut names = Vec::new();
            for i in 0..archive.len() {
                let entry = archive
                    .by_index(i)
                    .map_err(|e| SourceError::Parse(format!("cbz entry {i}: {e}")))?;
                if entry.is_file() && is_image_name(entry.name()) {
                    names.push(entry.name().to_string());
                }
            }
            Ok(names)
        })
        .await
        .map_err(|e| SourceError::Http(format!("cbz task: {e}")))??;

        sort_pages(&mut names);
        if names.is_empty() {
            return Err(SourceError::Parse(format!(
                "no page images in {chapter_key:?}"
            )));
        }
        Ok(names
            .iter()
            .map(|entry| self.local_url(chapter_key, Some(entry)))
            .collect())
    }

    /// Resolve a `local:` URL produced by this source back to file bytes.
    async fn read_local(&self, url: &Url) -> Result<ImageData> {
        if url.scheme() != "local" {
            return Err(SourceError::Parse(format!("not a local url: {url}")));
        }
        let relative = url
            .path_segments()
            .map(|segments| {
                segments
                    .map(|s| {
                        percent_encoding::percent_decode_str(s)
                            .decode_utf8_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default();
        let path = self.resolve(&relative)?;
        let entry = url
            .query_pairs()
            .find(|(k, _)| k == "entry")
            .map(|(_, v)| v.into_owned());

        match entry {
            None => {
                let bytes = tokio::fs::read(&path).await.map_err(io_err)?;
                Ok(ImageData {
                    bytes: bytes.into(),
                    content_type: content_type_of(&relative).to_string(),
                })
            }
            Some(entry) => {
                let content_type = content_type_of(&entry).to_string();
                let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    use std::io::Read;
                    let file = std::fs::File::open(&path).map_err(io_err)?;
                    let mut archive = zip::ZipArchive::new(file)
                        .map_err(|e| SourceError::Parse(format!("not a readable cbz: {e}")))?;
                    let mut zipped = archive
                        .by_name(&entry)
                        .map_err(|e| SourceError::Parse(format!("cbz entry {entry:?}: {e}")))?;
                    let mut bytes = Vec::new();
                    zipped.read_to_end(&mut bytes).map_err(io_err)?;
                    Ok(bytes)
                })
                .await
                .map_err(|e| SourceError::Http(format!("cbz task: {e}")))??;
                Ok(ImageData {
                    bytes: bytes.into(),
                    content_type,
                })
            }
        }
    }
}

#[async_trait::async_trait]
impl Source for LocalSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &Url {
        &self.base
    }

    async fn search(&self, query: &str) -> Result<Vec<MangaSummary>> {
        let mut out = Vec::new();
        for series in self.list_series(query).await? {
            // Cheap summary: title = dir name, cover resolved lazily via
            // the full details (avoids reading every series on search).
            out.push(MangaSummary {
                key: series.clone(),
                title: series,
                cover_url: None,
            });
        }
        Ok(out)
    }

    async fn manga(&self, key: &str) -> Result<MangaDetails> {
        self.series_details(key).await
    }

    async fn pages(&self, chapter_key: &str) -> Result<Vec<Url>> {
        self.chapter_pages(chapter_key).await
    }

    async fn image(&self, url: &Url) -> Result<ImageData> {
        self.read_local(url).await
    }
}

fn io_err(e: std::io::Error) -> SourceError {
    SourceError::Http(e.to_string())
}

fn is_image_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    !name.starts_with('.')
        && IMAGE_EXTENSIONS
            .iter()
            .any(|ext| lower.ends_with(&format!(".{ext}")))
}

fn content_type_of(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    match lower.rsplit_once('.').map(|(_, e)| e) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        _ => "image/jpeg",
    }
}

/// Chapter number from a directory/archive name: "Chapter 12.5", "ch. 3",
/// or just the first number ("0042 - The Tower").
fn chapter_number(title: &str) -> Option<f64> {
    static CHAPTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)ch(?:apter)?\.?\s*(\d+(?:\.\d+)?)").expect("valid regex")
    });
    static NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)").expect("valid regex"));
    let capture = CHAPTER.captures(title).or_else(|| NUMBER.captures(title))?;
    capture.get(1)?.as_str().parse().ok()
}

/// Reading order for page file names: numeric when they contain numbers
/// ("2.png" before "10.png"), lexicographic otherwise.
fn sort_pages(names: &mut [String]) {
    fn first_int(name: &str) -> Option<u64> {
        static NUMBER: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(\d+)").expect("valid regex"));
        NUMBER.captures(name)?.get(1)?.as_str().parse().ok()
    }
    names.sort_by(|a, b| match (first_int(a), first_int(b)) {
        (Some(x), Some(y)) if x != y => x.cmp(&y),
        _ => a.cmp(b),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_numbers_from_names() {
        assert_eq!(chapter_number("Chapter 12.5"), Some(12.5));
        assert_eq!(chapter_number("ch3"), Some(3.0));
        assert_eq!(chapter_number("0042 - The Tower"), Some(42.0));
        assert_eq!(chapter_number("Epilogue"), None);
    }

    #[test]
    fn pages_sort_numerically() {
        let mut names = vec!["10.png".into(), "2.png".into(), "1.png".into()];
        sort_pages(&mut names);
        assert_eq!(names, ["1.png", "2.png", "10.png"]);
    }

    #[tokio::test]
    async fn keys_cannot_escape_the_local_dir() {
        let source = LocalSource::new("local", "Local", std::env::temp_dir());
        for key in ["../etc", "/etc/passwd", "a/../../b", ".hidden", ""] {
            assert!(source.resolve(key).is_err(), "key {key:?} must be rejected");
        }
    }

    #[tokio::test]
    async fn series_chapters_and_pages_from_disk() {
        let root = std::env::temp_dir().join(format!("yomu-local-test-{}", std::process::id()));
        let series = root.join("Solo Farming");
        std::fs::create_dir_all(series.join("Chapter 2")).unwrap();
        std::fs::create_dir_all(series.join("Chapter 1")).unwrap();
        for name in ["2.png", "10.png", "1.png"] {
            std::fs::write(series.join("Chapter 1").join(name), b"png").unwrap();
        }
        std::fs::write(series.join("Chapter 2").join("001.jpg"), b"jpg").unwrap();
        std::fs::write(series.join("cover.png"), b"png").unwrap();

        let source = LocalSource::new("local", "Local", root.clone());

        let hits = source.search("solo").await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "Solo Farming");
        assert!(source.search("nope").await.unwrap().is_empty());

        let details = source.manga("Solo Farming").await.unwrap();
        assert_eq!(details.summary.title, "Solo Farming");
        assert_eq!(details.chapters.len(), 2);
        // Reading order with recency-rank source_order.
        assert_eq!(details.chapters[0].number, Some(1.0));
        assert_eq!(details.chapters[0].source_order, 1);
        assert_eq!(details.chapters[1].source_order, 0);
        let cover = details.summary.cover_url.clone().unwrap();
        assert_eq!(cover.scheme(), "local");

        let pages = source.pages("Solo Farming/Chapter 1").await.unwrap();
        assert_eq!(pages.len(), 3);
        let image = source.image(&pages[0]).await.unwrap();
        assert_eq!(image.content_type, "image/png");
        assert_eq!(&image.bytes[..], b"png");

        let cover_image = source.image(&cover).await.unwrap();
        assert_eq!(cover_image.content_type, "image/png");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
