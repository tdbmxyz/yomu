use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use roxmltree::{Document, Node};
use yomu_domain::{ChapterRef, MangaDetails, MangaSummary, PublicationLink};
use yomu_source::SourceError;

use super::Result;

const METADATA_LIMIT: u64 = 8 * 1024 * 1024;
const RESOURCE_LIMIT: u64 = 64 * 1024 * 1024;

pub(super) struct Epub {
    pub details: MangaDetails,
    pub cover_entry: Option<String>,
    pub resources: Vec<PublicationLink>,
}

pub(super) struct Resource {
    pub bytes: Vec<u8>,
    pub media_type: String,
}

pub(super) fn inspect(path: &Path, key: &str) -> Result<Epub> {
    let file = std::fs::File::open(path).map_err(super::io_err)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| SourceError::Parse(format!("not a readable epub: {e}")))?;
    // Encrypted/obfuscated resources need a dedicated content-protection
    // service. Do not import an apparently readable, partially broken book.
    if zip.by_name("META-INF/encryption.xml").is_ok() {
        return Err(SourceError::Parse(
            "encrypted or obfuscated EPUB resources are not supported".into(),
        ));
    }
    let (package_path, package) = read_package(&mut zip)?;
    let package = std::str::from_utf8(&package)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB package XML: {e}")))?;
    let package = Document::parse(package)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB package XML: {e}")))?;
    let package_dir = Path::new(&package_path).parent().unwrap_or(Path::new(""));
    if package.descendants().any(|node| {
        (local_name(node) == "meta"
            && node.attribute("property") == Some("rendition:layout")
            && node
                .text()
                .is_some_and(|value| value.trim() == "pre-paginated"))
            || node.attribute("properties").is_some_and(|properties| {
                properties
                    .split_whitespace()
                    .any(|property| property == "rendition:layout-pre-paginated")
            })
    }) {
        return Err(SourceError::Parse(
            "fixed-layout EPUB is not supported by the reflowable navigator".into(),
        ));
    }

    let title = first_text(&package, "title")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| file_stem(key));
    let description = first_text(&package, "description").filter(|s| !s.is_empty());
    let genres = package
        .descendants()
        .filter(|n| local_name(*n) == "subject")
        .filter_map(|n| n.text().map(str::trim).filter(|s| !s.is_empty()))
        .map(str::to_string)
        .collect();

    #[derive(Clone)]
    struct Item {
        href: String,
        media_type: String,
        properties: String,
    }
    let manifest: HashMap<String, Item> = package
        .descendants()
        .filter(|n| local_name(*n) == "item")
        .filter_map(|n| {
            Some((
                n.attribute("id")?.to_string(),
                Item {
                    href: archive_join(package_dir, n.attribute("href")?).ok()?,
                    media_type: n.attribute("media-type").unwrap_or("").to_string(),
                    properties: n.attribute("properties").unwrap_or("").to_string(),
                },
            ))
        })
        .collect();

    // EPUB 3 cover-image first, EPUB 2's <meta name="cover" content="id"> fallback.
    let legacy_cover_id = package
        .descendants()
        .find(|n| {
            local_name(*n) == "meta"
                && n.attribute("name")
                    .is_some_and(|v| v.eq_ignore_ascii_case("cover"))
        })
        .and_then(|n| n.attribute("content"));
    let cover_entry = manifest
        .iter()
        .find(|(_, item)| {
            item.properties
                .split_whitespace()
                .any(|p| p == "cover-image")
        })
        .or_else(|| legacy_cover_id.and_then(|id| manifest.get_key_value(id)))
        .filter(|(_, item)| {
            matches!(
                item.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/avif"
            )
        })
        .map(|(_, item)| item.href.clone());

    // EPUB 3 navigation labels first, with EPUB 2 NCX as a fallback. Resolve
    // links against the navigation document, which may live beside the OPF
    // rather than in the same directory.
    let mut labels = manifest
        .values()
        .find(|item| item.media_type == "application/x-dtbncx+xml")
        .and_then(|item| {
            let bytes = read_zip_entry(&mut zip, &item.href, METADATA_LIMIT).ok()?;
            let xml = String::from_utf8(bytes).ok()?;
            ncx_labels(
                &xml,
                Path::new(&item.href).parent().unwrap_or(Path::new("")),
            )
            .ok()
        })
        .unwrap_or_default();
    if let Some(nav) = manifest
        .values()
        .find(|item| item.properties.split_whitespace().any(|p| p == "nav"))
        && let Ok(bytes) = read_zip_entry(&mut zip, &nav.href, METADATA_LIMIT)
        && let Ok(xml) = String::from_utf8(bytes)
        && let Ok(nav_labels) =
            navigation_labels(&xml, Path::new(&nav.href).parent().unwrap_or(Path::new("")))
    {
        labels.extend(nav_labels);
    }

    let spine_ids: Vec<&str> = package
        .descendants()
        .filter(|n| local_name(*n) == "itemref")
        .filter(|n| n.attribute("linear") != Some("no"))
        .filter_map(|n| n.attribute("idref"))
        .collect();
    let last = (spine_ids.len() as u32).saturating_sub(1);
    let chapters = spine_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let item = manifest.get(*id).ok_or_else(|| {
                SourceError::Parse(format!("EPUB spine item {id:?} is missing or external"))
            })?;
            if !matches!(
                item.media_type.as_str(),
                "application/xhtml+xml" | "text/html"
            ) {
                return Err(SourceError::Parse(format!(
                    "unsupported EPUB spine media type {:?}",
                    item.media_type
                )));
            }
            let title = labels
                .get(&item.href)
                .cloned()
                .or_else(|| {
                    let bytes = read_zip_entry(&mut zip, &item.href, METADATA_LIMIT).ok()?;
                    let html = String::from_utf8(bytes).ok()?;
                    let doc = Document::parse(&html).ok()?;
                    first_text(&doc, "title")
                })
                .unwrap_or_else(|| resource_title(&item.href));
            Ok(ChapterRef {
                key: item.href.clone(),
                title,
                number: None,
                source_order: last.saturating_sub(index as u32),
                scanlator: None,
                published_at: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if chapters.is_empty() {
        return Err(SourceError::Parse("EPUB reading order is empty".into()));
    }

    let mut resources: Vec<_> = manifest
        .values()
        .filter(|item| !chapters.iter().any(|chapter| chapter.key == item.href))
        .map(|item| PublicationLink {
            href: item.href.clone(),
            media_type: item.media_type.clone(),
            title: None,
            rel: (cover_entry.as_ref() == Some(&item.href)).then(|| "cover".into()),
            unit_id: None,
        })
        .collect();
    resources.sort_by(|a, b| a.href.cmp(&b.href));
    Ok(Epub {
        details: MangaDetails {
            summary: MangaSummary {
                key: key.to_string(),
                title,
                cover_url: None,
                in_library: None,
            },
            description,
            genres,
            chapters,
        },
        cover_entry,
        resources,
    })
}

pub(super) fn resource(path: &Path, entry: &str) -> Result<Resource> {
    let entry = clean_archive_path(entry)?;
    let file = std::fs::File::open(path).map_err(super::io_err)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| SourceError::Parse(format!("not a readable epub: {e}")))?;
    let (package_path, package) = read_package(&mut zip)?;
    let package = std::str::from_utf8(&package)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB package XML: {e}")))?;
    let package = Document::parse(package)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB package XML: {e}")))?;
    let package_dir = Path::new(&package_path).parent().unwrap_or(Path::new(""));
    // The OPF declaration is authoritative. Valid EPUB resources need not
    // have an extension, and serving an extensionless spine item as an
    // octet-stream would bypass navigator injection and make it unreadable.
    let media_type = package
        .descendants()
        .filter(|node| local_name(*node) == "item")
        .find_map(|node| {
            let href = archive_join(package_dir, node.attribute("href")?).ok()?;
            (href == entry).then(|| node.attribute("media-type").unwrap_or("").to_string())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| super::resource_content_type(&entry).to_string());
    Ok(Resource {
        bytes: read_zip_entry(&mut zip, &entry, RESOURCE_LIMIT)?,
        media_type,
    })
}

fn read_package(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<(String, Vec<u8>)> {
    let container = read_zip_entry(zip, "META-INF/container.xml", METADATA_LIMIT)?;
    let container = std::str::from_utf8(&container)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB container XML: {e}")))?;
    let container = Document::parse(container)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB container XML: {e}")))?;
    let package_path = container
        .descendants()
        .find(|node| local_name(*node) == "rootfile")
        .and_then(|node| node.attribute("full-path"))
        .ok_or_else(|| SourceError::Parse("EPUB container has no package document".into()))?;
    let package_path = clean_archive_path(package_path)?;
    let package = read_zip_entry(zip, &package_path, METADATA_LIMIT)?;
    Ok((package_path, package))
}

fn read_zip_entry(
    zip: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let entry = zip
        .by_name(name)
        .map_err(|_| SourceError::Parse(format!("EPUB resource {name:?} not found")))?;
    if entry.size() > limit {
        return Err(SourceError::Parse(format!(
            "EPUB resource {name:?} exceeds the {} MiB limit",
            limit / 1024 / 1024
        )));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(super::io_err)?;
    if bytes.len() as u64 > limit {
        return Err(SourceError::Parse(format!(
            "EPUB resource {name:?} is too large"
        )));
    }
    Ok(bytes)
}

fn ncx_labels(xml: &str, package_dir: &Path) -> Result<HashMap<String, String>> {
    let doc =
        Document::parse(xml).map_err(|e| SourceError::Parse(format!("invalid EPUB NCX: {e}")))?;
    let mut labels = HashMap::new();
    for point in doc.descendants().filter(|n| local_name(*n) == "navPoint") {
        let Some(src) = point
            .descendants()
            .find(|n| local_name(*n) == "content")
            .and_then(|n| n.attribute("src"))
        else {
            continue;
        };
        let Some(label) = point
            .descendants()
            .find(|n| local_name(*n) == "navLabel")
            .and_then(|n| n.descendants().find(|c| local_name(*c) == "text"))
            .and_then(|n| n.text())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        labels
            .entry(archive_join(package_dir, src)?)
            .or_insert_with(|| label.to_string());
    }
    Ok(labels)
}

fn navigation_labels(xml: &str, base: &Path) -> Result<HashMap<String, String>> {
    let doc = Document::parse(xml)
        .map_err(|e| SourceError::Parse(format!("invalid EPUB navigation: {e}")))?;
    let mut labels = HashMap::new();
    for nav in doc.descendants().filter(|node| local_name(*node) == "nav") {
        if !nav
            .attribute(("http://www.idpf.org/2007/ops", "type"))
            .is_some_and(|types| types.split_whitespace().any(|kind| kind == "toc"))
        {
            continue;
        }
        for anchor in nav.descendants().filter(|node| local_name(*node) == "a") {
            let Some(href) = anchor.attribute("href") else {
                continue;
            };
            let title: String = anchor
                .descendants()
                .filter(|node| node.is_text())
                .filter_map(|node| node.text())
                .collect();
            if !title.trim().is_empty() {
                labels
                    .entry(archive_join(base, href)?)
                    .or_insert_with(|| title.trim().to_string());
            }
        }
    }
    Ok(labels)
}

fn local_name<'input>(node: Node<'_, 'input>) -> &'input str {
    node.tag_name().name()
}

fn first_text(doc: &Document<'_>, name: &str) -> Option<String> {
    doc.descendants()
        .find(|n| local_name(*n) == name)
        .and_then(|n| n.text())
        .map(str::trim)
        .map(str::to_string)
}

fn archive_join(base: &Path, href: &str) -> Result<String> {
    let href = href.split('#').next().unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(href)
        .decode_utf8()
        .map_err(|_| SourceError::Parse(format!("invalid UTF-8 EPUB href {href:?}")))?;
    if decoded.contains([':', '\\']) {
        return Err(SourceError::Parse(format!(
            "external or invalid EPUB href {href:?}"
        )));
    }
    let mut joined = PathBuf::new();
    for component in base.join(decoded.as_ref()).components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            Component::ParentDir if joined.pop() => {}
            _ => {
                return Err(SourceError::Parse(format!(
                    "EPUB href escapes archive: {href:?}"
                )));
            }
        }
    }
    clean_archive_path(joined.to_string_lossy().as_ref())
}

fn clean_archive_path(path: &str) -> Result<String> {
    let mut clean = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) if !part.is_empty() => clean.push(part),
            Component::CurDir => {}
            _ => {
                return Err(SourceError::Parse(format!(
                    "EPUB resource path escapes the archive: {path:?}"
                )));
            }
        }
    }
    let clean = clean.to_string_lossy().replace('\\', "/");
    if clean.is_empty() {
        return Err(SourceError::Parse("empty EPUB resource path".into()));
    }
    Ok(clean)
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

