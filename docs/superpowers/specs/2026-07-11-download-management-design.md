# Download Management UI — Design

**Goal:** A dedicated Downloads page showing the live download queue (pending / downloading / failed) with per-page progress, retry/cancel/clear actions, live refresh, and a local-vs-server storage overview.

## Server

### Live progress
There is a single download worker, so at most one chapter downloads at a time. `AppState` gains:

```rust
pub download_progress: Arc<RwLock<Option<ActiveDownload>>>
// ActiveDownload { chapter_id: Uuid, page: u32, total: u32 }
```

`downloader::fetch_pages` knows `pages.len()` up front; it writes `Some(ActiveDownload { chapter_id, page: index + 1, total })` after each page, and the worker clears it (`None`) when the chapter finishes (success or failure). No per-page DB writes.

### DB (`db/downloads.rs`)
- `download_queue() -> Vec<(Chapter, String)>` — every chapter whose `download_state` is `pending`/`downloading`/`failed`, joined to its manga title, ordered `downloading` → `pending` → `failed` then by manga title.
- `downloaded_summary() -> (u32, u32)` — `(count, sum(page_count))` over `download_state = 'downloaded'`.
- `retry_failed(ids) -> u32` — `failed` → `pending`, returns rows changed.
- `dismiss(ids) -> u32` — `pending`|`failed` → `none`, returns rows changed.

### API (`api/downloads.rs`)
- `GET /api/v1/downloads` (`OptionalUser`) → `DownloadsResponse`. Attaches the in-memory `progress` to the `downloading` entry.
- `POST /api/v1/downloads/retry` (`CurrentUser`, `DownloadChaptersRequest`) → retry, then `download_notify.notify_one()`.
- `POST /api/v1/downloads/dismiss` (`CurrentUser`, `DownloadChaptersRequest`) → dismiss.

## Domain (`api.rs`)

```rust
pub struct DownloadProgress { pub page: u32, pub total: u32 }

pub struct DownloadQueueEntry {
    pub chapter_id: Uuid,
    pub manga_id: Uuid,
    pub manga_title: String,
    pub chapter_title: String,
    pub state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<DownloadProgress>,
}

pub struct DownloadsResponse {
    pub queue: Vec<DownloadQueueEntry>,
    pub server_downloaded_chapters: u32,
    pub server_downloaded_pages: u32,
}
```

Reuse `DownloadChaptersRequest { chapter_ids }` for retry/dismiss.

## Client (`yomu-client`)
- `downloads() -> DownloadsResponse`
- `retry_downloads(&[Uuid]) -> BulkChaptersResponse`
- `dismiss_downloads(&[Uuid]) -> BulkChaptersResponse`

## UI (`pages/downloads.rs`, route `/downloads`, nav entry)
- **Storage overview:** server (downloaded chapters + pages) and this device (chapters saved locally, from `offline::device_chapters()`).
- **Queue**, grouped by state:
  - *Downloading* — manga · chapter with a `page/total` progress bar.
  - *Pending* — listed; "Cancel pending" dismisses all.
  - *Failed* — chapter + error; per-row Retry/Dismiss plus "Retry all failed" / "Clear failed".
- **Live refresh:** a `set_interval` re-fetches every 2 s while mounted; cleared on `on_cleanup`.
- **Empty state** when the queue is empty.

## Testing
- DB: `download_queue` returns only queued states with correct titles and ordering; `downloaded_summary` counts pages; `retry_failed`/`dismiss` transition only the intended states and report accurate counts.
- Worker progress reporting and UI polling are runtime/browser-verified (not unit-testable here), flagged like the other wasm UI.

## Notes / scope
- Progress is best-effort and transient (in-memory): a server restart mid-download loses the bar but the chapter re-queues normally. Acceptable.
- `GET /downloads` reveals library titles; it uses `OptionalUser` to match the existing read routes (never rejects), while the mutations require a session.
- Stacked on the PR #57 branch since it depends on the split `db/` modules and current API; its PR retargets to main once #57 merges.
</content>
