# Roadmap

## Phase 0 — Foundations (this repo bootstrap)

- [x] Workspace, flake (packages + module skeleton), docs, ADRs
- [x] Domain model incl. progress journal + merge rule (tested)
- [x] `Source` trait + selector-driven source + fixture tests
- [x] Server: library/chapters/progress API, download queue, updater
- [x] Web UI: library grid, source search/add, manga page, paged reader
- [x] E2E against a local fixture scan site

## Phase 1 — Daily-driver reader

- [ ] Webtoon mode (continuous vertical scroll) + reader mode per manga
- [ ] Page prefetch (next 2–3 pages) in the reader
- [ ] Mark chapter read / unread manually; read-state display in the list
- [ ] Library: unread badges, sort by last update/last read
- [ ] Download management: delete downloaded chapter, download-all button,
      storage usage per manga

## Phase 2 — Sources

- [ ] Native JSON-API sites source (clean JSON API) as the first non-selector source
- [ ] Per-source health surface (last error, last success) in the UI
- [ ] Hot-reload of sources.d without restart
- [ ] Selector spec: pagination for search results, multi-page chapter lists

## Phase 3 — Offline client

- [ ] `yomu-store` crate: local SQLite (chapter files + journal) behind the
      same domain types
- [ ] Download-to-device manager (select manga/chapters, evict policy)
- [ ] Journal sync: push local events, pull server tail, badge for pending sync
- [ ] Desktop shell (Tauri, as in chaos); mobile investigation after

## Phase 4 — Deployment

- [ ] Verify `nix build .#yomu-server` / `.#yomu-web` (flake exists, untested
      in CI terms)
- [ ] NixOS module eval test + deploy on zeus next to chaos
- [ ] chaos dashboard tile for yomu (service monitor entry)
