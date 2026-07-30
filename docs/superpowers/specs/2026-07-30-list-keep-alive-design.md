# Keeping lists alive across navigation

Returning to the library, the home shelves or a chapter list re-fetches and
flashes a loading state every time. The data was already on screen a second
ago. This makes a return free, and adds the one gesture that becomes
necessary once returning no longer refreshes.

## The problem, precisely

The router drops and rebuilds the page component on every visit, so a
`LocalResource` declared inside it is new each time and fetches from cold.
Nothing is wrong with the resource; it is scoped to the wrong lifetime.

`offline::cached` already persists every one of these lists to localStorage,
but only *reads* that copy when the app is offline or the request fails. On
the happy path the bytes for an instant paint are on disk and unused.

Affected: `pages/library.rs` (library + categories), `pages/home.rs`
(library), `pages/manga.rs` (detail + categories). Home and Library fetch the
same `library()` list, so one shared cache serves both and a Home↔Library
switch costs nothing.

## Decisions

- **A return does not re-fetch.** Not "fetch quietly in the background" —
  no request at all. A refetch happens only on a real trigger.
- **Pull-to-refresh is the manual trigger.** Touch-only, so the web UI and
  desktop shell are unaffected; there a page reload is the refresh.
- **A cold start still fetches.** Seeding the in-memory cache from
  localStorage at launch is deliberately out of scope.

## 1. The cache

New `crates/yomu-ui/src/cache.rs`. One generic single-entry cache, so the
"can this answer the key I want?" rule is written and tested once:

```rust
#[derive(Clone, Copy)]
pub struct Keyed<K: 'static, V: 'static> {
    slot: RwSignal<Option<(K, V)>>,
    stale: RwSignal<bool>,
}
```

- `answer(&self, key: &K) -> Option<V>` — `Some` only when the stored key
  equals the wanted one.
- `store(&self, key: K, value: V)` — stores and clears `stale`.
- `patch(&self, key: &K, f: impl FnOnce(&mut V))` — edit in place if the key
  matches; no-op otherwise.
- `mark_stale(&self)` / `is_stale(&self)`.
- `clear(&self)`.

Single-entry is deliberate: opening title B evicts title A, which bounds
memory and matches how the app is actually used.

Storing the **key alongside the payload** is the point. Without it, opening
title B shows title A's chapters — the failure this type exists to prevent.

Provided once in `App` (`lib.rs`), beside the existing contexts, as three
aliases (distinct types, so `use_context` resolves each unambiguously):

```rust
pub type LibraryCache    = Keyed<(), Vec<PublicationWithLocator>>;
pub type CategoriesCache = Keyed<(), Vec<Category>>;
pub type DetailCache     = Keyed<Uuid, (PublicationDetailResponse, bool)>;
```

The `bool` in `DetailCache` is the existing served-from-cache flag that tells
rows which chapters will not open.

Route changes destroy components, not contexts.

## 2. When a page fetches

Each page drops its `LocalResource` for a view signal plus one effect. The
decision is a pure function:

```rust
pub enum Fetch { No, Background, Blocking }

pub fn decide(answered: bool, stale: bool, refreshed: bool, came_online: bool) -> Fetch {
    match (answered, stale || refreshed || came_online) {
        (false, _)   => Fetch::Blocking,   // nothing to show; spinner is honest
        (true, true) => Fetch::Background, // show what we have, correct it quietly
        (true, false) => Fetch::No,
    }
}
```

- **Blocking** renders the existing loading state.
- **Background** leaves the cached list on screen and swaps the payload in
  when it lands.
- `refreshed` is the page's existing `refresh` counter changing, which keeps
  every current caller working untouched: the library's category edits, the
  manga page's mutations, its 2s download poll, and pull-to-refresh. It also
  removes a flicker that exists today — a `LocalResource` refetch yields
  `None` first, so the manga page's category select currently unmounts for a
  beat on every poll (the reason `categories` was hoisted out of
  `MangaDetail`).
- `came_online` is an Offline→**Online** transition, not "we are online":

```rust
let was_online = StoredValue::new(false);
Effect::new(move |prev: Option<()>| {
    let online = conn.get() == Connectivity::Online;
    // `prev.is_some()`: was_online starts false on every mount, so without
    // this guard every visit looks like a reconnection and re-fetches —
    // which silently undoes this entire change.
    let came_online = online && !was_online.get_value() && prev.is_some();
    was_online.set_value(online);
    // … decide(), then fetch
});
```

The fetch itself still goes through `offline::cached`, so offline reads and
the Offline downgrade rule are unchanged.

