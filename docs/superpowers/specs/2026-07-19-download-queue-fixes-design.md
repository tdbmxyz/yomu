# Download Queue Fixes Design

Date: 2026-07-19

## Problem

Three issues surfaced after 1.13.0:

1. **"Download both" only saved to the server.** The device pull is
   queued in a signal that lives on the manga page and is drained by an
   effect that waits for the server download to finish. Leaving the
   manga page before that completes (normal — server downloads take a
   while) drops the queue, so the local save never runs. The queue is
   not surfaced anywhere, so nothing shows as "waiting."
2. **Tofu tab icon on Android.** The Downloads tab icon is `⭳`
   (U+2B73), in a symbols block Android's WebView font doesn't bundle,
   so it renders as a crossed box. Desktop Chrome happens to have the
   glyph.
3. **Bulk downloads run newest-first.** The chapter list shows newest
   first; a bulk selection is collected in that display order, so
   downloads queue newest→oldest. The server worker also breaks
   `fetched_at` ties undefined-ly within a manga.

## Decisions (user)

- The device-pull queue is **persisted across restarts** (localStorage),
  lifted to app scope, with a background driver.
- Bulk downloads (all variants) and the pull queue process **oldest
  chapter first**.

## Design

### 1. Persistent app-level pull queue

New app-scoped, ordered, persisted queue:

```rust
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PullItem {
    pub chapter_id: Uuid,
    pub manga_id: Uuid,
    pub manga_title: String,
    pub chapter_title: String,
}
pub type PullQueue = RwSignal<Vec<PullItem>>;
```

- Provided in `App`, seeded from localStorage key `yomu-pull-queue`.
- A `pull_queue_push(queue, items)` / `pull_queue_remove(queue, id)`
  pair in `offline` (or inline helpers) mutate the signal **and**
  write-through to localStorage; dedupe by chapter id on push, preserve
  insertion (ascending) order.
- The manga page's page-local `pull_queue` signal and its drain effect
  are removed.

### 2. Background driver

An app-level poller in `App`, active only while the queue is non-empty
and connectivity is Online:

- Interval ~3 s. Each tick, if the queue is empty or not Online, do
  nothing.
- Fetch `GET /downloads`. Build the set of chapter ids that are still
  Pending/Downloading and the set that are Failed.
- Walk the queue **in order** (oldest first). For each item:
  - already in `DeviceMarks` → remove from queue (nothing to do);
  - Pending/Downloading on the server → stop (keep it and the rest
    waiting, preserving order);
  - Failed on the server → remove from queue, set a page-independent
    status log;
  - otherwise (absent from the queue = server download finished, or it
    was already downloaded) → this is the next one to pull.
- Pull the ready leading items sequentially in one `spawn_local`:
  `save_locally` each (which moves it into `LocalDownloads` and, on
  success, writes the mark), removing each from the queue as its save
  starts. Sequential awaits keep ascending order and avoid overlapping
  device writes.

`save_locally` is reused unchanged (it already takes the `LocalDownloads`
and `DeviceMarks` stores).

### 3. Downloads tab: waiting group

In the "On this device" section, above the in-flight rows, render a
"Waiting for server download" group from the pull queue (in order):
each row shows manga · chapter and a Cancel button that removes it from
the queue (`pull_queue_remove`). When both the queue and the in-flight
map are empty, the section shows the resting device-count line as today.

### 4. Ascending bulk order (client)

`ChapterList` collects bulk ids in display order (newest first).
`ids_where` results are reversed to ascending (oldest first) before use
in the download actions:

- `DownloadServer` / `DownloadBoth`: the `download_chapters` request and
  the pull-queue push both use the ascending id list.
- `DownloadLocal`: the immediate local saves iterate ascending.

The list's display order and selection UI are unchanged; only the action
id lists are reversed.

### 5. Ascending order (server)

`Db::next_pending_download` orders by `fetched_at, number` so that within
a manga's batch (shared `fetched_at`) the lowest chapter number is
downloaded first, instead of an undefined tiebreak. NULL numbers sort
after real ones (`ORDER BY fetched_at, number IS NULL, number`).

### 6. Tab icon

Replace `⭳` with `↓` (U+2193, in the core font on every platform).

### 7. Error handling

- Driver: a `/downloads` fetch failure while online is ignored (next
  tick retries); the queue is untouched. A server-failed chapter is
  dropped with a logged warning so the queue can't wedge on it.
- A `save_locally` error keeps its existing behavior (red ring, status
  line); the item is already out of the pull queue (its pull started),
  so it won't loop.
- localStorage write failures degrade to in-memory only (same as the
  other stores).

### 8. Testing

- Server unit test: queue three pending chapters with numbers 3, 1, 2
  (same `fetched_at`); `next_pending_download` returns number 1.
- Shell-sim E2E:
  - "Download both" on a not-yet-downloaded chapter adds a
    "Waiting for server download" row; stub `/downloads` so the chapter
    leaves the queue; the driver pulls it (device row appears, mark
    written) and the waiting row clears.
  - Two queued chapters (numbers 1 and 2) with staged completion pull in
    ascending order.
  - Reload the page mid-wait: the waiting row is still present (queue
    persisted).
  - The tab icon element's text is `↓` (no tofu — assert the codepoint).
- `just check`, `cargo test --workspace --exclude yomu-shell`.
