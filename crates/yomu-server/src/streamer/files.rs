//! File resolution for the streamer: CBZ archives and image directories
//! under the books dir, addressed by dir-relative keys and `local:` URLs.
//! Moved from the retired built-in local source; the `local:` URL scheme is
//! kept verbatim so cover/page URLs stored by 1.x keep resolving.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use url::Url;
use yomu_domain::{ChapterRef, MangaDetails, MangaSummary};
use yomu_source::{ImageData, SourceError};

pub type Result<T> = std::result::Result<T, SourceError>;

const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "gif", "avif"];
const COVER_STEMS: [&str; 2] = ["cover", "folder"];

/// Publication formats the streamer cannot open. Counted per publication
/// rather than passed over in silence: nine `.cbr` volumes sitting between
/// `.cbz` ones used to go missing from a series without a trace.
const UNSUPPORTED_EXTENSIONS: [&str; 8] =
    ["cbr", "cb7", "cbt", "rar", "pdf", "epub", "mobi", "azw3"];

/// Files one scan skipped because it cannot read their format.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Unsupported {
    pub count: u32,
    /// Lowercased extensions, sorted and deduplicated.
    pub formats: Vec<String>,
}

impl Unsupported {
    /// Count `name` when it looks like a book in a format we cannot read.
    /// Everything else a library folder collects — `.nfo` sidecars, `.xml`,
    /// scanner leftovers — is not a missing volume and must not be counted
    /// as one, or every folder would claim to be incomplete.
    fn record(&mut self, name: &str) {
        let Some((_, ext)) = name.rsplit_once('.') else {
            return;
        };
        let ext = ext.to_lowercase();
        if !UNSUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            return;
        }
        self.count += 1;
        if let Err(at) = self.formats.binary_search(&ext) {
            self.formats.insert(at, ext);
        }
    }

    fn merge(&mut self, other: &Unsupported) {
        self.count += other.count;
        for ext in &other.formats {
            if let Err(at) = self.formats.binary_search(ext) {
                self.formats.insert(at, ext.clone());
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Optional per-series metadata, Suwayomi-compatible subset.
#[derive(Debug, Default, Deserialize)]
struct Details {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
}

/// Serves and inspects the books dir. Shared behind `Arc` in `AppState`.
pub struct Streamer {
    pub books_dir: PathBuf,
    base: Url,
}

impl Streamer {
    pub fn new(books_dir: PathBuf) -> Self {
        Self {
            books_dir,
            base: Url::parse("local:///").expect("valid local base url"),
        }
    }

    /// Resolve a directory-relative key, refusing anything that could
    /// escape the books dir. The path must already exist.
    fn resolve(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty()
            || Path::new(key).components().any(|c| {
                !matches!(c, std::path::Component::Normal(part)
                          if !part.to_string_lossy().starts_with('.'))
            })
        {
            return Err(SourceError::Parse(format!("invalid local key {key:?}")));
        }
        let path = self.books_dir.join(key);
        if !path.exists() {
            return Err(SourceError::Parse(format!("local key {key:?} not found")));
        }
        // Lexical checks stop `..`, but a symlink under the dir can still point
        // outside it. Canonicalize and confirm the real path stays within the
        // (canonical) books dir before handing it back.
        let canon_dir = self
            .books_dir
            .canonicalize()
            .map_err(|e| SourceError::Parse(format!("local dir not resolvable: {e}")))?;
        let canon = path
            .canonicalize()
            .map_err(|e| SourceError::Parse(format!("local key {key:?} not resolvable: {e}")))?;
        if !canon.starts_with(&canon_dir) {
            return Err(SourceError::Parse(format!(
                "local key {key:?} escapes the local dir"
            )));
        }
        Ok(canon)
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

    /// Metadata for a series folder, given the unit names `discover_entry`
    /// already decided are readable. `units` may be empty: a folder holding
    /// only formats we cannot read still becomes a publication, so the
    /// library can show it as unsupported instead of hiding it.
    pub(super) async fn series_details(
        &self,
        series: &str,
        units: &[String],
    ) -> Result<MangaDetails> {
        let series_dir = self.resolve(series)?;

        let details: Details =
            match tokio::fs::read_to_string(series_dir.join("details.json")).await {
                Ok(raw) => serde_json::from_str(&raw)
                    .map_err(|e| SourceError::Definition(format!("{series}/details.json: {e}")))?,
                Err(_) => Details::default(),
            };

        // Units in reading order (parsed number, name as fallback);
        // source_order is the recency rank, assigned after sorting.
        let mut chapters: Vec<ChapterRef> = units
            .iter()
            .map(|name| {
                let title = name.trim_end_matches(".cbz").trim_end_matches(".CBZ");
                ChapterRef {
                    key: format!("{series}/{name}"),
                    title: title.to_string(),
                    number: chapter_number(title),
                    source_order: 0,
                    scanlator: None,
                    published_at: None,
                }
            })
            .collect();
        chapters.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.title.cmp(&b.title),
        });
        // saturating: an unsupported-only folder has no units at all.
        let last = (chapters.len() as u32).saturating_sub(1);
        for (index, chapter) in chapters.iter_mut().enumerate() {
            chapter.source_order = last - index as u32;
        }

        let cover_url = self
            .find_cover(series, &series_dir, &chapters)
            .await
            .map(|u| u.to_string());

        Ok(MangaDetails {
            summary: MangaSummary {
                key: series.to_string(),
                title: details.title.unwrap_or_else(|| series.to_string()),
                cover_url,
                in_library: None,
            },
            description: details.description,
            genres: details.genres,
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
        self.pages(&first.key).await.ok()?.into_iter().next()
    }

    /// Page image URLs of one chapter (directory of images or .cbz).
    pub async fn pages(&self, unit_key: &str) -> Result<Vec<Url>> {
        let path = self.resolve(unit_key)?;

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
                    "no page images in {unit_key:?}"
                )));
            }
            return Ok(names
                .iter()
                .map(|name| self.local_url(&format!("{unit_key}/{name}"), None))
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
                "no page images in {unit_key:?}"
            )));
        }
        Ok(names
            .iter()
            .map(|entry| self.local_url(unit_key, Some(entry)))
            .collect())
    }

    /// Resolve a `local:` URL produced by this streamer back to file bytes.
    pub async fn image(&self, url: &Url) -> Result<ImageData> {
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

/// One publication found on disk, ready to upsert.
pub(super) struct Discovered {
    /// Books-dir-relative path — the publication's identity.
    pub path: String,
    pub details: MangaDetails,
    /// Files in this folder the scan could not read.
    pub unsupported: Unsupported,
}

impl Streamer {
    /// Walk the books dir top level. Series directories (holding unit
    /// dirs / .cbz) become multi-unit publications; root-level .cbz files
    /// and loose image directories become single-unit ones. Anything else
    /// is skipped with one info line — the folder will legitimately hold
    /// future-format files (.epub, .pdf, .cbr).
    pub(super) async fn discover(&self) -> Vec<Discovered> {
        let mut out = Vec::new();
        let mut reader = match tokio::fs::read_dir(&self.books_dir).await {
            Ok(reader) => reader,
            // Missing dir = empty library, not an error.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return out,
            Err(err) => {
                tracing::warn!(%err, "streamer: cannot read books dir");
                return out;
            }
        };
        loop {
            let entry = match reader.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(%err, "streamer: directory walk aborted early");
                    break;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "details.json" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            match self.discover_entry(&name, is_dir).await {
                Ok(Some(found)) => out.push(found),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(entry = %name, %err, "streamer: skipping unreadable entry");
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    async fn discover_entry(&self, name: &str, is_dir: bool) -> Result<Option<Discovered>> {
        if !is_dir {
            if !name.to_lowercase().ends_with(".cbz") {
                tracing::info!(file = %name, "streamer: unsupported file type, skipping");
                return Ok(None);
            }
            // Root-level archive: single-unit publication. Probing the page
            // list up front surfaces corrupt archives at scan time; the
            // first page doubles as the cover.
            let pages = self.pages(name).await?;
            let title = name
                .trim_end_matches(".cbz")
                .trim_end_matches(".CBZ")
                .to_string();
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: single_unit_details(name, &title, pages.first().cloned()),
                unsupported: Unsupported::default(),
            }));
        }

        // A directory is a series when it holds readable unit dirs or
        // archives; a directory of loose page images is a single-unit
        // publication; one holding only unreadable formats becomes a
        // publication with no units at all, flagged for the library.
        let dir = self.books_dir.join(name);
        let mut candidates: Vec<(String, bool)> = Vec::new();
        let mut has_pages = false;
        let mut unsupported = Unsupported::default();
        let mut reader = tokio::fs::read_dir(&dir).await.map_err(io_err)?;
        while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
            let child = entry.file_name().to_string_lossy().into_owned();
            if child.starts_with('.') {
                continue;
            }
            let child_is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if child_is_dir || child.to_lowercase().ends_with(".cbz") {
                candidates.push((child, child_is_dir));
            } else if is_image_name(&child) {
                // Series art is not a page: a folder holding nothing but its
                // cover is not a one-page publication.
                has_pages |= !is_cover_name(&child);
            } else {
                unsupported.record(&child);
            }
        }

        // A subdirectory is a unit only if it actually holds pages. One full
        // of .pdf volumes used to import as a chapter that 404s on every page.
        let mut units = Vec::new();
        for (child, child_is_dir) in candidates {
            if !child_is_dir {
                units.push(child);
                continue;
            }
            match self.unit_dir_contents(&dir.join(&child)).await {
                Ok(contents) if contents.has_pages => units.push(child),
                Ok(contents) => unsupported.merge(&contents.unsupported),
                Err(err) => {
                    tracing::warn!(dir = %child, %err, "streamer: unit dir unreadable");
                }
            }
        }

        if !units.is_empty() {
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: self.series_details(name, &units).await?,
                unsupported,
            }));
        }
        if has_pages {
            let pages = self.pages(name).await?;
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: single_unit_details(name, name, pages.first().cloned()),
                unsupported,
            }));
        }
        if !unsupported.is_empty() {
            // Nothing readable, but these are books. Import the folder with
            // no units so the library can say why it is empty, instead of
            // dropping it and leaving the reader to wonder where it went.
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: self.series_details(name, &[]).await?,
                unsupported,
            }));
        }
        tracing::info!(dir = %name, "streamer: no readable content, skipping");
        Ok(None)
    }

    /// One level of a candidate unit directory: whether it holds any page
    /// image, and what unreadable book files it carries otherwise.
    async fn unit_dir_contents(&self, path: &Path) -> Result<UnitDirContents> {
        let mut contents = UnitDirContents::default();
        let mut reader = tokio::fs::read_dir(path).await.map_err(io_err)?;
        while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
            let child = entry.file_name().to_string_lossy().into_owned();
            if child.starts_with('.') {
                continue;
            }
            if is_image_name(&child) {
                contents.has_pages = true;
            } else {
                contents.unsupported.record(&child);
            }
        }
        Ok(contents)
    }
}

