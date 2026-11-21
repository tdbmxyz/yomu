//! UTC timestamps backed by Jiff. This deliberately small compatibility
//! surface keeps the existing JSON and SQLite representations unchanged while
//! removing the unmaintained Chrono dependency. Only UTC is supported.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    fmt,
    marker::PhantomData,
    ops::{Add, Sub},
    str::FromStr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime<Zone = Utc> {
    inner: jiff::Timestamp,
    zone: PhantomData<Zone>,
}
impl DateTime<Utc> {
    pub const MIN_UTC: Self = Self {
        inner: jiff::Timestamp::MIN,
        zone: PhantomData,
    };
    fn from_inner(inner: jiff::Timestamp) -> Self {
        Self {
            inner,
            zone: PhantomData,
        }
    }
    pub fn from_timestamp(seconds: i64, nanos: u32) -> Option<Self> {
        if nanos >= 1_000_000_000 {
            return None;
        }
        // Jiff normalizes a negative second's subsecond toward zero.
        jiff::Timestamp::from_second(seconds)
            .ok()?
            .checked_add(jiff::SignedDuration::from_nanos(nanos.into()))
            .ok()
            .map(Self::from_inner)
    }
    pub fn parse_from_rfc3339(text: &str) -> Result<Self, jiff::Error> {
        text.parse()
    }
    pub fn parse_from_str(text: &str, format: &str) -> Result<Self, jiff::Error> {
        jiff::Timestamp::strptime(format, text).map(Self::from_inner)
    }
    /// Date-only and timezone-free source formats resolve at UTC, as before.
    pub fn parse_source_date(text: &str, format: &str) -> Option<Self> {
        if let Ok(at) = Self::parse_from_str(text, format) {
            return Some(at);
        }
        let datetime = jiff::civil::DateTime::strptime(format, text)
            .ok()
            .or_else(|| {
                jiff::civil::Date::strptime(format, text)
                    .ok()
                    .map(|date| date.at(0, 0, 0, 0))
            })?;
        jiff::tz::Offset::UTC
            .to_timestamp(datetime)
            .ok()
            .map(Self::from_inner)
    }
    pub fn with_timezone(self, _: &Utc) -> Self {
        self
    }
    fn civil(self) -> jiff::civil::DateTime {
        jiff::tz::Offset::UTC.to_datetime(self.inner)
    }
    fn format_date(date: jiff::civil::Date) -> String {
        let year = date.year();
        let width = if year < 0 { 5 } else { 4 };
        format!("{year:0width$}-{:02}-{:02}", date.month(), date.day())
    }
    pub fn date_label(self) -> String {
        Self::format_date(self.civil().date())
    }
    pub fn checked_at_label(self) -> String {
        let civil = self.civil();
        format!(
            "{} {:02}:{:02} UTC",
            Self::format_date(civil.date()),
            civil.hour(),
            civil.minute()
        )
    }
    pub fn short_date_label(self, include_year: bool) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let civil = self.civil();
        let month = MONTHS[civil.month() as usize - 1];
        if include_year {
            format!("{month} {}, {}", civil.day(), civil.year())
        } else {
            format!("{month} {}", civil.day())
        }
    }
    pub fn signed_duration_since(self, other: Self) -> Duration {
        Duration(self.inner.duration_since(other.inner))
    }
    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        self.inner
            .checked_sub(duration.0)
            .ok()
            .map(Self::from_inner)
    }
    /// Preserve Chrono's AutoSi precision and +00:00 suffix in SQL TEXT.
    pub fn to_rfc3339(self) -> String {
        let civil = self.civil();
        let mut text = format!(
            "{}T{:02}:{:02}:{:02}",
            Self::format_date(civil.date()),
            civil.hour(),
            civil.minute(),
            civil.second()
        );
        let nanos = civil.subsec_nanosecond();
        if nanos != 0 {
            let (value, width) = if nanos % 1_000_000 == 0 {
                (nanos / 1_000_000, 3)
            } else if nanos % 1_000 == 0 {
                (nanos / 1_000, 6)
            } else {
                (nanos, 9)
            };
            text.push_str(&format!(".{value:0width$}"));
        }
        text.push_str("+00:00");
        text
    }
}
impl Utc {
    pub fn now() -> DateTime<Self> {
        DateTime::from_inner(jiff::Timestamp::now())
    }
}
impl FromStr for DateTime<Utc> {
    type Err = jiff::Error;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse::<jiff::Timestamp>().map(Self::from_inner)
    }
}
impl fmt::Display for DateTime<Utc> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}
impl Serialize for DateTime<Utc> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut text = self.to_rfc3339();
        text.truncate(text.len() - 6);
        text.push('Z');
        serializer.serialize_str(&text)
    }
}
impl<'de> Deserialize<'de> for DateTime<Utc> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
pub trait Datelike {
    fn year(&self) -> i32;
}
impl Datelike for DateTime<Utc> {
    fn year(&self) -> i32 {
        self.civil().year().into()
    }
}
pub trait TimeZone {
    fn with_ymd_and_hms(
        &self,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<DateTime<Utc>>;
}
impl TimeZone for Utc {
    fn with_ymd_and_hms(
        &self,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<DateTime<Utc>> {
        let civil = jiff::civil::DateTime::new(
            year.try_into().ok()?,
            month.try_into().ok()?,
            day.try_into().ok()?,
            hour.try_into().ok()?,
            minute.try_into().ok()?,
            second.try_into().ok()?,
            0,
        )
        .ok()?;
        jiff::tz::Offset::UTC
            .to_timestamp(civil)
            .ok()
            .map(DateTime::from_inner)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Duration(jiff::SignedDuration);
impl Duration {
    pub fn seconds(n: i64) -> Self {
        Self(jiff::SignedDuration::from_secs(n))
    }
    pub fn milliseconds(n: i64) -> Self {
        Self(jiff::SignedDuration::from_millis(n))
    }
    pub fn minutes(n: i64) -> Self {
        Self(jiff::SignedDuration::from_mins(n))
    }
    pub fn hours(n: i64) -> Self {
        Self(jiff::SignedDuration::from_hours(n))
    }
    pub fn days(n: i64) -> Self {
        Self::hours(n.checked_mul(24).expect("duration days in range"))
    }
    pub fn weeks(n: i64) -> Self {
        Self::days(n.checked_mul(7).expect("duration weeks in range"))
    }
    pub fn num_seconds(self) -> i64 {
        self.0.as_secs()
    }
    pub fn num_minutes(self) -> i64 {
        self.num_seconds() / 60
    }
    pub fn num_hours(self) -> i64 {
        self.num_seconds() / 3600
    }
    pub fn num_days(self) -> i64 {
        self.num_seconds() / 86400
    }
}
impl Add<Duration> for DateTime<Utc> {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self {
        Self::from_inner(self.inner + rhs.0)
    }
}
impl Sub<Duration> for DateTime<Utc> {
    type Output = Self;
    fn sub(self, rhs: Duration) -> Self {
        Self::from_inner(self.inner - rhs.0)
    }
}
impl Sub for DateTime<Utc> {
    type Output = Duration;
    fn sub(self, rhs: Self) -> Duration {
        self.signed_duration_since(rhs)
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use sqlx::{
        Decode, Encode, Sqlite, Type,
        encode::IsNull,
        error::BoxDynError,
        sqlite::{SqliteArgumentsBuffer, SqliteTypeInfo, SqliteValueRef},
    };
    impl Type<Sqlite> for DateTime<Utc> {
        fn type_info() -> SqliteTypeInfo {
            <String as Type<Sqlite>>::type_info()
        }
        fn compatible(info: &SqliteTypeInfo) -> bool {
            <String as Type<Sqlite>>::compatible(info)
        }
    }
    impl Encode<'_, Sqlite> for DateTime<Utc> {
        fn encode_by_ref(&self, buf: &mut SqliteArgumentsBuffer) -> Result<IsNull, BoxDynError> {
            <String as Encode<Sqlite>>::encode(self.to_rfc3339(), buf)
        }
    }
    impl<'r> Decode<'r, Sqlite> for DateTime<Utc> {
        fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
            let text = <String as Decode<Sqlite>>::decode(value)?;
            if let Ok(at) = text.parse() {
                return Ok(at);
            }
            // SQLite's CURRENT_TIMESTAMP and legacy fixtures can omit the
            // offset and fractional seconds. Interpret those as UTC.
            let civil = text.parse::<jiff::civil::DateTime>()?;
            Ok(Self::from_inner(jiff::tz::Offset::UTC.to_timestamp(civil)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_json_and_database_precision_are_preserved() {
        for (input, stored, wire) in [
            (
                "2026-01-02T03:04:05Z",
                "2026-01-02T03:04:05+00:00",
                "2026-01-02T03:04:05Z",
            ),
            (
                "2026-01-02T05:04:05.1+02:00",
                "2026-01-02T03:04:05.100+00:00",
                "2026-01-02T03:04:05.100Z",
            ),
            (
                "2026-01-02T03:04:05.123456Z",
                "2026-01-02T03:04:05.123456+00:00",
                "2026-01-02T03:04:05.123456Z",
            ),
            (
                "2026-01-02T03:04:05.123456789Z",
                "2026-01-02T03:04:05.123456789+00:00",
                "2026-01-02T03:04:05.123456789Z",
            ),
        ] {
            let at: DateTime<Utc> = input.parse().unwrap();
            assert_eq!(at.to_rfc3339(), stored);
            assert_eq!(serde_json::to_value(at).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<DateTime<Utc>>(serde_json::json!(wire)).unwrap(),
                at
            );
        }
    }
    #[test]
    fn negative_epoch_fractions_and_invalid_dates() {
        assert_eq!(
            DateTime::from_timestamp(-1, 500_000_000)
                .unwrap()
                .to_rfc3339(),
            "1969-12-31T23:59:59.500+00:00"
        );
        assert!(Utc.with_ymd_and_hms(2026, 2, 30, 0, 0, 0).is_none());
        assert!("not a timestamp".parse::<DateTime<Utc>>().is_err());
    }
}