fn resource_title(path: &str) -> String {
    file_stem(path).replace(['_', '-'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_links_normalize_only_inside_the_container() {
        assert_eq!(
            archive_join(Path::new("OPS/Package"), "../Text/chapter.xhtml#note").unwrap(),
            "OPS/Text/chapter.xhtml"
        );
        assert!(archive_join(Path::new("OPS"), "../../secret").is_err());
        assert!(archive_join(Path::new("OPS"), "https://outside.test/book.xhtml").is_err());
        for path in ["../secret", "OPS/../secret", "/etc/passwd", ""] {
            assert!(clean_archive_path(path).is_err());
        }
    }

    #[test]
    fn unsupported_layout_and_content_protection_are_explicit() {
        use std::io::Write;
        for (metadata, encrypted, expected) in [
            (
                "<meta property='rendition:layout'>pre-paginated</meta>",
                false,
                "fixed-layout",
            ),
            ("", true, "encrypted or obfuscated"),
        ] {
            let path =
                std::env::temp_dir().join(format!("yomu-epub-{}.epub", uuid::Uuid::new_v4()));
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zip.start_file(
                "META-INF/container.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"<container><rootfile full-path='book.opf'/></container>")
                .unwrap();
            zip.start_file("book.opf", zip::write::SimpleFileOptions::default())
                .unwrap();
            write!(zip, "<package><metadata>{metadata}</metadata></package>").unwrap();
            if encrypted {
                zip.start_file(
                    "META-INF/encryption.xml",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
                zip.write_all(b"<encryption/>").unwrap();
            }
            zip.finish().unwrap();
            let error = inspect(&path, "unsupported.epub").err().unwrap();
            assert!(error.to_string().contains(expected), "{error}");
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn navigation_labels_follow_document_relative_links_and_first_fragment() {
        let labels = navigation_labels("<html xmlns:epub='http://www.idpf.org/2007/ops'><nav epub:type='toc'><a href='../Text/first.xhtml'>First <span>section</span></a><a href='../Text/first.xhtml#nested'>Subsection</a></nav></html>", Path::new("OPS/Nav")).unwrap();
        assert_eq!(labels["OPS/Text/first.xhtml"], "First section");
        let labels = ncx_labels("<ncx><navPoint><navLabel><text>First</text></navLabel><content src='../Text/first.xhtml'/></navPoint></ncx>", Path::new("OPS/Nav")).unwrap();
        assert_eq!(labels["OPS/Text/first.xhtml"], "First");
    }

    #[test]
    fn epub_inspection_follows_spine_and_bounds_archive_reads() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("yomu-epub-{}.epub", uuid::Uuid::new_v4()));
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let entries = [
            (
                "META-INF/container.xml",
                "<container><rootfiles><rootfile full-path='OPS/book.opf'/></rootfiles></container>",
            ),
            (
                "OPS/book.opf",
                "<package><metadata><title>Original test book</title></metadata><manifest><item id='first' href='z' media-type='application/xhtml+xml'/><item id='second' href='a.xhtml' media-type='application/xhtml+xml'/><item id='cover' href='cover.png' media-type='image/png' properties='cover-image'/></manifest><spine><itemref idref='first'/><itemref idref='second'/></spine></package>",
            ),
            (
                "OPS/z",
                "<html><head><title>First</title></head><body>First resource</body></html>",
            ),
            (
                "OPS/a.xhtml",
                "<html><head><title>Second</title></head><body>Second resource</body></html>",
            ),
            ("OPS/cover.png", "original fixture"),
        ];
        for (name, bytes) in entries {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let book = inspect(&path, "test.epub").unwrap();
        assert_eq!(book.details.summary.title, "Original test book");
        assert_eq!(
            book.details
                .chapters
                .iter()
                .map(|c| (c.key.as_str(), c.title.as_str(), c.source_order))
                .collect::<Vec<_>>(),
            vec![("OPS/z", "First", 1), ("OPS/a.xhtml", "Second", 0)]
        );
        assert_eq!(book.cover_entry.as_deref(), Some("OPS/cover.png"));
        assert_eq!(book.resources[0].rel.as_deref(), Some("cover"));
        assert!(resource(&path, "OPS/../book.opf").is_err());
        let resource = resource(&path, "OPS/z").unwrap();
        assert_eq!(resource.media_type, "application/xhtml+xml");
        assert!(
            String::from_utf8(resource.bytes)
                .unwrap()
                .contains("First resource")
        );
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert!(read_zip_entry(&mut zip, "OPS/z", 5).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
