# Chapter download UX: state outlines + selection menu — design

## Problem

Each chapter row carries two icon buttons (device download, server
download). They crowd the row, expose no removal path, and split what is
really one question — "where does this chapter live?" — across two
controls. Bulk actions live in an ad-hoc bottom bar.

## Row outlines (replace the two buttons)

Every chapter row gets a 2px outline encoding its storage state:

| Connectivity | Server copy | Local copy | Outline |
|---|---|---|---|
| online | no | no | gray (`--border`) |
| online | yes | no | blue (new `--server` variable) |
| online | yes | yes | split: left half blue, right half green (border-image gradient) |
| online | no | yes | split with the blue half gray (local without server) |
| offline | no | no | gray + the existing dimmed "unavailable" row styling |
| offline | — | yes | green (`--saved`) |

Transient server states stay visible without buttons: `pending` /
`downloading` → pulsing blue outline (CSS animation); `failed` → red
(`--down`) outline, error text in the row's `title` tooltip. "Local"
means shell device storage in the apps and the service-worker prefetch
on the web (same behavior as today's device button, new representation).

## 3-dot menu and selection mode

- The chapter-list header gains a `⋮` button. Outside selection mode its
  menu holds a single entry, **Select**, which enters selection mode
  (long-press on a row still enters it directly; existing range-select
  and tap-toggle behavior is unchanged).
- In selection mode the bottom bar shrinks to selection management only:
  count, **All**, **✕**. All actions move into the `⋮` menu.
- Menu entries are computed from the union of the selected chapters'
  states; each action applies only to the chapters where it has an
  effect (mixed selections show the largest applicable set of actions,
  no-op chapters are skipped):
  - **Download (server)** — any selected without a server copy; online.
  - **Download (both)** — same condition; shells and SW-active web.
    Queues the server downloads, then watches the page's existing
    periodic refresh: each chapter that flips to `downloaded` is pulled
    to local storage. Leaving the page abandons not-yet-pulled locals
    (stated limitation).
  - **Download (local)** — any selected with a server copy but no local
    one; shells and SW-active web.
  - **Remove (server)** — any selected with a server copy; online. New
    endpoint below.
  - **Remove (local)** — any selected with a local copy; shells only
    (web Cache-API eviction is unreliable; hidden there). Uses the
    existing `device_delete_chapter` shell command (first UI for it)
    and clears the device mark.
  - **Mark read** / **Mark unread** — the selection, as today.
  - **Mark all before as read** — every chapter older than the oldest
    selected.
  - **Mark all after as unread** — every chapter newer than the newest
    selected.

## Offline bulk marking (marks outbox)

Read marks work offline and synchronize on reconnect:

- `offline::marks_outbox`: a localStorage map `chapter_id → bool`
  (desired read state, last write wins locally). All mark actions write
  through it when the server call fails (same failure-driven pattern as
  the progress outbox).
- Rendering overlays pending marks onto `chapter.read` from the cached
  detail, so the list reflects marks immediately while offline; unread
  counts self-correct after sync.
- Flush on the `online` event and at startup (alongside
  `flush_outbox`): one `/chapters/mark` call per direction (read ids,
  unread ids); entries are removed on success, kept on failure. The
  endpoint is a set operation, so replays are idempotent. Marks have no
  server-side journal — a mark made on another device in the meantime
  is overwritten by the flush (documented, acceptable for a
  single-reader setup).

## Server work

`POST /api/v1/chapters/remove-downloads` (bulk, mirrors
`/chapters/download`): body `{chapter_ids}`, response
`BulkChaptersResponse`. For each chapter currently `downloaded`: reset
`download_state` to `none` (clear `downloaded_at`, `download_error`,
keep `page_count` — it is still true knowledge), delete the page
directory, invalidate the live-pages cache entry. Chapters in other
states are skipped and don't count in `affected`.

## Client capability detection

- online/offline: the page's existing `offline` flag (cache-served
  detail).
- local tier available: `offline::shell_available() ||
  offline::service_worker_active()`; removal additionally requires
  `shell_available()`.

## Testing

- Server: db test for the state reset; endpoint behavior via db-level
  tests (no HTTP harness); live E2E for the full loop
  (download → remove → row state).
- UI: pure helper computing the menu-entry set from a selection's
  states (unit-tested table); marks-outbox overlay + flush logic
  unit-tested where wasm-free.
- Headless E2E: outline colors for each reachable state, menu contents
  for single and mixed selections, offline marking round-trip (mark
  offline → reload → still marked → reconnect → server agrees).

## Rollout

Server + all clients (APK-relevant). No config.
