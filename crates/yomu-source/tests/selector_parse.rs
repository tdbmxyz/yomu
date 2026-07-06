//! Selector source parsing against fixture HTML (no network).

use url::Url;
use yomu_source::selector::{SelectorSource, SelectorSpec};

fn fixture_source() -> SelectorSource {
    let spec: SelectorSpec = toml::from_str(
        r#"
        id = "fixture"
        name = "Fixture Scans"
        base_url = "https://fixture.test"

        [search]
        url = "{base}/search?q={query}"
        item = ".manga-item"
        link = "a.manga-link@href"
        cover = "img@src"

        [manga]
        title = "h1.entry-title"
        description = ".summary"
        cover = ".cover img@src"
        chapter_item = "li.chapter"
        chapter_link = "a@href"

        [pages]
        image = ".reading-content img.page@data-src"
        "#,
    )
    .expect("valid spec");
    SelectorSource::new(spec).expect("source compiles")
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn parses_search_results() {
    let source = fixture_source();
    let url = Url::parse("https://fixture.test/search?q=solo").unwrap();
    let hits = source.parse_search(&fixture("search.html"), &url).unwrap();

    assert_eq!(hits.len(), 2, "ad block item without link is skipped");
    assert_eq!(hits[0].title, "Solo Farming in the Tower");
    assert_eq!(hits[0].key, "https://fixture.test/manga/solo-farming");
    assert_eq!(
        hits[0].cover_url.as_ref().unwrap().as_str(),
        "https://fixture.test/covers/solo.jpg"
    );
}

#[test]
fn parses_browse_listing_with_search_selector_defaults() {
    use yomu_domain::BrowseSort;
    use yomu_source::Source;

    // Same spec as fixture_source, plus browse listings that reuse the
    // search result selectors.
    let spec: SelectorSpec = toml::from_str(
        r#"
        id = "fixture"
        name = "Fixture Scans"
        base_url = "https://fixture.test"

        [search]
        url = "{base}/search?q={query}"
        item = ".manga-item"
        link = "a.manga-link@href"
        cover = "img@src"

        [browse.popular]
        url = "{base}/list?order=views&page={page}"

        [manga]
        chapter_item = "li.chapter"
        chapter_link = "a@href"

        [pages]
        image = ".reading-content img.page@data-src"
        "#,
    )
    .expect("valid spec");
    let source = SelectorSource::new(spec).expect("source compiles");

    assert_eq!(source.browse_sorts(), vec![BrowseSort::Popular]);

    // A listing page has the same cards as a search page.
    let url = Url::parse("https://fixture.test/list?order=views&page=1").unwrap();
    let hits = source
        .parse_listing(BrowseSort::Popular, &fixture("search.html"), &url)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].key, "https://fixture.test/manga/solo-farming");

    // No latest listing configured.
    assert!(
        source
            .parse_listing(BrowseSort::Latest, &fixture("search.html"), &url)
            .is_err()
    );
}

#[test]
fn parses_manga_details_and_chapter_numbers() {
    let source = fixture_source();
    let url = Url::parse("https://fixture.test/manga/solo-farming").unwrap();
    let details = source.parse_manga(&fixture("manga.html"), &url).unwrap();

    assert_eq!(details.summary.title, "Solo Farming in the Tower");
    assert_eq!(
        details.description.as_deref(),
        Some("A farmer stuck in a tower. Comfy.")
    );
    assert_eq!(details.chapters.len(), 4);
    // Numbers parsed from titles, including decimals; order preserved as
    // listed (newest first here), captured in source_order.
    assert_eq!(details.chapters[0].number, Some(3.0));
    assert_eq!(details.chapters[1].number, Some(2.5));
    assert_eq!(details.chapters[0].source_order, 0);
    assert_eq!(details.chapters[3].title, "Chapter 1");
}

#[test]
fn parses_pages_from_lazy_load_attr() {
    let source = fixture_source();
    let url = Url::parse("https://fixture.test/manga/solo-farming/chapter-1").unwrap();
    let pages = source.parse_pages(&fixture("chapter.html"), &url).unwrap();

    assert_eq!(pages.len(), 3);
    assert_eq!(
        pages[0].as_str(),
        "https://fixture.test/pages/solo/1/001.png"
    );
}

#[test]
fn empty_chapter_list_is_an_error_not_silence() {
    let source = fixture_source();
    let url = Url::parse("https://fixture.test/manga/x").unwrap();
    let err = source.parse_manga("<html><body>cloudflare says hi</body></html>", &url);
    assert!(err.is_err());
}
