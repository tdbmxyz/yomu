# Download Queue Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Spec: docs/superpowers/specs/2026-07-19-download-queue-fixes-design.md

**Goal:** Make "download both" reliably save to device (persistent app-level pull queue + background driver, shown in the Downloads tab), download bulk selections oldest-first, and fix the Android tofu tab icon.

**Architecture:** A persisted app-scoped `PullQueue` signal drives a background poller in `App` that pulls each chapter to the device once its server download finishes. Bulk actions reverse their id lists to ascending order; the server worker tiebreaks by chapter number.

**Tech Stack:** Leptos (wasm), axum/sqlx (server).

---

### Task 1: Server download order

**Files:** Modify `crates/yomu-server/src/db/downloads.rs`; test in `crates/yomu-server/src/db/mod.rs`.

- [ ] **Step 1: order by number within fetched_at.** Replace the query in `next_pending_download`:

```rust
    pub async fn next_pending_download(&self) -> Result<Option<Chapter>> {
        let row = sqlx::query_as::<_, ChapterRow>(
            "SELECT * FROM chapters WHERE download_state = 'pending'
             ORDER BY fetched_at, number IS NULL, number LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(Chapter::try_from).transpose()
    }
```

- [ ] **Step 2: test.** In `crates/yomu-server/src/db/mod.rs` tests, add:

```rust
    #[tokio::test]
    async fn next_pending_download_is_lowest_number_first() {
        let db = Db::in_memory().await.unwrap();
        let manga = db
            .insert_manga(
                "fixture",
                &details("m1", &[("c3", Some(3.0)), ("c1", Some(1.0)), ("c2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        let chapters = db.list_chapters(manga.id).await.unwrap();
        let ids: Vec<_> = chapters.iter().map(|c| c.id).collect();
        db.mark_pending(&ids).await.unwrap();
        let next = db.next_pending_download().await.unwrap().unwrap();
        assert_eq!(next.number, Some(1.0));
    }
```

- [ ] **Step 3:** `cargo test -p yomu-server next_pending_download_is_lowest` → PASS; commit `fix(server): download lowest-numbered pending chapter first`.

### Task 2: Persistent PullQueue store

**Files:** Modify `crates/yomu-ui/src/lib.rs`, `crates/yomu-ui/src/offline.rs`.

- [ ] **Step 1: type + helpers (lib.rs).** After the `DeviceMarks` type/helpers add:

```rust
/// One chapter queued to pull to this device once its server download
/// finishes ("download both"). Ordered oldest-first; persisted so it
/// survives navigation and app restarts.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PullItem {
    pub chapter_id: uuid::Uuid,
    pub manga_id: uuid::Uuid,
    pub manga_title: String,
    pub chapter_title: String,
}

pub type PullQueue = RwSignal<Vec<PullItem>>;

pub fn use_pull_queue() -> PullQueue {
    use_context().expect("PullQueue provided by App")
}
```

- [ ] **Step 2: persistence helpers (offline.rs).** Add near the device-mark helpers:

```rust
const PULL_QUEUE_KEY: &str = "yomu-pull-queue";

pub fn load_pull_queue() -> Vec<crate::PullItem> {
    read_json(PULL_QUEUE_KEY)
}

pub fn save_pull_queue(items: &[crate::PullItem]) {
    write_json(PULL_QUEUE_KEY, &items.to_vec());
}
```

(`read_json`/`write_json` already exist in offline.rs; `read_json` needs `Vec<PullItem>: Default` — Vec is Default.)

- [ ] **Step 3: provide in App (lib.rs).** After `provide_context(device_marks);`:

```rust
    let pull_queue: PullQueue = RwSignal::new(offline::load_pull_queue());
    provide_context(pull_queue);
    // Write-through: persist any change so the queue survives restarts.
    Effect::new(move |_| {
        pull_queue.with(|q| offline::save_pull_queue(q));
    });
```

- [ ] **Step 4:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles (unused-warns ok). Commit `feat(ui): persistent app-level pull queue store`.

### Task 3: Background pull driver

**Files:** Modify `crates/yomu-ui/src/lib.rs`; a new `crate::pull` module.

- [ ] **Step 1: driver module (crates/yomu-ui/src/pull.rs, new).**

