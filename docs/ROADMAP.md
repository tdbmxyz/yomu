# Roadmap

This is a forward-looking roadmap, not a historical checklist. Shipped work is
summarized first so completed features do not repeatedly appear as unfinished.
Detailed implementation history lives in `docs/superpowers/` and the changelog.

## Shipped in 2.x

- Selector sources with search and browse pagination, source fixture tests, and
  resilient startup that reports and skips malformed definitions.
- Unified source and local-file publications, CBZ/directories, rename healing,
  unsupported-format reporting, and periodic/on-demand rescans.
- Paged and continuous vertical readers, fit/direction controls, prefetch,
  progress journal synchronization, manual read/unread marks, unread badges,
  and update ordering.
- Server downloads (single/bulk/remove/retry), browser and shell device saves,
  persistent pull queues, download progress, offline boot, and offline outboxes.
- OIDC/browser/native-app sign-in, per-user progress/read state, proxy identity,
  account sign-out, backup/restore, updates and notifications.
- Tauri desktop and Android shells, Android background notifications and
  immersive reading, reproducible Nix server/web/desktop packages, a hardened
  NixOS module, and CI package builds.

## Current priorities

### Reliability and security

- Isolate and purge browser Service Worker data on logout/account changes.
- Resolve and pin public DNS targets for selector fetches to close DNS-rebinding
  SSRF, including redirects.
- Make browser device storage quota-aware with byte reporting and eviction.
- Add source health diagnostics and atomic `sources.d` hot reload.

### Source compatibility

- Add paginated search and multi-page publication chapter listings with loop,
  duplicate, and page-limit guards.
- Add the first native JSON/API source to validate authentication, API
  pagination, unavailable/premium content, and source-specific metadata.

### Library and storage

- Add create/rename/reorder/delete controls for user-defined categories.
- Add publication/global server byte reporting, bulk download/remove, orphan
  detection, and optional free-space/retention policy.
- Evolve native-shell durable state into `yomu-store` SQLite while keeping the
  browser PWA's Web Storage/Service Worker adapter separate.

## Engineering and operations

- Keep the real-browser Playwright suite and AI-assisted pi workflow current as
  user journeys grow.
- Continue database backup drills, integrity checks, WAL/session maintenance,
  readiness thresholds, and useful service metrics.
- Keep advisories, licenses, dependency automation, Rust pin, NixOS module
  evaluation, desktop linting, and Android Kotlin compilation green in CI.
