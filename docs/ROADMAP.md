# Roadmap

This is a forward-looking roadmap, not a historical checklist. Shipped work is
summarized first so completed features do not repeatedly appear as unfinished.
Detailed implementation history lives in Git. Documentation and release hygiene
are described in [MAINTENANCE.md](MAINTENANCE.md).

## Implemented

- Selector sources with search and paginated browse, source fixture tests, and
  resilient startup that reports and skips malformed definitions.
- Unified source and local-file publications, CBZ/directories, rename healing,
  unsupported-format reporting, and periodic/on-demand rescans.
- Paged and continuous vertical readers, fit/direction controls, prefetch,
  progress journal synchronization, manual read/unread marks, unread badges,
  and update ordering.
- Server downloads (single/bulk/remove/retry), browser and shell device saves,
  persistent pull queues, download progress, offline boot/outboxes, and native
  SQLite-backed snapshots of allowlisted UI state. Typed journal, cursor, and
  download APIs exist but are not yet the shell's synchronization adapter.
- OIDC/browser/native-app sign-in, per-user progress/read state, proxy identity,
  account sign-out, backup/restore, updates and notifications.
- Tauri desktop and Android shells, Android background notifications and
  immersive reading, reproducible Nix server/web/desktop packages, a hardened
  NixOS module, and CI package builds.

- Account-scoped offline state with retained legacy recovery; acknowledged browser
  saves, quota/byte reporting and disposable-cache eviction; bounded sync retries.
- Connection-pinned public DNS policy for selector fetches and redirects, bounded
  HTML bodies, and explicit private-host exceptions for operator-owned sources.
- Historical database recovery drills, browser failure-path tests and frontend
  delivery-size budgets in CI.

## Current priorities

### Reliability and security

- Validate the offline-ownership/network-hardening upgrade in staging before
  deployment; follow the compatibility notes in OPERATIONS.md.
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

## Engineering and operations

- Keep the real-browser Playwright suite and AI-assisted pi workflow current as
  user journeys grow.
- Continue database backup drills, integrity checks, WAL/session maintenance,
  readiness thresholds, and useful service metrics.
- Keep advisories, licenses, dependency automation, Rust pin, NixOS module
  evaluation, desktop linting, and Android Kotlin compilation green in CI.
