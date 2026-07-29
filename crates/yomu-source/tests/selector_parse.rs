//! Selector source parsing against fixture HTML (no network).

use url::Url;
use yomu_source::SourceError;
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
        genres = ".genres a"
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
fn parses_chapters_from_a_separate_fragment() {
    // Sites that load the chapter list htmx-style: the manga page only has
    // the details, chapters come from `chapters_url`.
    let spec: SelectorSpec = toml::from_str(
        r#"
        id = "fixture"
        name = "Fixture Scans"
        base_url = "https://fixture.test"

        [search]
        url = "{base}/search?q={query}"
        item = ".manga-item"
        link = "a.manga-link@href"

        [manga]
        title = "h1.entry-title"
        chapters_url = "{url_parent}/chapter-list"
        chapter_item = "li.chapter"
        chapter_link = "a@href"

        [pages]
        image = ".reading-content img.page@data-src"
        "#,
    )
    .expect("valid spec");
    let source = SelectorSource::new(spec).expect("source compiles");

    let page_url = Url::parse("https://fixture.test/manga/solo-farming/some-slug").unwrap();
    let chapters_url = Url::parse("https://fixture.test/manga/solo-farming/chapter-list").unwrap();
    let fragment = r#"
        <ul>
            <li class="chapter"><a href="/manga/solo-farming/chapter-2">Chapter 2</a></li>
            <li class="chapter"><a href="/manga/solo-farming/chapter-1">Chapter 1</a></li>
        </ul>
    "#;
    let details = source
        .parse_manga_parts(&fixture("manga.html"), fragment, &page_url, &chapters_url)
        .unwrap();

    assert_eq!(details.summary.title, "Solo Farming in the Tower");
    assert_eq!(details.chapters.len(), 2);
    assert_eq!(details.chapters[0].number, Some(2.0));
    // Relative links resolve against the fragment URL.
    assert_eq!(
        details.chapters[1].key,
        "https://fixture.test/manga/solo-farming/chapter-1"
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
    // Genres collected in document order, deduplicated (Action appears twice).
    assert_eq!(details.genres, vec!["Action", "Fantasy"]);
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

/// A source can describe what a paywall looks like on that site. When the
/// chapter serves no page images and that marker is on the page, nothing is
/// broken — the chapter simply is not free — so it must not be reported as a
/// parse failure.
#[test]
fn a_paywalled_chapter_is_unavailable_when_the_source_says_so() {
    let source = source_with_unavailable(Some("div.premium-lock"));
    let url = Url::parse("https://fixture.test/manga/solo-farming/chapter-173").unwrap();
    let err = source
        .parse_pages(&fixture("premium_chapter.html"), &url)
        .expect_err("no page images on the page");

    match err {
        SourceError::Unavailable(reason) => {
            assert!(
                reason.contains("This chapter is premium"),
                "the reason names the source's own wording: {reason}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// Sources that don't configure the key must behave exactly as before: an
/// empty page is a parse failure, because without a marker we cannot tell a
/// paywall from a selector that stopped matching.
#[test]
fn without_the_key_the_same_page_is_still_a_parse_error() {
    let source = source_with_unavailable(None);
    let url = Url::parse("https://fixture.test/manga/solo-farming/chapter-173").unwrap();
    let err = source
        .parse_pages(&fixture("premium_chapter.html"), &url)
        .expect_err("no page images on the page");
    assert!(matches!(err, SourceError::Parse(_)), "got {err:?}");
}

/// The marker only speaks when there are no images. A page that renders its
/// pages is fine however the marker matches — a stray match must never hide
/// a chapter the user can actually read.
#[test]
fn a_chapter_with_images_is_never_unavailable() {
    let source = source_with_unavailable(Some("div.premium-lock"));
    let url = Url::parse("https://fixture.test/manga/solo-farming/chapter-1").unwrap();
    let html = fixture("chapter.html").replace(
        "<body>",
        r#"<body><div class="premium-lock">This chapter is premium</div>"#,
    );
    assert_eq!(source.parse_pages(&html, &url).unwrap().len(), 3);
}

/// And a page with neither images nor the marker is still a parse failure on
/// a source that configures the key — that one really is broken.
#[test]
fn a_configured_source_still_reports_a_broken_selector() {
    let source = source_with_unavailable(Some("div.premium-lock"));
    let url = Url::parse("https://fixture.test/manga/solo-farming/chapter-2").unwrap();
    let err = source
        .parse_pages("<html><body><p>nothing here</p></body></html>", &url)
        .expect_err("no page images on the page");
    assert!(matches!(err, SourceError::Parse(_)), "got {err:?}");
}

fn source_with_unavailable(selector: Option<&str>) -> SelectorSource {
    let unavailable = selector.map_or(String::new(), |s| format!("unavailable = {s:?}"));
    let spec: SelectorSpec = toml::from_str(&format!(
        r#"
        id = "fixture"
        name = "Fixture Scans"
        base_url = "https://fixture.test"

        [search]
        url = "{{base}}/search?q={{query}}"
        item = ".manga-item"
        link = "a.manga-link@href"

        [manga]
        chapter_item = "li.chapter"
        chapter_link = "a@href"

        [pages]
        image = ".reading-content img.page@data-src"
        {unavailable}
        "#
    ))
    .expect("valid spec");
    SelectorSource::new(spec).expect("source compiles")
}