```rust
//! Background driver for the device-pull queue ("download both"): once a
//! queued chapter's server download finishes, save it to this device, in
//! queue (oldest-first) order. Runs app-wide so it survives leaving the
//! manga page; the queue itself is persisted (see offline::save_pull_queue).

use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;
use uuid::Uuid;
use yomu_client::YomuClient;
use yomu_domain::DownloadState;

use crate::{Connectivity, DeviceMarks, LocalDownloads, PullQueue};

/// Start the 3s poller; call once from `App`.
pub fn start(
    conn: RwSignal<Connectivity>,
    client: YomuClient,
    queue: PullQueue,
    local: LocalDownloads,
    marks: DeviceMarks,
) {
    let running = StoredValue::new(false);
    let tick = move || {
        if running.get_value()
            || conn.get_untracked() != Connectivity::Online
            || queue.with_untracked(|q| q.is_empty())
        {
            return;
        }
        running.set_value(true);
        let client = client.clone();
        spawn_local(async move {
            drive(&client, queue, local, marks).await;
            running.set_value(false);
        });
    };
    let closure = leptos::wasm_bindgen::closure::Closure::<dyn Fn()>::new(tick);
    if let Some(window) = web_sys::window() {
        use leptos::wasm_bindgen::JsCast;
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            3000,
        );
    }
    closure.forget();
}

async fn drive(client: &YomuClient, queue: PullQueue, local: LocalDownloads, marks: DeviceMarks) {
    let Ok(downloads) = client.downloads().await else {
        return; // transient; next tick retries, queue untouched
    };
    let mut busy: HashSet<Uuid> = HashSet::new();
    let mut failed: HashSet<Uuid> = HashSet::new();
    for e in &downloads.queue {
        match e.state {
            DownloadState::Pending | DownloadState::Downloading => {
                busy.insert(e.chapter_id);
            }
            DownloadState::Failed { .. } => {
                failed.insert(e.chapter_id);
            }
            _ => {}
        }
    }
    // Walk oldest-first; pull the leading ready run, stop at the first
    // still-downloading item so order is preserved.
    loop {
        let Some(item) = queue.with_untracked(|q| q.first().cloned()) else {
            break;
        };
        let id = item.chapter_id;
        if marks.with_untracked(|m| m.contains_key(&id)) || failed.contains(&id) {
            remove(queue, id); // already on device, or server gave up
            if failed.contains(&id) {
                leptos::logging::warn!("pull queue: server download failed for {id}");
            }
            continue;
        }
        if busy.contains(&id) {
            break; // not ready yet — keep it and the rest queued
        }
        if local.with_untracked(|m| m.contains_key(&id)) {
            break; // its pull is already in flight
        }
        remove(queue, id);
        let _ = crate::pages::save_locally(
            client,
            item.manga_id,
            item.manga_title.clone(),
            id,
            item.chapter_title.clone(),
            local,
            marks,
        )
        .await;
    }
}

fn remove(queue: PullQueue, id: Uuid) {
    queue.update(|q| q.retain(|it| it.chapter_id != id));
}
```

- [ ] **Step 2: expose `save_locally`.** In `crates/yomu-ui/src/pages/manga.rs` change `async fn save_locally` to `pub(crate) async fn save_locally`. In `crates/yomu-ui/src/pages/mod.rs`, ensure `pub use` or the module path `crate::pages::save_locally` resolves — if manga is a private submodule, add `pub(crate) use manga::save_locally;` to pages/mod.rs (check how other cross-module items are exposed; if `pages` re-exports components via `pub use`, add there).

- [ ] **Step 3: register module + start driver (lib.rs).** Add `mod pull;` near the other `mod` lines. In `App`, after the notify block:

```rust
    pull::start(
        conn,
        YomuClient::new(config.api_base.clone()),
        pull_queue,
        local_downloads,
        device_marks,
    );
```

- [ ] **Step 4:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles. Commit `feat(ui): background driver pulls queued chapters to device`.

### Task 4: Manga page — app queue + ascending order

**Files:** Modify `crates/yomu-ui/src/pages/manga.rs`.

- [ ] **Step 1: remove the page-local pull_queue + drain.** Delete the `let pull_queue = RwSignal::new(HashSet::<Uuid>::new());` line (~46) and the entire pull-queue drain `Effect` block (the `{ let client = client.clone(); Effect::new(...) }` around lines 180-228). Remove `pull_queue` from the `<MangaDetail .../>` invocation, the `MangaDetail` signature, the `<ChapterList .../>` invocation, and the `ChapterList` signature.

- [ ] **Step 2: ChapterList reads the app queue.** Where `local_downloads`/`device_marks` are bound in `ChapterList` (after `let manga_title = StoredValue::new(manga_title);`), add:

```rust
    let pull_queue = crate::use_pull_queue();
```

- [ ] **Step 3: ascending helper + DownloadBoth via app queue.** Replace the `Action::DownloadServer | Action::DownloadBoth` arm:

```rust
            Action::DownloadServer | Action::DownloadBoth => {
                // Bulk order: oldest chapter first (the list shows newest
                // first, so reverse the display-order selection).
                let mut dl = ids_where(|s| !s.on_server);
                dl.reverse();
                if action == Action::DownloadBoth {
                    let mut both: Vec<Uuid> = ids_where(|s| !s.on_device);
                    both.reverse();
                    let mtitle = manga_title.get_value();
                    let ctitles = titles.get_value();
                    let items: Vec<crate::PullItem> = both
                        .iter()
                        .map(|id| crate::PullItem {
                            chapter_id: *id,
                            manga_id,
                            manga_title: mtitle.clone(),
                            chapter_title: ctitles.get(id).cloned().unwrap_or_default(),
                        })
                        .collect();
                    pull_queue.update(|q| {
                        for it in items {
                            if !q.iter().any(|e| e.chapter_id == it.chapter_id) {
                                q.push(it);
                            }
                        }
                    });
                }
                let client = use_client();
                spawn_local(async move {
                    match client.download_chapters(&dl).await {
                        Ok(r) => {
                            status.set(Some(match r.affected {
                                0 => "Nothing new to download".into(),
                                n => format!("{n} chapter(s) queued — downloads run one by one"),
                            }));
                            refresh.update(|n| *n += 1);
                        }
                        Err(err) => status.set(Some(format!("Download failed: {err}"))),
                    }
                });
            }
```