#[derive(Default)]
struct UnitDirContents {
    has_pages: bool,
    unsupported: Unsupported,
}

/// A one-shot: the publication and its only unit share the path as key.
/// The cover is the first page, mirroring `find_cover`'s fallback.
fn single_unit_details(path: &str, title: &str, cover: Option<Url>) -> MangaDetails {
    MangaDetails {
        summary: MangaSummary {
            key: path.to_string(),
            title: title.to_string(),
            cover_url: cover.map(|u| u.to_string()),
            in_library: None,
        },
        description: None,
        genres: Vec::new(),
        chapters: vec![ChapterRef {
            key: path.to_string(),
            title: title.to_string(),
            number: chapter_number(title),
            source_order: 0,
            scanlator: None,
            published_at: None,
        }],
    }
}

fn io_err(e: std::io::Error) -> SourceError {
    SourceError::Http(e.to_string())
}

/// `cover.jpg` / `folder.png`: the folder's own art rather than a page.
fn is_cover_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    COVER_STEMS.iter().any(|stem| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|ext| lower == format!("{stem}.{ext}"))
    })
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
        let streamer = Streamer::new(std::env::temp_dir());
        for key in ["../etc", "/etc/passwd", "a/../../b", ".hidden", ""] {
            assert!(
                streamer.resolve(key).is_err(),
                "key {key:?} must be rejected"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_keys_cannot_escape_the_local_dir() {
        let root =
            std::env::temp_dir().join(format!("yomu-streamer-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside =
            std::env::temp_dir().join(format!("yomu-streamer-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"x").unwrap();
        // A symlink inside the books dir aimed at content outside it: the
        // lexical check passes (no `..`), so only canonicalization stops it.
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

        let streamer = Streamer::new(root.clone());
        assert!(streamer.resolve("escape/secret").is_err());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn series_chapters_and_pages_from_disk() {
        let root = std::env::temp_dir().join(format!("yomu-streamer-test-{}", std::process::id()));
        let series = root.join("Solo Farming");
        std::fs::create_dir_all(series.join("Chapter 2")).unwrap();
        std::fs::create_dir_all(series.join("Chapter 1")).unwrap();
        for name in ["2.png", "10.png", "1.png"] {
            std::fs::write(series.join("Chapter 1").join(name), b"png").unwrap();
        }
        std::fs::write(series.join("Chapter 2").join("001.jpg"), b"jpg").unwrap();
        std::fs::write(series.join("cover.png"), b"png").unwrap();

        let streamer = Streamer::new(root.clone());

        let units = ["Chapter 2".to_string(), "Chapter 1".to_string()];
        let details = streamer
            .series_details("Solo Farming", &units)
            .await
            .unwrap();
        assert_eq!(details.summary.title, "Solo Farming");
        assert_eq!(details.chapters.len(), 2);
        // Reading order with recency-rank source_order.
        assert_eq!(details.chapters[0].number, Some(1.0));
        assert_eq!(details.chapters[0].source_order, 1);
        assert_eq!(details.chapters[1].source_order, 0);
        let cover: Url = details.summary.cover_url.clone().unwrap().parse().unwrap();
        assert_eq!(cover.scheme(), "local");

        let pages = streamer.pages("Solo Farming/Chapter 1").await.unwrap();
        assert_eq!(pages.len(), 3);
        let image = streamer.image(&pages[0]).await.unwrap();
        assert_eq!(image.content_type, "image/png");
        assert_eq!(&image.bytes[..], b"png");

        let cover_image = streamer.image(&cover).await.unwrap();
        assert_eq!(cover_image.content_type, "image/png");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
