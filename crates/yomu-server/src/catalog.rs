//! Stale-while-revalidate policy for cached browse pages, plus the
//! single-flight guard for background revalidations.

use std::collections::HashSet;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

/// What the browse endpoint should do for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePlan {
    /// Serve the cached page, done.
    Fresh,
    /// Serve the cached page and refresh it in the background.
    Revalidate,
    /// Nothing usable cached: fetch live before answering.
    Live,
}

impl CachePlan {
    pub fn decide(cached_at: Option<DateTime<Utc>>, ttl_secs: u64, now: DateTime<Utc>) -> Self {
        let Some(at) = cached_at else {
            return CachePlan::Live;
        };
        if ttl_secs == 0 {
            return CachePlan::Live;
        }
        if (now - at).num_seconds() as u64 <= ttl_secs {
            CachePlan::Fresh
        } else {
            CachePlan::Revalidate
        }
    }
}

/// Guards against a stampede of identical background revalidations.
#[derive(Default)]
pub struct Inflight(Mutex<HashSet<String>>);

impl Inflight {
    /// True when the caller acquired the slot (must call `finish`).
    pub fn start(&self, key: &str) -> bool {
        self.0
            .lock()
            .expect("inflight lock")
            .insert(key.to_string())
    }

    pub fn finish(&self, key: &str) {
        self.0.lock().expect("inflight lock").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cache_plan_covers_all_states() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let fresh = now - chrono::Duration::minutes(5);
        let stale = now - chrono::Duration::hours(7);
        assert_eq!(CachePlan::decide(None, 3600, now), CachePlan::Live);
        assert_eq!(CachePlan::decide(Some(fresh), 21600, now), CachePlan::Fresh);
        assert_eq!(
            CachePlan::decide(Some(stale), 21600, now),
            CachePlan::Revalidate
        );
        // ttl 0 = caching off, even with a cached page
        assert_eq!(CachePlan::decide(Some(fresh), 0, now), CachePlan::Live);
    }

    #[test]
    fn inflight_is_single_entry() {
        let guard = Inflight::default();
        assert!(guard.start("k"));
        assert!(!guard.start("k"));
        guard.finish("k");
        assert!(guard.start("k"));
    }
}