Note: `DownloadBoth` queues every selected not-on-device chapter (both the not-on-server ones — pulled after their server download — and the already-on-server ones — pulled on the driver's next tick, since they are absent from `/downloads`). No immediate-pull special case is needed; the driver handles both.

- [ ] **Step 4: DownloadLocal ascending.** In that arm, after `let pull = ids_where(|s| s.on_server && !s.on_device);` add `let mut pull = pull; pull.reverse();` (or bind `let mut pull = ...` and `pull.reverse();`).

- [ ] **Step 5:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles. Commit `fix(ui): bulk downloads oldest-first; download-both uses persistent queue`.

### Task 5: Downloads tab waiting group

**Files:** Modify `crates/yomu-ui/src/pages/downloads.rs`.

- [ ] **Step 1: read the queue.** In `Downloads`, add `let pull = crate::use_pull_queue();` and pass it to `DownloadsView` (new prop `pull: crate::PullQueue`), threaded from the call site like `local`.

- [ ] **Step 2: render the waiting group** at the top of the "On this device" section (before the in-flight `move ||` block):

```rust
        {move || {
            let items = pull.get();
            (!items.is_empty()).then(|| {
                view! {
                    <p class="muted downloads-waiting-head">"Waiting for server download"</p>
                    <ul class="download-list">
                        {items
                            .into_iter()
                            .map(|it| view! { <WaitingRow it pull/> })
                            .collect_view()}
                    </ul>
                }
            })
        }}
```

- [ ] **Step 3: WaitingRow component** (end of file):

```rust
/// A chapter queued to pull to this device once its server download
/// finishes; Cancel drops it from the queue.
#[component]
fn WaitingRow(it: crate::PullItem, pull: crate::PullQueue) -> impl IntoView {
    let id = it.chapter_id;
    let cancel = move |_| pull.update(|q| q.retain(|e| e.chapter_id != id));
    view! {
        <li class="download-row">
            <div class="download-row-head">
                <a class="download-title" href=format!("/manga/{}", it.manga_id)>
                    <strong>{it.manga_title}</strong>
                    " · " {it.chapter_title}
                </a>
                <button class="button" on:click=cancel>"Cancel"</button>
            </div>
            <span class="muted">"waiting for server download…"</span>
        </li>
    }
}
```

- [ ] **Step 4:** the device section's empty-state condition should account for the queue: show the resting device-count line only when both the queue and the in-flight map are empty. Wrap the existing in-flight `items.is_empty()` resting-line so it renders the resting line only when `pull.get().is_empty()` too (change the `if items.is_empty()` to `if items.is_empty() && pull.get().is_empty()`).

- [ ] **Step 5:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles. Commit `feat(ui): waiting-for-server group in the Downloads tab`.

### Task 6: Tab icon

**Files:** Modify `crates/yomu-ui/src/lib.rs`.

- [ ] **Step 1:** replace the Downloads tab icon glyph:

```rust
                    <A href="/downloads"><span class="tab-icon">"↓"</span>"Downloads"</A>
```

- [ ] **Step 2:** `cargo check -p yomu-ui --target wasm32-unknown-unknown`. Commit `fix(ui): downloads tab icon uses a core-font glyph`.

### Task 7: Verify + E2E

**Files:** Create `<scratchpad>/vscroll/pull-queue-e2e.js`.

- [ ] **Step 1:** `just check`; `cargo test --workspace --exclude yomu-shell`. Both pass.

- [ ] **Step 2: shell-sim E2E** (model on `vscroll/unified-dl-e2e.js`): fake `__TAURI__.core.invoke`, static dist on 4791 real server. Intercept `/api/v1/downloads` with a mutable stub so a chapter can be made to "finish" (move from a downloading entry to absent). Drive:
  - Long-press a not-on-server chapter → "Download (both)"; go to Downloads; assert a "Waiting for server download" row exists.
  - Flip the `/downloads` stub so the chapter is absent (finished); within a few driver ticks assert the device row appears (in-flight), then a device mark is written, and the waiting row clears.
  - Reload `/downloads` page mid-wait (before flipping): assert the waiting row is still present (persistence).
  - Icon: assert `document.querySelector('.tabbar a[href="/downloads"] .tab-icon').textContent === '↓'`.

Run: `cd <scratchpad> && bun vscroll/pull-queue-e2e.js` → all PASS.

- [ ] **Step 3:** commit any test-only tweaks; open the PR.
