# yomu — session handoff (2026-07-05)

Fresh-session primer. Everything committed on `main` (initial branch:
`master`/`main` as git defaulted), working tree clean. Sibling of
[chaos](../../chaos) — its HANDOFF documents the shared conventions
(crate boundaries, worker pattern, DB conventions, nix gotchas); they all
apply here unchanged.

## What yomu is

Self-hosted manga/webtoon tracker + downloader + reader (Suwayomi-inspired,
no extension system). Leptos CSR + Axum + sqlx/SQLite. Scan sites are
declarative TOML selector definitions (`sources.d/*.toml`, `selector[@attr]`
syntax) — adding a site needs no code (ADR-0001). Progress is an
append-only journal of UUIDv7 events, merged by (at, id) — the same rule in
Rust (`yomu_domain::merge_position`), SQL, and the client (ADR-0002).

## State: what works today (verified against a fixture scan site)

- **Library**: search a source, track (± auto-download), covers
  proxied+cached, continue/start states.
- **Downloads (server)**: queue worker fetches chapters to
  `data_dir/<manga>/<chapter>/`, atomic publish, crash recovery,
  per-source politeness delay. Live reading proxies pages without storing.
  One endpoint serves both (`/chapters/{id}/pages/{n}`).
- **Updates**: periodic re-check (default 6h) + manual refresh; new chapters
  of auto-download manga are queued automatically.
- **Reader**: fullscreen immersive (tap center toggles chrome), paged AND
  vertical/webtoon modes (persisted per manga), arrow keys, prev/next
  chapter, position reported per page turn.
- **Offline (web/PWA)**: service worker (`crates/yomu-web/sw.js`) caches
  shell + page images (cache-first) + API GETs (network-first/fallback);
  manifest makes it phone-installable. "Save to device" prefetches a
  chapter. Offline page turns append journal events (client UUIDv7 via
  Web Crypto, `offline.rs` outbox in localStorage) flushed idempotently on
  reconnect. Verified with the server process killed: library + reader
  render from cache, the offline event synced back and won the merge.
- Importer status: none (yomu is fresh; no Suwayomi import planned yet).

## Layout notes

- `yomu-source`: `Source` trait + `SelectorSource` (pure parse fns tested on
  `tests/fixtures/*.html`) + `Registry` (fails loudly on broken TOML).
- `yomu-ui/src/offline.rs`: outbox, device marks, reader prefs, uuid_v7_js.
- E2E pattern: static fixture site (HTML like the test fixtures + PNGs)
  served by `python -m http.server 8765`, a `sources.d/fixture.toml`
  pointing at it; then curl through the whole flow. Recreate from the test
  fixtures if needed.
- Ports: server 4700, trunk dev 8081.

## Next steps

1. **Reader polish**: page prefetch (next 2-3), remember scroll offset in
   vertical mode, preserve initial-page scroll target in vertical, reading
   direction (RTL for manga), maybe pinch zoom on phone.
2. **Library QoL**: unread badges, sort by last update/read, mark
   read/unread, delete server download, storage usage.
3. **Sources**: native JSON-API source (first non-selector `Source` impl),
   per-source health in UI, hot-reload of sources.d, real-site selector
   definitions for the user's scan sites (expect tuning: Cloudflare,
   lazy-load attrs).
4. **Offline hardening**: quota awareness (Cache API eviction), download
   whole-manga button, show device-cached state in the reader, offline
   indicator in the topbar.
5. **Deployment (phase 4)**: verify `nix build .#yomu-server/.#yomu-web`,
   eval-test `services.yomu` (module written, untested), deploy on zeus,
   add a chaos dashboard tile.
6. Later: desktop/mobile shell (Tauri), `yomu-store` for a fully native
   offline app (the web PWA already covers the phone use case meanwhile).
