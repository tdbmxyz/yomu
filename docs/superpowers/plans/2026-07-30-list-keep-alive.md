# List Keep-Alive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Returning to the library, the home shelves or a chapter list shows the list instantly, with no fetch and no loading flash.

**Architecture:** A `Copy` single-entry cache keyed by the request lives above the router in `App`, so route changes destroy components but not data. Each page swaps its `LocalResource` for a view signal plus one effect whose fetch decision is a pure, tested function: blocking when nothing is cached, background when the cache is stale, otherwise nothing at all. A reading session patches the locator into the cache exactly and flags the rest stale; pull-to-refresh is the manual trigger.

**Tech Stack:** Rust, Leptos 0.8 (CSR/wasm), `leptos::prelude` signals, `web-sys` touch events, `just` for checks.

**Spec:** `docs/superpowers/specs/2026-07-30-list-keep-alive-design.md`

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/yomu-ui/src/cache.rs` (create) | `Keyed<K, V>` cache, the `decide()` fetch rule, the `keep_alive()` effect that joins them, and `remember_scroll()`. One module because all four exist to serve the same lifetime problem. |
| `crates/yomu-ui/src/refresh.rs` (create) | `use_pull_to_refresh` — the touch gesture and its pure arithmetic. Independent of the cache; a page wires the two together. |
| `crates/yomu-ui/src/lib.rs` (modify) | Declare both modules; provide the three caches in `App`. |
| `crates/yomu-ui/src/pages/library.rs` (modify) | Use `LibraryCache` + `CategoriesCache`; pull-to-refresh; scroll restore. |
| `crates/yomu-ui/src/pages/home.rs` (modify) | Use `LibraryCache` (shared with Library); pull-to-refresh; scroll restore. |
| `crates/yomu-ui/src/pages/manga.rs` (modify) | Use `DetailCache` + `CategoriesCache`; pull-to-refresh; scroll restore via the shared helper. |
| `crates/yomu-ui/src/pages/reader/mod.rs` (modify) | Patch the locator into the caches and flag them stale. |
| `crates/yomu-ui/src/chapter_actions.rs`, `pages/downloads.rs`, `pages/search.rs`, `offline.rs` (modify) | Flag caches stale at the remaining mutation sites. |
| `crates/yomu-web/styles.css` (modify) | `.pull-refresh` styling. |

**Commands used throughout:**

- Unit tests: `cargo test -p yomu-ui`
- Full gate: `just check` (fmt, clippy `-D warnings`, wasm check)

---

## Task 1: The `Keyed` cache

**Files:**
- Create: `crates/yomu-ui/src/cache.rs`
- Modify: `crates/yomu-ui/src/lib.rs` (module declaration)

- [ ] **Step 1: Declare the module**

In `crates/yomu-ui/src/lib.rs`, in the module list at the top (lines 5-12), add `mod cache;` in alphabetical position — after `mod chapter_actions;` is wrong, it goes before it:

```rust
mod cache;
mod chapter_actions;
mod cover;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/yomu-ui/src/cache.rs` with only the tests plus a `use super::*;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        cache.patch(&2, |v| v.push_str("!"));
        assert_eq!(cache.answer(&1), Some("one".to_string()));
        cache.patch(&1, |v| v.push_str("!"));
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p yomu-ui cache::`
Expected: FAIL to compile — `cannot find type Keyed in this scope`.

- [ ] **Step 4: Implement `Keyed`**

Prepend to `crates/yomu-ui/src/cache.rs` (above the test module):

```rust
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
        self.slot
            .with_untracked(|slot| match slot {
                Some((cached, value)) if cached == key => Some(value),
                _ => None,
            })
            .cloned()
    }

    /// Reactive read for the view: the value if it answers `key`.
    pub fn watch(&self, key: &K) -> Option<V> {
        self.slot
            .with(|slot| match slot {
                Some((cached, value)) if cached == key => Some(value),
                _ => None,
            })
            .cloned()
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
        self.stale.get()
    }

    /// Drop the payload entirely: the next view fetches with a spinner.
    pub fn clear(&self) {
        self.slot.set(None);
        self.stale.set(false);
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yomu-ui cache::`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/yomu-ui/src/cache.rs crates/yomu-ui/src/lib.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): a keyed cache that outlives the page component

The router rebuilds a page on every visit, so a LocalResource declared
inside one fetches from cold. Keyed holds a payload and the request it
answers, so it can be provided once above the router and asked whether
it answers the key a page actually wants — the check that keeps title B
from showing title A's chapters.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 2: The fetch decision

**Files:**
- Modify: `crates/yomu-ui/src/cache.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/yomu-ui/src/cache.rs`:

```rust
    /// Nothing cached: a spinner is the honest answer.
    #[test]
    fn an_empty_cache_blocks() {
        assert_eq!(decide(false, false, false, false), Fetch::Blocking);
        // Even with a trigger — there is nothing to show meanwhile.
        assert_eq!(decide(false, true, true, true), Fetch::Blocking);
    }

    /// The whole feature: a plain revisit costs nothing. This goes red if
    /// the Offline->Online transition ever degrades to "we are online".
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yomu-ui cache::`
Expected: FAIL to compile — `cannot find function decide`.

- [ ] **Step 3: Implement the decision**

Add to `crates/yomu-ui/src/cache.rs`, after the `Keyed` impl:

```rust
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
/// every mount, so without it every visit looks like a reconnection.
pub fn came_online(online: bool, was_online: bool, first_run: bool) -> bool {
    online && !was_online && !first_run
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yomu-ui cache::`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-ui/src/cache.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): the fetch decision, as a pure function

Blocking when nothing is cached, background when something changed
behind the list, and nothing at all for a plain revisit — which is the
case the whole feature exists for, and the one a test now pins.

came_online is a transition rather than a state. was_online starts false
on every mount, so without the first-run guard every visit reads as a
reconnection and refetches, quietly undoing the change.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 3: The `keep_alive` effect

Joins Task 1 and Task 2 into the single call a page makes. Not unit-tested (it is a wasm-only reactive effect); its logic is the two tested functions.

**Files:**
- Modify: `crates/yomu-ui/src/cache.rs`

- [ ] **Step 1: Implement `Kept` and `keep_alive`**

Add to `crates/yomu-ui/src/cache.rs`:

```rust
/// What a page renders from: the payload, and an error that never
/// replaces a payload already on screen.
#[derive(Clone, Copy)]
pub struct Kept<V: Send + Sync + 'static> {
    pub value: RwSignal<Option<V>>,
    pub error: RwSignal<Option<String>>,
}

/// Wire a cache to a page: seed from the cache, fetch only on a trigger,
/// and keep both in step.
///
/// `refresh` is the page's existing counter — every current caller (a
/// mutation, the manga page's download poll, pull-to-refresh) keeps
/// working by bumping it.
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
    let effect_key = key.clone();
    Effect::new(move |prev: Option<()>| {
        let online = conn.get() == crate::Connectivity::Online;
        let bump = refresh.get();
        let refreshed = bump != last_refresh.get_value();
        last_refresh.set_value(bump);
        let reconnected = came_online(online, was_online.get_value(), prev.is_none());
        was_online.set_value(online);

        let answer = cache.answer(&effect_key);
        // Seed the view from whatever the cache holds, before deciding.
        if let Some(value) = answer.clone() {
            kept.value.set(Some(value));
        }
        let what = decide(answer.is_some(), cache.is_stale(), refreshed, reconnected);
        if what == Fetch::No {
            return;
        }
        let key = effect_key.clone();
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors (warnings about the unused `keep_alive` are expected until Task 5).

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-ui/src/cache.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): keep_alive, the one call a page makes

Seeds the view from the cache, fetches only when decide() says so, and
holds errors in their own signal so a failed refresh leaves the list
being read on screen instead of emptying it.

The page's existing refresh counter is the trigger, so every current
caller keeps working untouched: category edits, the manga page's
download poll, and shortly pull-to-refresh.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 4: Provide the caches in `App`

**Files:**
- Modify: `crates/yomu-ui/src/cache.rs` (aliases + accessors)
- Modify: `crates/yomu-ui/src/lib.rs:170-186` (`App`)

- [ ] **Step 1: Add the aliases and accessors**

Add to `crates/yomu-ui/src/cache.rs`:

```rust
use uuid::Uuid;
use yomu_domain::{Category, PublicationDetailResponse, PublicationWithLocator};

/// The library list, shared by the Library and Home pages — they fetch the
/// same thing, so switching between them costs nothing.
pub type LibraryCache = Keyed<(), Vec<PublicationWithLocator>>;
/// The category list, shared by the Library and manga pages.
pub type CategoriesCache = Keyed<(), Vec<Category>>;
/// One open publication. The bool is the served-from-cache flag that tells
/// rows which chapters will not open.
pub type DetailCache = Keyed<Uuid, (PublicationDetailResponse, bool)>;

pub fn use_library_cache() -> LibraryCache {
    use_context().expect("LibraryCache provided by App")
}

pub fn use_categories_cache() -> CategoriesCache {
    use_context().expect("CategoriesCache provided by App")
}

pub fn use_detail_cache() -> DetailCache {
    use_context().expect("DetailCache provided by App")
}

/// Everything a change to one publication can invalidate. Called from
/// mutation sites, which is why it takes no arguments: the caches are
/// contexts, and the caller must already hold them (a `use_context` after
/// an await returns None silently).
pub fn mark_publication_stale(library: LibraryCache, detail: DetailCache) {
    library.mark_stale();
    detail.mark_stale();
}
```

- [ ] **Step 2: Provide them**

In `crates/yomu-ui/src/lib.rs`, after `provide_context(pull_queue);` (line 181):

```rust
    // Page data that outlives the page component: a return to a list is
    // free (see cache.rs).
    provide_context(cache::LibraryCache::default());
    provide_context(cache::CategoriesCache::default());
    provide_context(cache::DetailCache::default());
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-ui/src/cache.rs crates/yomu-ui/src/lib.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): provide the three page caches above the router

Library and Home fetch the same library() list, so one cache serves both
and switching between them costs nothing. Route changes destroy the
component, not the context.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 5: Rewire the Library page

**Files:**
- Modify: `crates/yomu-ui/src/pages/library.rs:9-105`

- [ ] **Step 1: Replace the two resources**

In `crates/yomu-ui/src/pages/library.rs`, replace the `library` and `categories` `LocalResource` declarations (lines 15-40) with:

```rust
    let library = crate::cache::keep_alive(
        crate::cache::use_library_cache(),
        (),
        refresh,
        conn,
        {
            let client = client.clone();
            move || {
                let client = client.clone();
                async move {
                    offline::cached(conn, "library", || client.library())
                        .await
                        .map(|(value, _)| value)
                }
            }
        },
    );
    let categories = crate::cache::keep_alive(
        crate::cache::use_categories_cache(),
        (),
        refresh,
        conn,
        {
            let client = client.clone();
            move || {
                let client = client.clone();
                async move {
                    offline::cached(conn, "categories", || client.categories())
                        .await
                        .map(|(value, _)| value)
                }
            }
        },
    );
```

- [ ] **Step 2: Update every read of the two**

`Kept::value` is `RwSignal<Option<V>>`, not `Option<Result<V, _>>`. Apply these exact substitutions in `crates/yomu-ui/src/pages/library.rs`:

- Line 47 (cover sweep): `if let Some(Ok(entries)) = library.get()` → `if let Some(entries) = library.value.get()`
- Line 61 (category seeding): `let Some(Ok(list)) = categories.get() else` → `let Some(list) = categories.value.get() else`
- Line 72 (kind fallback): `if let Some(Ok(entries)) = library.get()` → `if let Some(entries) = library.value.get()`
- Lines 88, 96, 101: `library.get().and_then(|r| r.ok()).unwrap_or_default()` → `library.value.get().unwrap_or_default()`
- Line 92: `categories.get().and_then(|r| r.ok()).map(|list| {` → `categories.value.get().map(|list| {`

- [ ] **Step 3: Update the main match**

Replace the match at line 105 (`{move || match library.get() {`) with:

```rust
            {move || library.error.get().map(|err| view! {
                <p class="error">"Could not reach yomu server: " {err}</p>
            })}
            {move || match library.value.get() {
                None => view! { <p class="muted">"Loading library…"</p> }.into_any(),
                Some(list) if list.is_empty() => {
```

and change the following `Some(Ok(list)) => {` arm to `Some(list) => {`. The `None`/error split is now: no payload and no error means loading; an error is rendered *beside* whatever is on screen, never instead of it.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-ui/src/pages/library.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): the library grid survives leaving the tab

Returning no longer refetches or flashes a loading state. A failed
refresh now shows its error above the grid rather than replacing it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 6: Rewire the Home page

**Files:**
- Modify: `crates/yomu-ui/src/pages/home.rs:11-50`

- [ ] **Step 1: Add a refresh counter and replace the resource**

In `crates/yomu-ui/src/pages/home.rs`, replace the `library` `LocalResource` (lines 17-28) with:

```rust
    let refresh = RwSignal::new(0u32);
    let library = crate::cache::keep_alive(
        crate::cache::use_library_cache(),
        (),
        refresh,
        conn,
        {
            let client = client.clone();
            move || {
                let client = client.clone();
                async move {
                    offline::cached(conn, "library", || client.library())
                        .await
                        .map(|(value, _)| value)
                }
            }
        },
    );
```

- [ ] **Step 2: Update every read**

- Line 34 (cover sweep): `if let Some(Ok(entries)) = library.get()` → `if let Some(entries) = library.value.get()`
- Line 43: `{move || match library.get() {` → `{move || match library.value.get() {`
- Line 45: delete the `Some(Err(err)) => …` arm; it is replaced by the error banner below.
- Line 49: `Some(Ok(list)) if list.is_empty()` → `Some(list) if list.is_empty()`
- Line 58: `Some(Ok(list)) => {` → `Some(list) => {`

Directly after `<section class="home">`, add the banner:

```rust
            {move || library.error.get().map(|err| view! {
                <p class="error">"Could not reach yomu server: " {err}</p>
            })}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-ui/src/pages/home.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): home shelves survive leaving the tab

Home reads the same library() list as the Library page, so sharing one
cache makes the switch between them free in both directions and halves
what a launch costs.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 7: Rewire the manga page

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs:50-79`, and every read of `detail`/`categories`

- [ ] **Step 1: Replace the two resources**

In `crates/yomu-ui/src/pages/manga.rs`, replace the `detail` and `categories` `LocalResource` declarations (lines 50-78) with:

```rust
    let detail = crate::cache::keep_alive(
        crate::cache::use_detail_cache(),
        id,
        refresh,
        conn,
        {
            let client = client.clone();
            move || {
                let client = client.clone();
                async move {
                    // The flag marks the detail as served-from-cache
                    // (server unreachable) so rows can show which chapters
                    // won't open.
                    offline::cached(conn, &format!("manga:{id}"), || client.publication(id)).await
                }
            }
        },
    );
    // Which categories the updater checks is configured on the library page.
    let categories = crate::cache::keep_alive(
        crate::cache::use_categories_cache(),
        (),
        refresh,
        conn,
        {
            let client = client.clone();
            move || {
                let client = client.clone();
                async move {
                    offline::cached(conn, "categories", || client.categories())
                        .await
                        .map(|(value, _)| value)
                }
            }
        },
    );
```

The comment about hoisting `categories` out of `MangaDetail` to avoid a flicker (lines 63-68) is now obsolete: a background refetch no longer blanks the value. Replace it with the one-line comment shown above.

- [ ] **Step 2: Update every read of `detail` and `categories`**

Find them with:

```bash
grep -n "detail\.get()\|categories\.get()\|detail:.*LocalResource\|categories:.*LocalResource" crates/yomu-ui/src/pages/manga.rs
```

Apply mechanically:
- `detail.get().and_then(|r| r.ok())` → `detail.value.get()`
- `match detail.get() { None => …, Some(Err(err)) => …, Some(Ok((d, cached))) => … }` → keep `None` as the loading arm, drop the `Err` arm, and use `Some((d, cached))`; render `detail.error.get()` as a banner in the same place the `Err` arm rendered.
- The `categories: LocalResource<Result<Vec<Category>, yomu_client::ClientError>>` parameter on the child component (line 202) becomes `categories: crate::cache::Kept<Vec<Category>>`, and its reads become `categories.value.get()`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-ui/src/pages/manga.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): the chapter list survives leaving the page

Keyed by publication id, so opening another title still fetches — the
one thing a shared cache must never get wrong.

This also removes a flicker that predates the change: a LocalResource
refetch yields None first, so the category select unmounted for a beat
on every download poll. A background refetch keeps the value.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 8: Patch the locator from the reader

**Files:**
- Modify: `crates/yomu-ui/src/cache.rs` (the patch function + tests)
- Modify: `crates/yomu-ui/src/pages/reader/mod.rs:129-155`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/yomu-ui/src/cache.rs`:

```rust
    use uuid::Uuid;
    use yomu_domain::{Kind, Locations, Locator, Publication, PublicationWithLocator};

    fn entry(id: Uuid) -> PublicationWithLocator {
        PublicationWithLocator {
            publication: Publication {
                id,
                source_id: "local".into(),
                external_id: id.to_string(),
                title: "A publication".into(),
                kind: Kind::Comics,
                category: "reading".into(),
                ..Default::default()
            },
            locator: None,
            unit_count: 3,
            unread_count: 3,
            downloaded_count: 0,
            latest_unit_at: None,
            locator_unit_title: None,
        }
    }

    fn at() -> chrono::DateTime<chrono::Utc> {
        "2026-07-30T00:00:00Z".parse().unwrap()
    }

    /// Returning from the reader must show the position just read, with no
    /// fetch: the client knows it exactly, so it is written in rather than
    /// waited for.
    #[test]
    fn a_reading_position_is_written_into_the_library_entry() {
        let mine = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let library: LibraryCache = Keyed::default();
        library.store((), vec![entry(other), entry(mine)]);

        let locator = Locator {
            unit_id: Uuid::from_u128(9),
            locations: Locations::Page { page: 4 },
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
```

If `Publication` has no `Default`, build it field by field instead of using `..Default::default()` — check with `grep -n "pub struct Publication" -A 40 crates/yomu-domain/src/publication.rs`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yomu-ui cache::`
Expected: FAIL to compile — `cannot find function patch_locator`.

- [ ] **Step 3: Implement the patch**

Add to `crates/yomu-ui/src/cache.rs`:

```rust
/// Write a reading position into the library list without a fetch.
///
/// The locator is exact — the reader knows the unit and page — so it is
/// patched. `unread_count` is not: `set_position` also folds the position
/// into read marks server-side (`api/progress.rs`, `auto_mark_read`), by a
/// reading-order rule the client must not duplicate. So the cache is also
/// flagged stale and the next view corrects the counts behind the list.
pub fn patch_locator(
    library: LibraryCache,
    publication_id: Uuid,
    locator: &Locator,
    unit_title: Option<String>,
) {
    library.patch(&(), |list| {
        if let Some(entry) = list
            .iter_mut()
            .find(|e| e.publication.id == publication_id)
        {
            entry.locator = Some(locator.clone());
            if unit_title.is_some() {
                entry.locator_unit_title = unit_title.clone();
            }
        }
    });
    library.mark_stale();
}

/// The same, for the open publication's detail.
pub fn patch_detail_locator(detail: DetailCache, publication_id: Uuid, locator: &Locator) {
    detail.patch(&publication_id, |(value, _)| {
        value.locator = Some(locator.clone());
    });
    detail.mark_stale();
}
```

Add `Locator` to the `yomu_domain` import at the top of the file.

- [ ] **Step 4: Run the test**

Run: `cargo test -p yomu-ui cache::`
Expected: PASS, 9 tests.

- [ ] **Step 5: Call it from the reader**

In `crates/yomu-ui/src/pages/reader/mod.rs`, the `report` closure (lines 130-155). The caches must be read **before** the `spawn_local`: a `use_context` after an await returns `None` silently — no panic, no compile error, just a patch that never happens (auth playbook §7.1). `Keyed` is `Copy`, so capturing it is free.

```rust
    // Read during setup, not inside the task: a context lookup after an
    // await has no reactive owner and silently yields None.
    let library_cache = crate::cache::use_library_cache();
    let detail_cache = crate::cache::use_detail_cache();
    let report = {
        let client = client.clone();
        move |unit: uuid::Uuid, p: u32| {
            // Built client-side rather than from the response, so the
            // position is right even when the write fails and lands in the
            // outbox — the offline path stays correct.
            let locator = yomu_domain::Locator {
                unit_id: unit,
                locations: yomu_domain::Locations::Page { page: p },
                at: Utc::now(),
            };
            let unit_title = detail
                .get()
                .and_then(|r| r.ok())
                .and_then(|d| d.units.iter().find(|c| c.id == unit).map(|c| c.title.clone()));
            crate::cache::patch_locator(library_cache, manga_id, &locator, unit_title);
            crate::cache::patch_detail_locator(detail_cache, manga_id, &locator);
            let client = client.clone();
            spawn_local(async move {
                let req = SetLocatorRequest {
                    unit_id: unit,
                    page: p,
                    device: "web".into(),
                };
                if client.set_locator(manga_id, &req).await.is_err() {
                    offline::outbox_push(ProgressEvent {
                        id: offline::uuid_v7_js(),
                        publication_id: manga_id,
                        unit_id: unit,
                        page: p,
                        device: "web-offline".into(),
                        at: Utc::now(),
                    });
                }
            });
        }
    };
```

The reader's own `detail` is still a `LocalResource` (the reader is not cached by this work), so `detail.get().and_then(|r| r.ok())` is correct there. Confirm the unit title field name with `grep -n "pub struct ReadingUnit" -A 12 crates/yomu-domain/src/publication.rs`.

- [ ] **Step 6: Verify it compiles**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown && cargo test -p yomu-ui`
Expected: no errors; all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/yomu-ui/src/cache.rs crates/yomu-ui/src/pages/reader/mod.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): a reading position reaches the lists without a fetch

With no refetch on return, a position the caches never heard about is a
lie on screen. The locator is exact, so the reader writes it straight
into the cached library entry and detail — built client-side, so it is
also right when the write fails and lands in the outbox.

The unread count is not exact: set_position folds the position into read
marks server-side by reading order, a rule the client must not
duplicate. So the caches are flagged stale and the next visit corrects
the counts quietly behind the list already on screen.

The caches are captured during setup, not inside the spawned task: a
context read after an await returns None with no panic and no compile
error, and the patch would simply never happen.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 9: Flag the remaining mutations

Every mutation the caches do not hear about is a stale list that never corrects itself. The manga page's own mutations already bump its `refresh`, which refetches the detail — but not the library.

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs` (mark, update, download, delete, refresh_publication)
- Modify: `crates/yomu-ui/src/pages/library.rs` (update_category)
- Modify: `crates/yomu-ui/src/pages/search.rs` (add_publication)
- Modify: `crates/yomu-ui/src/pages/downloads.rs` (retry/remove/dismiss)
- Modify: `crates/yomu-ui/src/offline.rs` (outbox and mark flushes)

- [ ] **Step 1: List the sites**

```bash
grep -rn "client\.\(mark_units\|update_publication\|update_category\|add_publication\|delete_publication\|refresh_publication\|download_units\|retry_downloads\|remove_downloads\|dismiss_downloads\)(" crates/yomu-ui/src --include=*.rs
```

Expected: the twelve sites from the spec's table.

- [ ] **Step 2: Flag at each site**

At the top of each component that mutates, capture the caches during setup:

```rust
    let library_cache = crate::cache::use_library_cache();
    let detail_cache = crate::cache::use_detail_cache();
```

then, in the success path of each mutation (inside the spawned task, using the captured `Copy` values):

- `mark_units`, `update_publication`, `delete_publication`, `refresh_publication`, `add_publication`:
  ```rust
  crate::cache::mark_publication_stale(library_cache, detail_cache);
  ```
- `update_category` (library page):
  ```rust
  categories_cache.mark_stale();
  library_cache.mark_stale();
  ```
- `download_units`, `retry_downloads`, `remove_downloads`, `dismiss_downloads`:
  ```rust
  detail_cache.mark_stale();
  ```

`delete_publication` also removes a row that is still cached, so it clears rather than flags:

```rust
  library_cache.clear();
  detail_cache.clear();
```

- [ ] **Step 3: Flag the offline flushes**

`offline::flush_outbox` and `offline::flush_marks` (`crates/yomu-ui/src/offline.rs:680-705`) are called from `App`'s effect and reconcile server state after a reconnection. They are plain async functions with no reactive owner, so they must not call `use_context`. Give each an extra parameter and pass the captured caches from `App`:

```rust
pub async fn flush_marks(client: &YomuClient, library: crate::cache::LibraryCache, detail: crate::cache::DetailCache) {
    // … existing body …
    if !flushed.is_empty() {
        crate::cache::mark_publication_stale(library, detail);
        // … existing localStorage cleanup …
    }
}
```

and the same shape for `flush_outbox`. In `crates/yomu-ui/src/lib.rs`, the flush effect (lines 192-201) captures both caches before its `spawn_local` and passes them in.

- [ ] **Step 4: Verify**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown && cargo test -p yomu-ui`
Expected: no errors; tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-ui/src
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): every mutation tells the caches

With no refetch on return, a mutation the caches never heard about is a
stale list that never corrects itself — the failure mode is silent,
which is why all twelve client mutations are covered rather than the
obvious few.

Marks, updates and the offline flushes flag stale, so the next visit
paints instantly and corrects behind the list. Deleting a publication
clears instead: the row is gone, and showing it until a refetch lands
would be worse than a spinner.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 10: Pull-to-refresh

**Files:**
- Create: `crates/yomu-ui/src/refresh.rs`
- Modify: `crates/yomu-ui/src/lib.rs` (module declaration)

- [ ] **Step 1: Declare the module**

In `crates/yomu-ui/src/lib.rs`, add `mod refresh;` after `mod pull;`. (`pull.rs` is the device-pull download queue and is unrelated — hence the separate name.)

- [ ] **Step 2: Write the failing tests**

Create `crates/yomu-ui/src/refresh.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Damped, so the indicator feels weighted rather than glued to the
    /// thumb, and never negative on an upward drag.
    #[test]
    fn travel_is_damped_and_never_negative() {
        assert_eq!(travel(0.0), 0.0);
        assert_eq!(travel(100.0), 45.0);
        assert_eq!(travel(-40.0), 0.0);
    }

    /// Only past the threshold does letting go refresh.
    #[test]
    fn arming_needs_the_threshold() {
        assert!(!armed(travel(100.0)));
        assert!(armed(travel(200.0)));
    }

    /// Swiping down mid-list must not refresh — only a pull from the very
    /// top arms the gesture.
    #[test]
    fn only_the_top_of_the_page_arms_the_gesture() {
        assert!(can_start(0.0));
        assert!(can_start(0.4));
        assert!(!can_start(120.0));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p yomu-ui refresh::`
Expected: FAIL to compile — `cannot find function travel`.

- [ ] **Step 4: Implement the hook**

Prepend to `crates/yomu-ui/src/refresh.rs`:

```rust
//! Pull-to-refresh. Touch-only by design: on the web and the desktop shell
//! a page reload is the refresh, and a mouse never triggers this.
//!
//! Not to be confused with `pull.rs`, which drains the device-download
//! queue.

use leptos::ev;
use leptos::prelude::*;

/// Resistance, so the indicator trails the thumb rather than tracking it.
const DAMPING: f64 = 0.45;
/// How far the damped travel must reach before letting go refreshes.
const THRESHOLD: f64 = 72.0;
/// Anything above this is "not at the top": a swipe down mid-list must
/// scroll, not refresh.
const TOP: f64 = 0.5;

fn travel(raw: f64) -> f64 {
    (raw * DAMPING).max(0.0)
}

fn armed(travel: f64) -> bool {
    travel >= THRESHOLD
}

fn can_start(scroll_y: f64) -> bool {
    scroll_y <= TOP
}

/// What the indicator draws from.
#[derive(Clone, Copy)]
pub struct PullState {
    /// Damped pixels the list has been dragged down. 0 when idle.
    pub distance: RwSignal<f64>,
    /// A refresh is running; keep the spinner up.
    pub refreshing: RwSignal<bool>,
    /// Far enough that letting go will refresh.
    pub armed: RwSignal<bool>,
}

/// Listen on `window`, so the gesture works whatever actually scrolls.
///
/// `window_event_listener` + `on_cleanup` rather than a hand-rolled
/// `Closure`: `Closure` is neither `Send` nor `Sync`, so it cannot live in
/// a `StoredValue`, and rolling one reimplements what leptos provides.
pub fn use_pull_to_refresh(on_refresh: impl Fn() + Copy + 'static) -> PullState {
    let state = PullState {
        distance: RwSignal::new(0.0),
        refreshing: RwSignal::new(false),
        armed: RwSignal::new(false),
    };
    let origin = StoredValue::new(None::<f64>);

    let start = window_event_listener(ev::touchstart, move |e| {
        let Some(touch) = e.touches().get(0) else {
            return;
        };
        origin.set_value(
            can_start(window().scroll_y().unwrap_or(0.0)).then(|| touch.client_y() as f64),
        );
    });
    let move_ = window_event_listener(ev::touchmove, move |e| {
        let (Some(from), Some(touch)) = (origin.get_value(), e.touches().get(0)) else {
            return;
        };
        let d = travel(touch.client_y() as f64 - from);
        state.distance.set(d);
        state.armed.set(armed(d));
    });
    let end = window_event_listener(ev::touchend, move |_| {
        origin.set_value(None);
        let fire = state.armed.get_untracked() && !state.refreshing.get_untracked();
        state.distance.set(0.0);
        state.armed.set(false);
        if fire {
            state.refreshing.set(true);
            on_refresh();
        }
    });
    on_cleanup(move || {
        start.remove();
        move_.remove();
        end.remove();
    });
    state
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yomu-ui refresh::`
Expected: PASS, 3 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/yomu-ui/src/refresh.rs crates/yomu-ui/src/lib.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): pull-to-refresh

Once a return no longer refreshes, this is how a refresh is asked for.
Touch-only, so the web UI and desktop shell are untouched — there F5 is
the refresh.

Armed only from the very top of the page, or a swipe down mid-list would
refresh instead of scrolling.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 11: Wire the gesture and its indicator

**Files:**
- Modify: `crates/yomu-ui/src/pages/library.rs`, `home.rs`, `manga.rs`
- Modify: `crates/yomu-web/styles.css`

- [ ] **Step 1: Wire each page**

In each of the three pages, after the `refresh` signal exists:

```rust
    let pull = crate::refresh::use_pull_to_refresh(move || refresh.update(|n| *n += 1));
```

and, as the first child of the page's root element:

```rust
            <div
                class="pull-refresh"
                class:armed=move || pull.armed.get()
                class:spinning=move || pull.refreshing.get()
                style:height=move || format!("{}px", pull.distance.get())
            >
                <span class="pull-refresh-dot"></span>
            </div>
```

Clear `refreshing` when the fetch lands: add to each page, after the `keep_alive` call,

```rust
    Effect::new(move |_| {
        // Any settled outcome ends the spinner — a failed refresh must not
        // leave it spinning forever.
        let _ = library.value.get();
        let _ = library.error.get();
        pull.refreshing.set(false);
    });
```

using `detail` in place of `library` on the manga page.

- [ ] **Step 2: Style it**

Append to `crates/yomu-web/styles.css`:

```css
/* Pull-to-refresh: height is driven inline from the gesture. */
.pull-refresh {
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  height: 0;
}

.pull-refresh-dot {
  width: 20px;
  height: 20px;
  border-radius: 50%;
  border: 2px solid var(--muted);
  border-top-color: transparent;
  opacity: 0.5;
  transition: opacity 120ms ease;
}

.pull-refresh.armed .pull-refresh-dot {
  opacity: 1;
}

.pull-refresh.spinning {
  height: 32px !important;
}

.pull-refresh.spinning .pull-refresh-dot {
  opacity: 1;
  animation: pull-spin 700ms linear infinite;
}

@keyframes pull-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .pull-refresh.spinning .pull-refresh-dot {
    animation: none;
    border-top-color: var(--muted);
  }
}
```

Confirm `--muted` exists with `grep -n "\-\-muted" crates/yomu-web/styles.css`; if it does not, use the variable the file already uses for secondary text.

- [ ] **Step 3: Verify**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-ui/src crates/yomu-web/styles.css
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): wire pull-to-refresh into the three lists

Any settled outcome ends the spinner, success or failure — a refresh
that fails must not leave it turning.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 12: Shared scroll restoration

**Files:**
- Modify: `crates/yomu-ui/src/cache.rs` (the helper)
- Modify: `crates/yomu-ui/src/pages/manga.rs:83-115` (use it), `library.rs`, `home.rs` (gain it)

- [ ] **Step 1: Extract the helper**

Add to `crates/yomu-ui/src/cache.rs`:

```rust
/// Return to where the list was left.
///
/// Saved from scroll events as they happen, not in `on_cleanup`: that runs
/// after navigation has already reset scroll to 0 (and that reset's own
/// event fires asynchronously, past the listener's removal, so it cannot
/// clobber the recording). `ready` gates the restore until there is
/// content to scroll through.
pub fn remember_scroll(key: String, ready: impl Fn() -> bool + 'static) {
    let save_key = key.clone();
    let save = leptos::prelude::window_event_listener(leptos::ev::scroll, move |_| {
        if let Some(storage) = window().session_storage().ok().flatten() {
            let y = window().scroll_y().unwrap_or(0.0);
            let _ = storage.set_item(&save_key, &y.to_string());
        }
    });
    leptos::prelude::on_cleanup(move || save.remove());

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
```

- [ ] **Step 2: Use it on the manga page**

Replace `crates/yomu-ui/src/pages/manga.rs:83-115` (the `scroll_key` block and the `restored` effect) with:

```rust
    // Coming back from the reader must land where the list was left.
    crate::cache::remember_scroll(format!("yomu-scroll:manga:{id}"), move || {
        detail.value.get().is_some()
    });
```

- [ ] **Step 3: Add it to Library and Home**

In `crates/yomu-ui/src/pages/library.rs`, after the `keep_alive` calls:

```rust
    crate::cache::remember_scroll("yomu-scroll:library".to_string(), move || {
        library.value.get().is_some()
    });
```

In `crates/yomu-ui/src/pages/home.rs`, after the `keep_alive` call:

```rust
    crate::cache::remember_scroll("yomu-scroll:home".to_string(), move || {
        library.value.get().is_some()
    });
```

- [ ] **Step 4: Verify**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-ui/src
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(ui): the library and home shelves keep their scroll position

The manga page has done this since the reader shipped; lifting it into a
shared helper gives the other two lists the same behaviour without a
third copy. Manga behaviour is unchanged.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 13: Full gate

- [ ] **Step 1: Run the workspace check**

Run: `just check`
Expected: fmt clean, clippy clean with `-D warnings`, wasm check passes for `yomu-web` and `yomu-ui`.

- [ ] **Step 2: Run the shell check**

Run: `nix develop .#tauri --command just check-shell`
Expected: `yomu-shell` compiles.

- [ ] **Step 3: Fix anything the gate finds, then commit**

```bash
git add -A crates/
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
chore(ui): satisfy fmt and clippy for the keep-alive work

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

Skip this commit if the gate was already clean.

---

## Manual verification (on device, after merge)

The behaviours this plan changes cannot be unit-tested in wasm. After deploying:

1. Library → a title → back. The grid appears instantly, with no loading text.
2. Home → Library → Home. Neither flashes; the network tab shows no `/api/v1/library` call after the first.
3. Open title A, back, open title B. B shows **B's** chapters, not A's.
4. Read a chapter to the end, back out. The chapter list shows the new position immediately; the unread count and read ticks correct themselves a beat later.
5. Pull down from the top of the library. The dot appears, firms up past ~72px, spins on release, and stops when the fetch lands.
6. Swipe down from the middle of a long chapter list. It scrolls; nothing refreshes.
7. Turn the server off, pull to refresh. The list stays on screen with an error above it — it must not empty.
