//! Page data that outlives the page component.
//!
//! The router drops and rebuilds a page on every visit, so a
//! `LocalResource` declared inside one is new each time and fetches from
//! cold. These caches are provided once in `App`, above the router, so a
//! return costs nothing. See
//! `docs/superpowers/specs/2026-07-30-list-keep-alive-design.md`.

use leptos::prelude::*;

/// One cached payload and the request it answers. Single-entry: storing a
/// different key evicts the previous one, which bounds memory and matches
/// how the app is used (one library, one open title).
pub struct Keyed<K: Send + Sync + 'static, V: Send + Sync + 'static> {
    slot: RwSignal<Option<(K, V)>>,
    /// Something happened that this copy does not reflect. It is still
    /// worth showing — the next view paints it, then corrects it in the
    /// background.
    stale: RwSignal<bool>,
}

// Derived impls would demand `K: Clone, V: Clone`; the signals inside are
// `Copy` whatever they carry.
impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Clone for Keyed<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Copy for Keyed<K, V> {}

impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Default for Keyed<K, V> {
    fn default() -> Self {
        Self {
            slot: RwSignal::new(None),
            stale: RwSignal::new(false),
        }
    }
}

impl<K, V> Keyed<K, V>
where
    K: PartialEq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// The cached value, but only if it answers `key`.
    pub fn answer(&self, key: &K) -> Option<V> {
        self.slot.with_untracked(|slot| match slot {
            Some((cached, value)) if cached == key => Some(value.clone()),
            _ => None,
        })
    }

    /// Store a freshly fetched value. Fresh means not stale.
    pub fn store(&self, key: K, value: V) {
        self.slot.set(Some((key, value)));
        self.stale.set(false);
    }

    /// Edit in place if the cache holds `key`; otherwise do nothing. Used
    /// for what the client knows exactly, such as a reading position.
    pub fn patch(&self, key: &K, edit: impl FnOnce(&mut V)) {
        self.slot.update(|slot| {
            if let Some((cached, value)) = slot
                && cached == key
            {
                edit(value);
            }
        });
    }

    /// Something changed that this copy does not reflect.
    pub fn mark_stale(&self) {
        self.stale.set(true);
    }

    pub fn is_stale(&self) -> bool {
        self.stale.get_untracked()
    }

    /// Drop the payload entirely: the next view fetches with a spinner.
    pub fn clear(&self) {
        self.slot.set(None);
        self.stale.set(false);
    }
}

/// What a page should do when it is (re)entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fetch {
    /// The cache answers and nothing has happened since. Do nothing.
    No,
    /// Show what is cached, correct it quietly.
    Background,
    /// Nothing to show; fetch with the loading state.
    Blocking,
}

/// The whole staleness policy, in one place.
pub fn decide(answered: bool, stale: bool, refreshed: bool, came_online: bool) -> Fetch {
    match (answered, stale || refreshed || came_online) {
        (false, _) => Fetch::Blocking,
        (true, true) => Fetch::Background,
        (true, false) => Fetch::No,
    }
}

/// An Offline->Online *transition*, not "we are online".
///
/// `first_run` is the guard that matters: `was_online` starts false on
/// every mount, so without it every visit looks like a reconnection and
/// refetches, silently undoing the whole point of the cache.
pub fn came_online(online: bool, was_online: bool, first_run: bool) -> bool {
    online && !was_online && !first_run
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing cached: a spinner is the honest answer.
    #[test]
    fn an_empty_cache_blocks() {
        assert_eq!(decide(false, false, false, false), Fetch::Blocking);
        // Even with a trigger — there is nothing to show meanwhile.
        assert_eq!(decide(false, true, true, true), Fetch::Blocking);
    }

    /// The whole feature: a plain revisit costs nothing.
    #[test]
    fn a_plain_revisit_does_not_fetch() {
        assert_eq!(decide(true, false, false, false), Fetch::No);
    }

    /// A trigger refreshes behind the list already on screen.
    #[test]
    fn a_trigger_refreshes_in_the_background() {
        assert_eq!(decide(true, true, false, false), Fetch::Background);
        assert_eq!(decide(true, false, true, false), Fetch::Background);
        assert_eq!(decide(true, false, false, true), Fetch::Background);
    }

    /// `was_online` starts false on every mount, so without the first-run
    /// guard every visit looks like a reconnection and refetches — which
    /// silently undoes this entire change.
    #[test]
    fn the_first_run_is_never_a_reconnection() {
        assert!(!came_online(true, false, true));
        assert!(came_online(true, false, false));
        assert!(!came_online(true, true, false));
        assert!(!came_online(false, true, false));
    }

    /// The stored key is the whole point: without it, opening title B
    /// answers with title A's chapters.
    #[test]
    fn only_the_stored_key_is_answered() {
        let cache: Keyed<u8, &'static str> = Keyed::default();
        assert_eq!(cache.answer(&1), None);
        cache.store(1, "one");
        assert_eq!(cache.answer(&1), Some("one"));
        assert_eq!(cache.answer(&2), None);
    }

    /// A fresh value answers the question the stale flag was asking, so
    /// storing must clear it — otherwise every page refetches forever.
    #[test]
    fn storing_clears_stale() {
        let cache: Keyed<u8, &'static str> = Keyed::default();
        cache.store(1, "one");
        cache.mark_stale();
        assert!(cache.is_stale());
        cache.store(1, "fresh");
        assert!(!cache.is_stale());
    }

    /// Patching the wrong key must not corrupt what is cached: the reader
    /// reports a position for one publication while the cache may hold
    /// another.
    #[test]
    fn patching_a_different_key_changes_nothing() {
        let cache: Keyed<u8, String> = Keyed::default();
        cache.store(1, "one".to_string());
        cache.patch(&2, |v| v.push('!'));
        assert_eq!(cache.answer(&1), Some("one".to_string()));
        cache.patch(&1, |v| v.push('!'));
        assert_eq!(cache.answer(&1), Some("one!".to_string()));
    }

    /// Single-entry by design: opening title B evicts title A, which is
    /// what bounds memory.
    #[test]
    fn a_second_key_evicts_the_first() {
        let cache: Keyed<u8, &'static str> = Keyed::default();
        cache.store(1, "one");
        cache.store(2, "two");
        assert_eq!(cache.answer(&1), None);
        assert_eq!(cache.answer(&2), Some("two"));
    }
}
