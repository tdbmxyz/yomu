//! Best-effort parsing of chapter release dates scraped from listings.
//! Sites print them three ways: machine-readable RFC 3339 (usually a
//! `<time datetime>` attribute), an absolute local convention like
//! "2026/05/19", or English relative phrases like "2 days ago".

use chrono::{DateTime, NaiveDate, Utc};

/// `text` is whitespace-normalized selector output. `format` is the
/// source's optional `chapter_date_format` (chrono syntax); date-only
/// formats resolve to midnight UTC. Relative phrases resolve against
/// `now`. Returns `None` rather than erroring: a missing or odd date
/// must never fail a sync.
pub(crate) fn parse_chapter_date(
    text: &str,
    format: Option<&str>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Some(fmt) = format {
        if let Ok(dt) = DateTime::parse_from_str(text, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
        // A format carrying a time but no offset (e.g. "%Y-%m-%d %H:%M")
        // parses as neither a zoned DateTime nor a bare NaiveDate; treat it
        // as local-naive wall time at UTC, matching the date-only branch.
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, fmt) {
            return Some(dt.and_utc());
        }
        if let Ok(date) = NaiveDate::parse_from_str(text, fmt) {
            return date.and_hms_opt(0, 0, 0).map(|d| d.and_utc());
        }
    }
    relative(text, now)
}

/// "just now" / "N <unit>(s) ago" / "a(n) <unit> ago", English only —
/// what the deployed sites print. Months and years are approximate by
/// nature (the site already rounded).
fn relative(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let lower = text.to_ascii_lowercase();
    if lower == "just now" || lower == "now" {
        return Some(now);
    }
    if lower == "yesterday" {
        return Some(now - chrono::Duration::days(1));
    }
    // "last week" / "last month" / "last year" reuse the unit table below.
    let (amount, unit) = match lower.strip_prefix("last ") {
        Some(unit) => ("1", unit),
        None => lower.strip_suffix(" ago")?.split_once(' ')?,
    };
    let n: i64 = match amount {
        "a" | "an" | "one" => 1,
        _ => amount.parse().ok()?,
    };
    let unit_seconds: i64 = match unit.trim_end_matches('s') {
        "second" | "sec" => 1,
        "minute" | "min" => 60,
        "hour" | "hr" => 3_600,
        "day" => 86_400,
        "week" => 604_800,
        "month" => 30 * 86_400,
        "year" => 365 * 86_400,
        _ => return None,
    };
    Some(now - chrono::Duration::seconds(n * unit_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn rfc3339_with_fraction_and_zulu() {
        assert_eq!(
            parse_chapter_date("2026-07-09T21:11:00.205Z", None, now()),
            Some(at(2026, 7, 9, 21, 11, 0) + chrono::Duration::milliseconds(205)),
        );
    }

    #[test]
    fn configured_date_only_format_is_midnight_utc() {
        assert_eq!(
            parse_chapter_date("2026/05/19", Some("%Y/%m/%d"), now()),
            Some(at(2026, 5, 19, 0, 0, 0)),
        );
    }

    #[test]
    fn configured_datetime_without_timezone_parses() {
        assert_eq!(
            parse_chapter_date("2026-05-19 14:30", Some("%Y-%m-%d %H:%M"), now()),
            Some(at(2026, 5, 19, 14, 30, 0)),
        );
    }

    #[test]
    fn relative_phrases() {
        let n = now();
        assert_eq!(parse_chapter_date("just now", None, n), Some(n));
        assert_eq!(
            parse_chapter_date("42 minutes ago", None, n),
            Some(n - chrono::Duration::minutes(42)),
        );
        assert_eq!(
            parse_chapter_date("2 days ago", None, n),
            Some(n - chrono::Duration::days(2)),
        );
        assert_eq!(
            parse_chapter_date("an hour ago", None, n),
            Some(n - chrono::Duration::hours(1)),
        );
        assert_eq!(
            parse_chapter_date("3 months ago", None, n),
            Some(n - chrono::Duration::days(90)),
        );
        assert_eq!(
            parse_chapter_date("1 year ago", None, n),
            Some(n - chrono::Duration::days(365)),
        );
    }

    #[test]
    fn named_relative_phrases() {
        let n = now();
        assert_eq!(
            parse_chapter_date("yesterday", None, n),
            Some(n - chrono::Duration::days(1)),
        );
        assert_eq!(
            parse_chapter_date("last week", None, n),
            Some(n - chrono::Duration::weeks(1)),
        );
        assert_eq!(
            parse_chapter_date("last month", None, n),
            Some(n - chrono::Duration::days(30)),
        );
        assert_eq!(
            parse_chapter_date("last year", None, n),
            Some(n - chrono::Duration::days(365)),
        );
    }

    #[test]
    fn case_insensitive_relative() {
        assert_eq!(
            parse_chapter_date("2 Days Ago", None, now()),
            Some(now() - chrono::Duration::days(2)),
        );
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse_chapter_date("Chapter 12", None, now()), None);
        assert_eq!(parse_chapter_date("", None, now()), None);
        assert_eq!(parse_chapter_date("someday soon", None, now()), None);
        // configured format that doesn't match falls through to None
        assert_eq!(
            parse_chapter_date("19-05-2026", Some("%Y/%m/%d"), now()),
            None
        );
    }
}
