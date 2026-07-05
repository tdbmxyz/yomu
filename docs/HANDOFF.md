# yomu — session handoff (2026-07-05, session 2)

Fresh-session primer. Sibling of [chaos](../../chaos) — its HANDOFF
documents the shared conventions (crate boundaries, worker pattern, DB
conventions, nix gotchas); they all apply here unchanged.

## What yomu is

Self-hosted manga/webtoon tracker + downloader + reader (Suwayomi-inspired,
no extension system). Leptos CSR + Axum + sqlx/SQLite. Scan sites are
declarative TOML selector definitions (`sources.d/*.toml`, `selector[@attr]`
syntax) — adding a site needs no code (ADR-0001). Progress is an
append-only journal of UUIDv7 events, merged by (at, id) — the same rule in
Rust (`yomu_domain::merge_position`), SQL, and the client (ADR-0002, cursor
amended: sync paging is by server `seq`, not event id).

## State: what works today (verified end-to-end against a fixture site)

- **Library**: search a source, track (± auto-download), covers
  proxied+cached, continue/start states.
- **Local source** (new): `local/<Series>/<Chapter>/{*.png|*.cbz}` on the
  server is a built-in searchable source (`[local]` config; cover.jpg /
  details.json optional; keys are dir-relative paths, `local:` URL scheme,
  traversal-checked). Empty search lists every local series.
- **Downloads (server)**: queue worker fetches chapters to
  `data_dir/<manga>/<chapter>/`, atomic publish, crash recovery, `.partial`
  removed on failure, orphan files discarded when the manga was deleted
  mid-download, per-source politeness delay (kept even after transport
  errors). Live reading proxies pages without storing; the page-URL cache
  has a 30-min TTL + bound and re-resolves once on image failure (expiring
  CDN URLs). One endpoint serves both (`/chapters/{id}/pages/{n}`);
  downloaded-but-missing directories fall back to the live path.
- **Updates**: periodic re-check (default 6h, min 60s) + manual refresh; new
  chapters of auto-download manga are queued automatically. Duplicate
  chapter keys in one scraped listing are deduped.
- **Categories** (new): every manga has one (seeded Reading/Paused/Finished,
  default reading; migration 0003). Per-category `update_enabled` gates the
  periodic sweep (manual refresh unaffected). Library page: filter tabs +
  per-category "new-chapter checks" toggle; manga page: category select.
  `GET/PUT /categories`, `UpdateMangaRequest.category` (None = keep).
- **Auth** (new, ADR-0003): `[auth]` + OIDC issuer (authentik) → sign-in
  via code+PKCE, userinfo claims, chaos-style sessions (yomu_session
  cookie / bearer); no `[auth]` → single-account mode, every request is
  the seeded shared "everyone" user (nil UUID), no login UI. Progress is
  per-user (migration 0004); library/downloads/categories stay shared.
  Extractors: `CurrentUser` (progress endpoints, 401 signed-out in OIDC
  mode), `OptionalUser` (library positions). Topbar shows sign-in/out only
  in OIDC mode. SW lets /api navigations through (login redirects);
  outbox keeps events on 401/403. E2E'd against a stub IdP (python).
- **Reader**: fullscreen immersive, paged AND vertical modes (persisted per
  manga). Vertical mode scrolls to the target page on entry and only
  journals *user* scrolling (programmatic positioning and placeholder
  heights can't rewind progress). Opening position is journaled only after
  the chapter is confirmed to exist.
- **Progress sync**: `POST /progress/events` is transactional and *skips*
  events for deleted manga (`{accepted, skipped}` response) — a stale event
  can't wedge the outbox. Client outbox removes only flushed ids (no
  read-flush-clobber race) and drops batches the server rejects with 4xx.
  `GET /progress/events?since=<seq>` pages by arrival order (`next_since`).
- **Offline (web/PWA)**: sw.js precaches the shell *and its hashed assets*
  at install (first-session offline works) and re-syncs that asset set on
  every online navigation (single SHELL cache entry, stale assets pruned,
  assets cached before the shell so a mid-refresh disconnect can't strand a
  shell without its assets). SW registered from index.html, before the wasm
  loads. "Save to device" refuses (instead of lying) when no SW controls
  the page. API base is resolvable via `window.YOMU_API_BASE` /
  `localStorage["yomu-api-base"]` / same-origin — the seam a Tauri shell
  needs (`tauri.localhost` is explicitly not trusted as API origin).
- Importer status: none.

## Layout notes

- `yomu-source`: `Source` trait + `SelectorSource` (pure parse fns tested on
  `tests/fixtures/*.html`) + `LocalSource` (`local.rs`, tested on temp dirs)
  + `Registry` (fails loudly on broken TOML, duplicate ids, non-slug ids).
- `yomu-ui/src/offline.rs`: outbox, device marks, reader prefs, uuid_v7_js
  (Math.random fallback), `service_worker_active()`.
- Migrations: `0002_progress_seq.sql` rebuilt progress_events with a `seq`
  arrival cursor. sqlx runs it automatically.
- E2E pattern: static fixture site (HTML like the test fixtures + PNGs)
  served by `python -m http.server 8766`, a `sources.d/fixture.toml`
  pointing at it; then curl through the whole flow. Recreate from the test
  fixtures if needed.
- Ports: server 4700, trunk dev 8081.

## Next steps

1. **Reader polish**: page prefetch (next 2-3), reading direction (RTL),
   pinch zoom on phone. (Vertical initial-scroll + per-page journal fixed.)
2. **Library QoL**: unread badges, sort by last update/read, mark
   read/unread, delete server download, storage usage; local-source UI
   affordances (hide "download"/auto-download for local manga, covers in
   local search results); category QoL (user-defined categories, rename,
   reorder — schema is ready, only CRUD endpoints/UI missing).
3. **Sources**: native JSON-API source, per-source health in UI, hot-reload
   of sources.d, real-site selector definitions (expect tuning: Cloudflare,
   lazy-load attrs). `chapter_order = "oldest_first"` exists for sites that
   list oldest-first.
4. **Offline hardening**: quota awareness (Cache API eviction), download
   whole-manga button, offline indicator in the topbar, surface the
   "save to device" no-SW error in the UI (it's only a console warn).
5. **Deployment (phase 4)**: `nix build .#yomu-server/.#yomu-web` verified;
   eval-test `services.yomu` (module written, untested), deploy on zeus,
   add a chaos dashboard tile. Server handles SIGTERM now (systemd stop is
   graceful).
6. Later: desktop/mobile shell (Tauri) — inject `YOMU_API_BASE` and note
   the prev/next-chapter links use full document loads (`rel=external`),
   which need an SPA fallback the Tauri asset protocol doesn't provide;
   `yomu-store` for a fully native offline app.
