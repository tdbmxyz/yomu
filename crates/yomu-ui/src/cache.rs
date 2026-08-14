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

/// What a page renders from: the payload, and an error that never
/// replaces a payload already on screen.
pub struct Kept<V: Send + Sync + 'static> {
    pub value: RwSignal<Option<V>>,
    pub error: RwSignal<Option<String>>,
}

// Hand-written, like `Keyed`'s: a derived `Copy` would add a `V: Copy`
// bound, and none of the payloads (a Vec, a response struct) are Copy —
// the signals inside are, whatever they carry. Without this a page cannot
// use the value in more than one closure.
impl<V: Send + Sync + 'static> Clone for Kept<V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<V: Send + Sync + 'static> Copy for Kept<V> {}

/// Wire a cache to a page: seed from the cache, fetch only on a trigger,
/// and keep both in step.
///
/// `refresh` is the page's existing counter, so every current caller
/// keeps working by bumping it — a mutation, the manga page's download
/// poll, or pull-to-refresh.
pub fn keep_alive<K, V, Fut>(
    cache: Keyed<K, V>,
    key: K,
    refresh: RwSignal<u32>,
    conn: RwSignal<crate::Connectivity>,
    fetch: impl Fn() -> Fut + 'static,
) -> Kept<V>
where
    K: Clone + PartialEq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<V, yomu_client::ClientError>> + 'static,
{
    let kept = Kept {
        value: RwSignal::new(cache.answer(&key)),
        error: RwSignal::new(None),
    };
    let was_online = StoredValue::new(false);
    let last_refresh = StoredValue::new(refresh.get_untracked());
    let fetch = std::rc::Rc::new(fetch);
    Effect::new(move |prev: Option<()>| {
        let online = conn.get() == crate::Connectivity::Online;
        let bump = refresh.get();
        let refreshed = bump != last_refresh.get_value();
        last_refresh.set_value(bump);
        let reconnected = came_online(online, was_online.get_value(), prev.is_none());
        was_online.set_value(online);

        let answer = cache.answer(&key);
        // Seed the view from whatever the cache holds, before deciding.
        if answer.is_some() {
            kept.value.set(answer.clone());
        }
        if decide(answer.is_some(), cache.is_stale(), refreshed, reconnected) == Fetch::No {
            return;
        }
        let key = key.clone();
        let fetch = fetch.clone();
        leptos::task::spawn_local(async move {
            match fetch().await {
                Ok(value) => {
                    cache.store(key, value.clone());
                    kept.value.set(Some(value));
                    kept.error.set(None);
                }
                // A failed refresh must never empty a list being read: the
                // cached payload stays, the error is shown beside it.
                Err(err) => kept.error.set(Some(err.to_string())),
            }
        });
    });
    kept
}

/// The library list, shared by the Library and Home pages — they fetch the
/// same thing, so switching between them costs nothing.
pub type LibraryCache = Keyed<(), Vec<yomu_domain::PublicationWithLocator>>;
/// The category list, shared by the Library and manga pages.
pub type CategoriesCache = Keyed<(), Vec<yomu_domain::Category>>;
/// One open publication. The bool is the served-from-cache flag that tells
/// rows which chapters will not open.
pub type DetailCache = Keyed<uuid::Uuid, (yomu_domain::PublicationDetailResponse, bool)>;

pub fn use_library_cache() -> LibraryCache {
    use_context().expect("LibraryCache provided by App")
}

pub fn use_categories_cache() -> CategoriesCache {
    use_context().expect("CategoriesCache provided by App")
}

pub fn use_detail_cache() -> DetailCache {
    use_context().expect("DetailCache provided by App")
}

/// Everything a change to one publication can invalidate.
///
/// Takes the caches rather than reading them, because callers are inside
/// spawned tasks: a `use_context` after an await has no reactive owner and
/// returns None silently (auth playbook §7.1). `Keyed` is `Copy`, so the
/// caller captures it during setup for free.
pub fn mark_publication_stale(library: LibraryCache, detail: DetailCache) {
    library.mark_stale();
    detail.mark_stale();
}

