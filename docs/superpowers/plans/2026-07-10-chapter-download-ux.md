# Chapter Download UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two per-row download buttons with state-colored row outlines, move all bulk actions into a top `⋮` menu driven by the selection's state union, add server-download removal and offline bulk marking.

**Architecture:** One new server endpoint (`POST /chapters/remove-downloads`); a pure `menu_actions` helper decides menu entries from (selection states × client capabilities); a localStorage marks-outbox overlays read state offline and flushes on reconnect; outlines are CSS classes computed from `DownloadState` + device marks + offline flag.

**Tech Stack:** Rust — axum/sqlx server, Leptos UI, CSS.

**Spec:** `docs/superpowers/specs/2026-07-10-chapter-download-ux-design.md`

Branch `feature/chapter-download-ux` (spec committed). Standard commit trailer.

---

### Task 1: Server — remove downloaded chapters

**Files:**
- Modify: `crates/yomu-server/src/db.rs`, `crates/yomu-server/src/api/chapters.rs`, `crates/yomu-server/src/api/mod.rs`

- [ ] **Step 1: Failing db test** (reuse `details` helper; promote a chapter to 'downloaded' the way `library_lifecycle`-adjacent tests do — `mark_pending` then the downloader's completion setter; find it with `grep -n "download_state = 'downloaded'" crates/yomu-server/src/db.rs` and use the same public method, e.g. `mark_downloaded`):

```rust
#[tokio::test]
async fn remove_downloads_resets_only_downloaded_rows() {
    let db = Db::in_memory().await.unwrap();
    let manga = db
        .insert_manga("fixture", &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]), false)
        .await
        .unwrap();
    let chapters = db.list_chapters(manga.id).await.unwrap();
    db.mark_pending(&[chapters[0].id]).await.unwrap();
    // …promote chapters[0] to downloaded via the downloader's setter…
    let removed = db
        .remove_downloads(&[chapters[0].id, chapters[1].id])
        .await
        .unwrap();
    assert_eq!(removed, vec![chapters[0].id]); // the 'none' row is skipped
    let after = db.list_chapters(manga.id).await.unwrap();
    assert!(matches!(after[0].download, DownloadState::None));
    // page_count survives: it is still true knowledge about the chapter
    assert_eq!(after[0].page_count, chapters[0].page_count);
}
```

- [ ] **Step 2: Run to fail** — `cargo test -p yomu-server remove_downloads` → method missing.

- [ ] **Step 3: db method** (in the downloads section of `impl Db`):

```rust
    /// Forget the server copies of these chapters: rows go back to
    /// 'none' (page_count survives — still true knowledge). Returns the
    /// ids that actually were downloaded, so the caller can delete their
    /// page directories.
    pub async fn remove_downloads(&self, chapter_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let mut removed = Vec::new();
        for id in chapter_ids {
            let result = sqlx::query(
                "UPDATE chapters SET download_state = 'none', downloaded_at = NULL,
                                     download_error = NULL
                 WHERE id = ? AND download_state = 'downloaded'",
            )
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                removed.push(*id);
            }
        }
        Ok(removed)
    }
```

- [ ] **Step 4: Endpoint** — in api/chapters.rs (mirror `download_many`'s shape; it also shows which extractors/types to import):

```rust
/// Remove the server copies of the given chapters (pages deleted, rows
/// reset). Chapters not currently downloaded are skipped.
pub async fn remove_downloads(
    State(state): State<AppState>,
    Json(req): Json<DownloadChaptersRequest>,
) -> Result<Json<BulkChaptersResponse>, ApiError> {
    let removed = state.db.remove_downloads(&req.chapter_ids).await?;
    for id in &removed {
        // The page dir needs the owning manga; chapters store it.
        if let Ok(chapter) = state.db.get_chapter(*id).await {
            let _ = tokio::fs::remove_dir_all(state.chapter_dir(chapter.manga_id, *id)).await;
        }
    }
    state.live_pages.invalidate_many(&removed).await;
    Ok(Json(BulkChaptersResponse {
        affected: removed.len() as u32,
    }))
}
```

(Check `get_chapter` exists — `grep -n "fn get_chapter" crates/yomu-server/src/db.rs`; if named differently, use the real accessor.) Route in api/mod.rs next to `/chapters/download`:

```rust
        .route(
            "/chapters/remove-downloads",
            axum::routing::post(chapters::remove_downloads),
        )
```

- [ ] **Step 5: Run + commit** — `cargo test -p yomu-server` PASS → `feat(server): endpoint to remove downloaded chapters`.

---

### Task 2: Client plumbing — remove_downloads + local delete + marks outbox

**Files:**
- Modify: `crates/yomu-client/src/lib.rs`, `crates/yomu-ui/src/offline.rs`, `crates/yomu-ui/src/lib.rs`

- [ ] **Step 1: yomu-client method** (next to `download_chapters`):

```rust
    /// Remove the server copies of these chapters.
    pub async fn remove_downloads(&self, ids: &[Uuid]) -> Result<BulkChaptersResponse> {
        let req = self
            .http
            .post(self.url("api/v1/chapters/remove-downloads")?)
            .json(&DownloadChaptersRequest {
                chapter_ids: ids.to_vec(),
            });
        self.send(req).await
    }
```

- [ ] **Step 2: offline.rs — shell delete + unmark** (next to `shell_save_chapter` / `mark_device_chapter`):

```rust
/// Delete a device-saved chapter (shell storage) and forget its mark.
pub async fn shell_delete_chapter(chapter_id: Uuid) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &args,
        &"chapter".into(),
        &chapter_id.to_string().into(),
    );
    shell_invoke("device_delete_chapter", args).await?;
    Ok(())
}

/// Drop a chapter's "on this device" mark (after deletion).
pub fn unmark_device_chapter(id: Uuid) {
    let mut marks = device_chapters();
    marks.remove(&id);
    write_json(DEVICE_KEY, &marks);
}
```

(Match `shell_invoke`'s actual argument conventions by reading `shell_save_chapter` first; the shell command takes `chapter: String`.)

- [ ] **Step 3: offline.rs — marks outbox**:

```rust
const MARKS_KEY: &str = "yomu-marks-outbox";

/// Read marks made while the server was unreachable: chapter → desired
/// state, last write wins. Flushed by `flush_marks`.
pub fn pending_marks() -> std::collections::BTreeMap<Uuid, bool> {
    read_json(MARKS_KEY)
}

pub fn queue_marks(ids: &[Uuid], read: bool) {
    let mut marks = pending_marks();
    for id in ids {
        marks.insert(*id, read);
    }
    write_json(MARKS_KEY, &marks);
}

/// Replay queued read marks; entries survive failed flushes.
pub async fn flush_marks(client: &yomu_client::YomuClient) {
    let marks = pending_marks();
    if marks.is_empty() {
        return;
    }
    let (read, unread): (Vec<_>, Vec<_>) = marks.iter().partition(|(_, r)| **r);
    let read: Vec<Uuid> = read.into_iter().map(|(id, _)| *id).collect();
    let unread: Vec<Uuid> = unread.into_iter().map(|(id, _)| *id).collect();
    let mut flushed: Vec<Uuid> = Vec::new();
    if !read.is_empty() && client.mark_chapters(&read, true).await.is_ok() {
        flushed.extend(read);
    }
    if !unread.is_empty() && client.mark_chapters(&unread, false).await.is_ok() {
        flushed.extend(unread);
    }
    if !flushed.is_empty() {
        let mut marks = pending_marks();
        for id in &flushed {
            marks.remove(id);
        }
        write_json(MARKS_KEY, &marks);
        leptos::logging::log!("synced {} offline read mark(s)", flushed.len());
    }
}
```

- [ ] **Step 4: lib.rs — flush alongside the progress outbox** — in `App`, both spawn sites currently call `offline::flush_outbox(&client)`; extend each to also `offline::flush_marks(&client).await`.

- [ ] **Step 5: Run + commit** — wasm check + workspace tests PASS → `feat(ui): local chapter removal and offline read-mark outbox`.

---

### Task 3: Pure menu-actions helper

**Files:**
- Create: `crates/yomu-ui/src/chapter_actions.rs`
- Modify: `crates/yomu-ui/src/lib.rs` (`mod chapter_actions;`)

- [ ] **Step 1: Write module with failing tests**

```rust
//! Which bulk actions the chapter-selection menu offers, from the
//! union of the selected chapters' storage states and what this client
//! can do. Pure so the matrix is unit-testable.

/// Storage state of one selected chapter, as the menu cares about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChapterState {
    pub on_server: bool,
    pub on_device: bool,
}

/// What this client/context can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub online: bool,
    /// Shell storage or an active service worker.
    pub local_tier: bool,
    /// Shell only (web can't reliably evict its cache).
    pub local_remove: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    DownloadServer,
    DownloadBoth,
    DownloadLocal,
    RemoveServer,
    RemoveLocal,
    MarkRead,
    MarkUnread,
    MarkBeforeRead,
    MarkAfterUnread,
}

/// Menu entries for a selection: every action that would affect at
/// least one selected chapter (mixed selections show the union;
/// no-op chapters are skipped by the action handlers).
pub fn menu_actions(states: &[ChapterState], caps: Caps) -> Vec<Action> {
    let mut out = Vec::new();
    let any_missing_server = states.iter().any(|s| !s.on_server);
    let any_server = states.iter().any(|s| s.on_server);
    let any_server_not_local = states.iter().any(|s| s.on_server && !s.on_device);
    let any_local = states.iter().any(|s| s.on_device);
    if caps.online && any_missing_server {
        out.push(Action::DownloadServer);
        if caps.local_tier {
            out.push(Action::DownloadBoth);
        }
    }
    if caps.online && caps.local_tier && any_server_not_local {
        out.push(Action::DownloadLocal);
    }
    if caps.online && any_server {
        out.push(Action::RemoveServer);
    }
    if caps.local_remove && any_local {
        out.push(Action::RemoveLocal);
    }
    // Read marks work offline through the marks outbox.
    out.extend([
        Action::MarkRead,
        Action::MarkUnread,
        Action::MarkBeforeRead,
        Action::MarkAfterUnread,
    ]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S00: ChapterState = ChapterState { on_server: false, on_device: false };
    const S10: ChapterState = ChapterState { on_server: true, on_device: false };
    const S11: ChapterState = ChapterState { on_server: true, on_device: true };

    const APP_ONLINE: Caps = Caps { online: true, local_tier: true, local_remove: true };
    const WEB_ONLINE: Caps = Caps { online: true, local_tier: true, local_remove: false };
    const APP_OFFLINE: Caps = Caps { online: false, local_tier: true, local_remove: true };

    fn has(actions: &[Action], a: Action) -> bool {
        actions.contains(&a)
    }

    #[test]
    fn undownloaded_online_offers_server_and_both() {
        let a = menu_actions(&[S00], APP_ONLINE);
        assert!(has(&a, Action::DownloadServer) && has(&a, Action::DownloadBoth));
        assert!(!has(&a, Action::RemoveServer) && !has(&a, Action::RemoveLocal));
    }

    #[test]
    fn server_only_offers_local_pull_and_server_remove() {
        let a = menu_actions(&[S10], APP_ONLINE);
        assert!(has(&a, Action::DownloadLocal) && has(&a, Action::RemoveServer));
        assert!(!has(&a, Action::DownloadServer));
    }

    #[test]
    fn both_offers_both_removals_only() {
        let a = menu_actions(&[S11], APP_ONLINE);
        assert!(has(&a, Action::RemoveServer) && has(&a, Action::RemoveLocal));
        assert!(!has(&a, Action::DownloadServer) && !has(&a, Action::DownloadLocal));
    }

    #[test]
    fn mixed_selection_shows_the_union() {
        let a = menu_actions(&[S00, S10, S11], APP_ONLINE);
        for action in [
            Action::DownloadServer,
            Action::DownloadBoth,
            Action::DownloadLocal,
            Action::RemoveServer,
            Action::RemoveLocal,
        ] {
            assert!(has(&a, action), "{action:?} missing from union");
        }
    }

    #[test]
    fn web_never_offers_local_remove() {
        assert!(!has(&menu_actions(&[S11], WEB_ONLINE), Action::RemoveLocal));
    }

    #[test]
    fn offline_offers_only_local_remove_and_marks() {
        let a = menu_actions(&[S11], APP_OFFLINE);
        assert_eq!(
            a,
            vec![
                Action::RemoveLocal,
                Action::MarkRead,
                Action::MarkUnread,
                Action::MarkBeforeRead,
                Action::MarkAfterUnread,
            ],
        );
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p yomu-ui chapter_actions` → PASS (pure module; tests and logic land together but every branch is asserted).

- [ ] **Step 3: Commit** — `feat(ui): pure chapter-selection action matrix`.

---

### Task 4: Row outlines (CSS + ChapterItem de-buttoning)

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs` (`ChapterItem`, ~448-700), `crates/yomu-web/styles.css`

- [ ] **Step 1: ChapterItem** — remove the two `icon-btn` buttons and their closures (`download`, `device_download`, `server_glyph` block, `device_busy`); keep `on_device` (drives outlines + offline availability). Add state classes on the `<li>`:

```rust
    let on_server = matches!(chapter.download, DownloadState::Downloaded { .. });
    let dl_busy = matches!(
        chapter.download,
        DownloadState::Pending | DownloadState::Downloading
    );
    let dl_failed = matches!(chapter.download, DownloadState::Failed { .. });
    let failed_reason = match &chapter.download {
        DownloadState::Failed { reason, .. } => Some(format!("Download failed: {reason}")),
        _ => None,
    };
```

and on the view: `class:dl-server=move || on_server && !on_device.get()`,
`class:dl-local=move || on_device.get() && !on_server`,
`class:dl-both=move || on_server && on_device.get()`,
`class:dl-busy=dl_busy`, `class:dl-failed=dl_failed`; merge
`failed_reason` into the row `title` (alongside the offline hint).
Offline+local rows: `dl-local` already colors them green; the
`unavailable` class continues to handle offline-without-local.

- [ ] **Step 2: CSS** — add `--server: #60a5fa;` (and a paper-theme–legible override if needed) to `:root`; then:

```css
.chapter-item {
  border: 2px solid var(--border);
}

.chapter-item.dl-server {
  border-color: var(--server);
}

.chapter-item.dl-local {
  border-color: var(--saved);
}

/* on the server AND on this device: blue left half, green right half */
.chapter-item.dl-both {
  border: 2px solid transparent;
  background:
    linear-gradient(var(--surface), var(--surface)) padding-box,
    linear-gradient(90deg, var(--server) 50%, var(--saved) 50%) border-box;
}

.chapter-item.dl-busy {
  border-color: var(--server);
  animation: dl-pulse 1.2s ease-in-out infinite;
}

.chapter-item.dl-failed {
  border-color: var(--down);
}

@keyframes dl-pulse {
  50% { border-color: color-mix(in srgb, var(--server) 35%, transparent); }
}
```

Check `.chapter-item`'s existing border/background rules first and fold these in (don't double borders); `dl-both`'s background trick must preserve the row's surface color — verify against the real rule.

- [ ] **Step 3: Run + commit** — wasm check + `just check` PASS → `feat(ui): chapter rows show storage state as colored outlines`.

---

### Task 5: ⋮ menu, slim select bar, actions

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs` (chapter-list component around lines 320–446), `crates/yomu-web/styles.css`

- [ ] **Step 1: State plumbing** — the list component needs per-chapter states for the helper: build alongside `ids` a `StoredValue<Vec<ChapterState>>` (reading order, same indexing as `ids`) from `chapter.download` + `offline::device_chapters()`. Caps:

```rust
    let caps = crate::chapter_actions::Caps {
        online: !offline,
        local_tier: offline::shell_available() || offline::service_worker_active(),
        local_remove: offline::shell_available(),
    };
```

- [ ] **Step 2: Menu component** — a `menu_open: RwSignal<bool>` + a `⋮` button rendered in the chapter-list header row (add a header flex row above `<ul class="chapter-list">` holding a "Chapters" label and the button). Menu body:
  - selection empty → single "Select" entry (`selected.set` the first... no: entering selection mode with nothing selected needs an explicit flag; add `select_mode: RwSignal<bool>` OR seed selection-by-tap: simplest is a `forced_selection: RwSignal<bool>` that `selection_active` ORs in: `Memo::new(move |_| forced.get() || !selected.with(|s| s.is_empty()))`; "Select" sets `forced=true`, ✕ clears both).
  - selection active → entries from `menu_actions(&selected_states, caps)`, each a button with its label:
    `Download (server)` / `Download (both)` / `Download (local)` / `Remove (server)` / `Remove (local)` / `Mark read` / `Mark unread` / `Mark all before as read` / `Mark all after as unread`.
- [ ] **Step 3: Action handlers** (each filters the selection to affected chapters, runs, then `clear()` + `refresh`):
  - DownloadServer: existing `download_selected` filtered to `!on_server`.
  - DownloadBoth: same + add those ids to a page-level `pull_queue: RwSignal<HashSet<Uuid>>` (declared next to `selected` in `MangaPage` so refreshes don't wipe it). New Effect in `MangaPage`: on detail change, for every id in `pull_queue` whose chapter is now `Downloaded`, remove from the queue and spawn the device save (`shell_save_chapter` or `prefetch_chapter`, then `mark_device_chapter`). The existing 2s poll keeps refreshing while anything is pending, which drives this chain.
  - DownloadLocal: for selected with server copy and no mark: same device-save spawn directly.
  - RemoveServer: `client.remove_downloads(&ids)` (ids filtered to on_server).
  - RemoveLocal: for each selected with a mark: `shell_delete_chapter(id).await` then `unmark_device_chapter(id)`; refresh.
  - MarkRead/MarkUnread: current `mark`, but offline-capable: try `client.mark_chapters`; on error `offline::queue_marks(&ids, read)` and still refresh (the overlay shows it).
  - MarkBeforeRead: ids with reading-order index `< min(selected indices)` → same offline-capable mark path with `read=true`.
  - MarkAfterUnread: index `> max(selected indices)` → `read=false`.
- [ ] **Step 4: Read overlay** — where `ChapterItem` receives `chapter`, apply `chapter.read = offline::pending_marks().get(&chapter.id).copied().unwrap_or(chapter.read);` (do it once in the list map, reading `pending_marks()` a single time per render).
- [ ] **Step 5: Slim the select bar** — keep count, All, ✕; drop Download/Read/Unread buttons (they live in ⋮ now). CSS for the menu: reuse `.reader-menu`-style dropdown or a simple absolutely-positioned `.chapter-menu` under the header button (match existing dropdown styling if one exists; else minimal new block).
- [ ] **Step 6: Run + commit** — full checks PASS → `feat(ui): chapter selection menu with download/remove/mark actions`.

---

### Task 6: E2E verification + PR

- [ ] Scratch server + headless firefox at phone size:
  1. outlines: fresh manga rows gray; download two → pulsing then blue; pull one local (via menu) → split border; screenshot.
  2. menu: long-press one blue row → ⋮ shows Download (local)/Remove (server)/marks (web caps: no Remove (local)); mixed selection → union.
  3. before/after marks: select a middle chapter, "mark all before as read" → older rows gain the read style.
  4. offline marking: block the server (stop it), reload (cached detail), mark a chapter read → row shows read; restart server, trigger the online flush (reload), GET the detail → server agrees.
  5. `Remove (server)`: row returns to gray, pages dir gone from the scratch data_dir.
- [ ] Screenshots to the user; PR into develop with verification evidence; auto-merge. APK-relevant note in the body.

---

## Self-review notes

- Spec coverage: endpoint (T1), client + local delete + marks outbox + flush (T2), action matrix incl. web/offline rules (T3), outlines incl. busy/failed/split (T4), menu/selection/actions/overlay/both-chain (T5), E2E incl. offline round-trip (T6).
- Type consistency: `ChapterState`/`Caps`/`Action` defined in T3 and consumed in T5; `remove_downloads` name shared by db, endpoint, client.
- The `forced_selection` flag is the one structural addition to selection state; documented in T5 step 2.