**Errors get their own signal.** A failed background refresh leaves the
cached list on screen and shows the error beside it; only a failed blocking
fetch renders the error in place of content. A refresh that fails must never
empty a list being read.

## 3. What a mutation does

With no refetch on return, a mutation that the cache does not hear about is
a lie on screen. Two treatments, and the split is deliberate:

**Patched exactly** — the client knows the new value:

| Event | Effect |
| --- | --- |
| Reader reports a position | write the `Locator` and `locator_unit_title` into the cached library entry and the cached detail |

**Flagged stale** — the server derives something the client should not
recompute; the next visit paints from cache and corrects in the background:

| Event | Caches |
| --- | --- |
| Reader reports a position | library + detail (see below) |
| `mark_units` | library + detail |
| `update_publication`, `add_publication`, `delete_publication`, `refresh_publication` | library + detail |
| `update_category` | categories + library |
| `download_units`, `retry_downloads`, `remove_downloads`, `dismiss_downloads` | detail |
| Offline outbox flush (`offline.rs` marks/progress) | library + detail |

A reading session appears in both tables, and that is the interesting case.
`set_position` also calls `auto_mark_read` (`api/progress.rs:41`), which
folds the position into read marks by reading order — so finishing a chapter
changes the locator *and* `unread_count`. The locator is exact, so it is
patched and the list is correct the instant you return. `unread_count`
follows a server rule the client must not duplicate, so it is refreshed
quietly a moment later.

The patched `Locator` is built client-side from `(unit_id, page, now)`
rather than from the response, so it also applies when the write fails and
lands in the outbox — the offline path stays correct.

**Capture the caches before spawning.** The reader's report closure spawns a
task and awaits before it would touch a cache, and a `use_context` after an
await returns `None` silently — no panic, no compile error, just a patch that
never happens. `Keyed` is `Copy`, so it is read during component setup and
moved into the task. Every call site in this spec follows that rule.

## 4. Pull-to-refresh

New `crates/yomu-ui/src/refresh.rs` — `pull.rs` is taken by the device-pull
download queue and is unrelated.

`use_pull_to_refresh(on_refresh) -> PullState { distance, refreshing }`,
generic over the refresh future, wired on Library, Home and the chapter list;
each page's handler bumps its `refresh` counter.

- `window_event_listener(leptos::ev::touchstart|touchmove|touchend, …)` with
  `on_cleanup(|| handle.remove())`. Not a hand-rolled `Closure` +
  `add_event_listener`: `Closure` is neither `Send` nor `Sync`, so it cannot
  live in a `StoredValue`, and it reimplements what leptos already provides.
- Listen on `window`, not a container, so it works whatever actually scrolls.
- Arm only when `scroll_y() <= 0.5`, or a downward swipe mid-list refreshes.
- Damping 0.45 and a 72px threshold, so it feels weighted rather than glued
  to the thumb.
- Touch events only. A mouse never triggers it.

`crates/yomu-ui/Cargo.toml` already enables the `Touch`, `TouchEvent` and
`TouchList` web-sys features. `pager.rs` already does touch handling and is
the local convention to match.

CSS in `crates/yomu-web/styles.css` under `.pull-refresh`: height driven
inline from the gesture, `.armed` signals "let go now", `.spinning` animates,
with a `prefers-reduced-motion` fallback.

## 5. Scroll restoration

`manga.rs:83-115` already saves scroll position to sessionStorage from scroll
events (not `on_cleanup`, which runs after navigation has reset scroll to 0)
and restores it in a `request_animation_frame` once content exists. Library
and Home have nothing.

Lift that into `cache::remember_scroll(key)` and call it from all three
pages. Behaviour on the manga page is unchanged; the extraction is what makes
it available to the other two without a third copy.

## 6. Testing

Unit tests, no browser needed:

- `Keyed`: a matching key answers, a different key does not, an empty cache
  does not; `store` clears `stale`; `patch` edits only on a key match.
- `decide`: every arm, and specifically that a plain revisit while online
  returns `No` — that case goes red if the `prev.is_some()` guard is dropped.
- The locator patch: a cached library entry for the read publication gets the
  new locator and title; an entry for a different publication is untouched.
- Pull gesture arithmetic: damping applied to raw travel, `armed` only past
  the threshold, and no arming when `scroll_y > 0.5`.

Each must fail if its guard is removed. Wiring beyond this is covered by
`just check`.

## Out of scope

- **Seeding the cache from localStorage at launch.** A cold start still
  fetches with a loading state.
- **A desktop/web refresh control.** The gesture is touch-only by design; F5
  is the refresh there.
- **Caching search, sources and downloads pages.** Those are live views where
  a fetch on entry is the correct behaviour.
