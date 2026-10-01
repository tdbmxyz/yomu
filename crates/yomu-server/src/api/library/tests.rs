use super::*;
use chrono::{TimeZone, Utc};
use yomu_domain::{Locations, Locator};

fn version(id: u128, filename: &str, units: u32, day: Option<u32>) -> PublicationWithLocator {
    let publication: Publication = serde_json::from_value(serde_json::json!({
        "id": Uuid::from_u128(id), "work_id": Uuid::from_u128(100),
        "kind": "novels", "source_id": "local", "source_key": filename,
        "title": filename, "auto_download": false, "added_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    PublicationWithLocator {
        editions: PublicationEdition::from_publication(&publication)
            .into_iter()
            .collect(),
        publication,
        locator: day.map(|day| Locator {
            unit_id: Uuid::from_u128(id + 1000),
            locations: Locations::Page { page: 5 },
            at: Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap(),
        }),
        unit_count: units,
        unread_count: units,
        downloaded_count: 0,
        latest_unit_at: None,
        locator_unit_title: None,
    }
}

#[test]
fn library_counts_and_location_belong_to_one_version() {
    let epub = version(1, "Book.epub", 20, Some(1));
    let pdf = version(2, "Book.pdf", 1, Some(2));
    let out = group_book_versions(vec![epub, pdf.clone(), version(3, "Book.mobi", 0, None)]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].publication.id, pdf.publication.id);
    assert_eq!(out[0].publication.title, "Book.epub");
    assert_eq!(out[0].unit_count, 1);
    assert_eq!(out[0].locator, pdf.locator);
    assert_eq!(out[0].editions.len(), 3);
}

#[test]
fn missing_or_unreadable_versions_do_not_displace_a_readable_book() {
    let mut epub = version(1, "Book.epub", 20, Some(3));
    epub.publication.missing_since = Some(Utc::now());
    let pdf = version(2, "Book.pdf", 1, Some(1));
    let out = group_book_versions(vec![epub, pdf.clone(), version(3, "Book.mobi", 0, Some(4))]);
    assert_eq!(out[0].publication.id, pdf.publication.id);
    assert_eq!(out[0].editions.len(), 3);
    let out = group_book_versions(vec![
        version(1, "Book.epub", 20, None),
        version(2, "Book.pdf", 1, None),
    ]);
    assert_eq!(out[0].publication.book_format(), Some(BookFormat::Epub));
}
