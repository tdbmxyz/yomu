# Offline UX Design

Date: 2026-07-14

## Problem

With the server unreachable the app is unpleasant to use:

1. **Indefinite "Loading"** — the wasm client has no request timeout, so an
   unreachable host can hang the boot gate and every page resource for
   minutes (webview connect timeouts).
2. **Covers missing offline** — covers are plain `<img>` against the server.
   The web service worker caches them, but the native shells have no service
   worker and no cover storage.
3. **A retry at every step** — offline knowledge lives only in the boot
   gate's banner. Every page still fires its own fetch, waits for it to
   fail, and only then falls back to the localStorage cache.

Future direction this design must not block: a serverless mode where the
shells run an embedded local server. The client already talks to "a base
URL"; that future only re-points it at localhost.

## Decisions (user)

- Launch: keep a brief "Connecting…" gate but cap the probe (~3 s), then
  render the cached UI.
- Offline covers in shells: cache library covers automatically whenever the
  library loads online. Browse/search covers stay online-only.
- Recovery: manual retry via the offline badge. No background re-probe
  timers. Single exception: the browser's free `online` event triggers one
  health ping.

## Design

### 1. Connectivity state

`Connectivity { Checking, Online, Offline }` in an app-wide
`RwSignal<Connectivity>` provided from `App` (context; `use_connectivity()`
accessor). Transitions:

- boot health probe result (gate);
- any API fetch failure inside the cached-read helper ⇒ `Offline`;
- manual retry ⇒ `Checking` ⇒ probe result;
- browser `online` event while not `Online` ⇒ one probe.

### 2. Request timeouts (yomu-client)

All requests funnel through `check_status`; it applies a default **8 s**
timeout to any request that doesn't carry one (via `Request::timeout_mut`
after `build()`, which works on native and wasm — reqwest wasm implements
timeouts with AbortController). `health()` sets its own **3 s** timeout.
This alone bounds every "Loading" state.

### 3. Cache-first reads

New helper in `offline`:

```rust
pub async fn cached<T, E, Fut>(conn: RwSignal<Connectivity>, key: &str,
    fetch: impl FnOnce() -> Fut) -> Result<(T, bool), E>
where Fut: Future<Output = Result<T, E>>, T: Serialize + DeserializeOwned
```

- `Offline` and a cached copy exists ⇒ return it immediately, **no
  network**.
- Otherwise fetch: success ⇒ `cache_put`, flip to `Online` if the state
  disagreed, return `(value, false)`; failure ⇒ flip to `Offline`, return
  the cached copy `(value, true)` or the error if none.

The bool is the existing "stale" flag (`with_cache_flagged` semantics).
Pages migrate their `LocalResource`s to this helper and read `conn.get()`
in the tracked closure, so a successful retry refetches every open view.
The reader's `chapter_pages` resource short-circuits to the device-saved
metadata when `Offline` instead of trying the network first.

### 4. Offline badge = retry button

The banner moves out of the boot gate into an `OfflineBadge` component
rendered whenever `conn == Offline` (so mid-session failures show it too).
Tap ⇒ `Checking` (spinner in the badge) ⇒ capped health probe ⇒ on success
`Online`, flush progress/marks outboxes (resources refetch by tracking);
on failure back to `Offline` with a brief "still offline" flash.

The boot gate keeps: `Checking` (bounded by the 3 s probe), `Ready`
(children; conn drives the badge), `Unreachable` (connect form, unchanged,
for never-seen servers). The old `Offline` gate state becomes
`Ready` + `conn = Offline`. "Continue anyway" maps to exactly that.

### 5. Offline covers in the shells

- New Tauri command `device_save_cover { base, manga }`: downloads
  `api/v1/manga/<id>/cover` to `<app-data>/covers/<id>.<ext>` (same
  content-type/extension rules as chapter pages).
- The `yomudev` protocol gains `cover/<manga_id>` serving that file.
- `offline` gains `shell_cover_url`, `shell_save_cover`, and a
  `yomu-device-covers` localStorage set mirroring which covers are stored
  (same pattern as device chapter marks).
- A shared `Cover` component (new `crates/yomu-ui/src/cover.rs`) renders a
  manga cover: `Offline` + shell + stored ⇒ shell URL; otherwise the server
  URL; `onerror` ⇒ the existing `cover-empty` placeholder. Used by the
  library grid and the manga page. Browse/search keep their current img
  (out of scope).
- After a successful **online** library load in a shell, a background sweep
  saves covers missing from the device set, sequentially.
- Web: the service worker already caches `/api/v1/manga/<id>/cover`
  cache-first; unchanged.

### 6. Error handling

- Offline with nothing cached: the fetch still runs (it fails fast now) and
  pages render their existing error states.
- A cover that 404s on the shell protocol falls back to the placeholder via
  `onerror` — no special existence checks needed beyond the mark set.
- Outbox flushing stays as-is (already reconciled/idempotent server-side).

### 7. Testing

- Existing unit/integration suites must stay green.
- Headless E2E (Blink + Firefox against a scratch server):
  - kill the server, relaunch: cached library visible in < 4 s;
  - instrument `fetch` while navigating offline: zero API calls besides
    explicit retry probes;
  - retry with the server back: badge clears, views refresh;
  - desktop shell: library covers render offline after one online load.
