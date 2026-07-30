# Porting chaos' list keep-alive + pull-to-refresh to yomu

Written 2026-07-29. Deliberately **outside** `/projects/rust/yomu` — this is a
handover note for a future conversation, not a file to commit there.

Target in yomu: the chapter list and the title list, which re-fetch every time
you navigate back, the same way chaos' News tab did.

## The problem, precisely

The router drops and rebuilds the page component on every visit. Any
`LocalResource::new(...)` declared inside that component is therefore new each
time, so it re-fetches from cold and flashes a loading state. Nothing is wrong
with the resource; it is simply scoped to the wrong lifetime.

## The fix, in three parts

### 1. Move the payload above the router

A `Copy` context struct provided once in `App`, holding the loaded data and the
scroll offset. Route changes destroy the *component*, not the context.

chaos, `crates/chaos-ui/src/lib.rs`:

```rust
#[derive(Clone, Copy, Default)]
pub struct NewsCache {
    pub loaded: RwSignal<Option<(Source, PostsData, bool)>>,
    pub scroll: RwSignal<f64>,
}
```

provided next to `Session`:

```rust
provide_context(NewsCache::default());
```

Key detail: cache the **request key alongside the payload** (chaos stores
`Source`). The page compares the cached key with the wanted one to decide
whether the cache can answer. For yomu that key is the manga/title id for a
chapter list — without it, opening title B would show title A's chapters.

### 2. Fetch only when the cache cannot answer

Replace the resource with an effect. The trap worth knowing: the obvious
condition re-fetches on every visit anyway.

```rust
let was_online = StoredValue::new(false);
Effect::new(move |prev: Option<()>| {
    let wanted = source.get();
    let online = conn.get() == Connectivity::Online;
    // `prev.is_some()`: was_online starts false on every mount, so without
    // this guard every visit looks like a reconnection and re-fetches.
    let came_online = online && !was_online.get_value() && prev.is_some();
    was_online.set_value(online);
    let stale = !matches!(cache.loaded.get_untracked(), Some((cached, _, _)) if cached == wanted);
    if stale || came_online {
        spawn_local(async move { reload().await });
    }
});
```

`if online { fetch }` is wrong — it is true on every normal mount, which
defeats the entire change. It must be an Offline→Online *transition*.

Also: hold errors in a separate signal and leave the cached payload in place on
a failed refresh. A refresh that fails should not empty a list being read.

### 3. Pull-to-refresh

chaos, `crates/chaos-ui/src/hooks.rs` — `use_pull_to_refresh(on_refresh)`,
generic over the refresh future, returns `{ distance, refreshing }` signals for
the indicator. Copy it nearly verbatim; it is not news-specific.

Points that matter:

- Use leptos' `window_event_listener(leptos::ev::touchstart, …)` and
  `on_cleanup(|| handle.remove())`. Do **not** hand-roll `Closure` + manual
  `add_event_listener`: `Closure` is neither `Send` nor `Sync`, so it cannot go
  in a `StoredValue`, and you end up reimplementing what leptos already does.
- Listen on `window`, not a container, so it works whatever actually scrolls.
- Only arm when `scroll_y() <= 0.5`, or swiping down mid-list refreshes.
- Damping (~0.45) and a threshold (~72px) make it feel weighted rather than
  glued to the thumb.
- Touch events only, so a desktop browser is untouched — there, a page reload
  is the refresh, which is the intended behaviour.

yomu already enables the right web-sys features (`Touch`, `TouchEvent`,
`TouchList` in `crates/yomu-ui/Cargo.toml`); chaos had to add them.
`crates/yomu-ui/src/pager.rs` already does touch handling and is worth matching
for local convention.

CSS lives in `crates/chaos-web/styles.css` under `.pull-refresh` — height is
driven inline from the gesture, `.armed` signals "let go now", `.spinning`
animates, and there is a `prefers-reduced-motion` fallback.

## Scroll restoration

Save on scroll, restore once on mount inside a 0ms `set_timeout` so the rows
have committed to the DOM first:

```rust
let scroll_listener = window_event_listener(leptos::ev::scroll, move |_| { … });
on_cleanup(move || scroll_listener.remove());
```

## Not done, decide for yomu

- **In-memory only.** A cold start still fetches. Persisting through the
  offline cache would make cold starts instant too; for yomu's chapter lists
  that is probably more valuable than it was for news, since chapter lists
  change rarely.
- **One list only.** chaos wired just the News tab so the behaviour could be
  judged in one place first. yomu has two lists (titles, chapters) that likely
  want the same treatment — consider a small generic cache keyed by page +
  request key rather than two bespoke structs.

## Reference commits

In chaos, on `feat/app-oidc-auth` (pending, blocked on GPG signing at time of
writing): the keep-alive change touches `hooks.rs`, `pages/news.rs`, `lib.rs`,
`chaos-web/styles.css`, and adds three web-sys touch features to
`crates/chaos-ui/Cargo.toml`.
