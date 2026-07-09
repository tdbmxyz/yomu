//! Human formatting helpers shared by pages.

use chrono::{DateTime, Datelike, Utc};

/// Chapter release date, compact: relative under a week ("5 h. ago"),
/// short absolute beyond ("May 19", year appended when it isn't
/// `now`'s). Future dates (clock skew, site rounding) read "just now".
pub fn published_label(published: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let age = now.signed_duration_since(published);
    let mins = age.num_minutes();
    if mins < 1 {
        return "just now".into();
    }
    if mins < 60 {
        return format!("{mins} min. ago");
    }
    if age.num_hours() < 24 {
        return format!("{} h. ago", age.num_hours());
    }
    if age.num_days() < 7 {
        return format!("{} d. ago", age.num_days());
    }
    if published.year() == now.year() {
        published.format("%b %-d").to_string()
    } else {
        published.format("%b %-d, %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
    }

    #[test]
    fn relative_tiers() {
        let n = now();
        assert_eq!(
            published_label(n - chrono::Duration::seconds(30), n),
            "just now"
        );
        assert_eq!(
            published_label(n - chrono::Duration::minutes(42), n),
            "42 min. ago"
        );
        assert_eq!(
            published_label(n - chrono::Duration::hours(5), n),
            "5 h. ago"
        );
        assert_eq!(published_label(n - chrono::Duration::days(3), n), "3 d. ago");
    }

    #[test]
    fn absolute_beyond_a_week() {
        let n = now();
        assert_eq!(
            published_label(Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap(), n),
            "May 19"
        );
        assert_eq!(
            published_label(Utc.with_ymd_and_hms(2025, 5, 19, 0, 0, 0).unwrap(), n),
            "May 19, 2025"
        );
    }

    #[test]
    fn future_reads_just_now() {
        let n = now();
        assert_eq!(published_label(n + chrono::Duration::hours(2), n), "just now");
    }
}