/// Write a reading position into the library list without a fetch.
///
/// The locator is exact — the reader knows the unit and the page — so it
/// is patched. `unread_count` is not: `set_position` also folds the
/// position into read marks server-side (`api/progress.rs`,
/// `auto_mark_read`) by a reading-order rule the client must not
/// duplicate. So the cache is also flagged stale, and the next view
/// corrects the counts behind the list already on screen.
pub fn patch_locator(
    library: LibraryCache,
    publication_id: uuid::Uuid,
    locator: &yomu_domain::Locator,
    unit_title: Option<String>,
) {
    library.patch(&(), |list| {
        if let Some(entry) = list.iter_mut().find(|e| e.publication.id == publication_id) {
            entry.locator = Some(locator.clone());
            if unit_title.is_some() {
                entry.locator_unit_title = unit_title.clone();
            }
        }
    });
    library.mark_stale();
}

/// The same, for the open publication's detail.
pub fn patch_detail_locator(
    detail: DetailCache,
    publication_id: uuid::Uuid,
    locator: &yomu_domain::Locator,
) {
    detail.patch(&publication_id, |(value, _)| {
        value.locator = Some(locator.clone());
    });
    detail.mark_stale();
}

/// Return to where the list was left.
///
/// Saved from scroll events as they happen, not in `on_cleanup`: that
/// runs after navigation has already reset scroll to 0 (and that reset's
/// own event fires asynchronously, past the listener's removal, so it
/// cannot clobber the recording). `ready` gates the restore until there
/// is content to scroll through.
pub fn remember_scroll(key: String, ready: impl Fn() -> bool + 'static) {
    let save_key = key.clone();
    let save = window_event_listener(leptos::ev::scroll, move |_| {
        if let Some(storage) = window().session_storage().ok().flatten() {
            let y = window().scroll_y().unwrap_or(0.0);
            let _ = storage.set_item(&save_key, &y.to_string());
        }
    });
    on_cleanup(move || save.remove());

    let restored = StoredValue::new(false);
    Effect::new(move |_| {
        if !ready() || restored.get_value() {
            return;
        }
        restored.set_value(true);
        let key = key.clone();
        request_animation_frame(move || {
            if let Some(storage) = window().session_storage().ok().flatten()
                && let Ok(Some(saved)) = storage.get_item(&key)
                && let Ok(y) = saved.parse::<f64>()
            {
                window().scroll_to_with_x_and_y(0.0, y);
            }
        });
    });
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

    fn at() -> chrono::DateTime<chrono::Utc> {
        "2026-07-30T00:00:00Z".parse().unwrap()
    }

    fn entry(id: uuid::Uuid) -> yomu_domain::PublicationWithLocator {
        yomu_domain::PublicationWithLocator {
            publication: yomu_domain::Publication {
                id,
                kind: yomu_domain::Kind::Comics,
                origin: yomu_domain::Origin::LocalFile {
                    path: id.to_string(),
                },
                title: "A publication".into(),
                description: None,
                cover_url: None,
                auto_download: false,
                category: "reading".into(),
                genres: Vec::new(),
                added_at: at(),
                last_checked_at: None,
                missing_since: None,
                unsupported_count: 0,
                unsupported_formats: Vec::new(),
            },
            locator: None,
            unit_count: 3,
            unread_count: 3,
            downloaded_count: 0,
            latest_unit_at: None,
            locator_unit_title: None,
        }
    }

    /// Returning from the reader must show the position just read, with no
    /// fetch: the client knows it exactly, so it is written in rather than
    /// waited for.
    #[test]
    fn a_reading_position_is_written_into_the_library_entry() {
        let mine = uuid::Uuid::from_u128(1);
        let other = uuid::Uuid::from_u128(2);
        let library: LibraryCache = Keyed::default();
        library.store((), vec![entry(other), entry(mine)]);

        let locator = yomu_domain::Locator {
            unit_id: uuid::Uuid::from_u128(9),
            locations: yomu_domain::Locations::Page { page: 4 },
            at: at(),
        };
        patch_locator(library, mine, &locator, Some("Chapter 5".into()));

        let list = library.answer(&()).expect("cached");
        let updated = list.iter().find(|e| e.publication.id == mine).unwrap();
        assert_eq!(updated.locator.as_ref().unwrap().page(), 4);
        assert_eq!(updated.locator_unit_title.as_deref(), Some("Chapter 5"));
        // Every other title is untouched.
        let untouched = list.iter().find(|e| e.publication.id == other).unwrap();
        assert!(untouched.locator.is_none());
        // The unread count follows a server rule (auto_mark_read), so it is
        // refreshed rather than guessed.
        assert!(library.is_stale());
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
