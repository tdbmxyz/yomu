# Unified Downloads Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Spec: docs/superpowers/specs/2026-07-18-unified-downloads-design.md

**Goal:** Show in-flight local (device) downloads in the Downloads tab in their own cancelable section, make chapter rows flip to their on-device style live, and add a sixth phone tab-bar entry for Downloads.

**Architecture:** Two app-level Leptos context signals — `LocalDownloads` (in-flight device saves) and `DeviceMarks` (reactive mirror of the saved-chapter localStorage marks) — provided in `App`. The manga page writes `LocalDownloads` as saves run (server-tier ring progress stays a page-local signal), the Downloads tab reads it, and chapter rows read `DeviceMarks` so a completed save updates their style without a remount. The save loop gains a cancel check.

**Tech Stack:** Leptos (wasm), Tauri shell bridge, existing yomu-ui offline module.

---

### Task 1: App-level context stores

**Files:**
- Modify: `crates/yomu-ui/src/lib.rs`

- [ ] **Step 1: define the store types + helpers.** Near the top of `lib.rs` (after the `use_connectivity` helper around line 43), add:

```rust
/// One in-flight local (device) save, shown on the manga page ring and
/// in the Downloads tab's device section. Keyed by chapter id in the
/// `LocalDownloads` map.
#[derive(Clone, PartialEq)]
pub struct LocalDownload {
    pub manga_id: uuid::Uuid,
    pub manga_title: String,
    pub chapter_title: String,
    pub done: u32,
    pub total: u32,
    pub failed: bool,
    pub cancel_requested: bool,
}

pub type LocalDownloads =
    leptos::prelude::RwSignal<std::collections::HashMap<uuid::Uuid, LocalDownload>>;

/// Reactive mirror of the device-saved-chapter marks (localStorage), so a
/// row flips to its on-device style the instant a save completes.
pub type DeviceMarks =
    leptos::prelude::RwSignal<std::collections::BTreeMap<uuid::Uuid, crate::offline::DeviceMark>>;

pub fn use_local_downloads() -> LocalDownloads {
    use_context().expect("LocalDownloads provided by App")
}

pub fn use_device_marks() -> DeviceMarks {
    use_context().expect("DeviceMarks provided by App")
}
```

- [ ] **Step 2: provide them in `App`.** In `pub fn App` (after `provide_context(conn);` at line 50), add:

```rust
    let local_downloads: LocalDownloads = RwSignal::new(std::collections::HashMap::new());
    provide_context(local_downloads);
    let device_marks: DeviceMarks = RwSignal::new(offline::device_chapters());
    provide_context(device_marks);
```

`offline::device_chapters()` already returns `BTreeMap<Uuid, DeviceMark>`; `DeviceMark` is `pub` in offline.rs.

- [ ] **Step 3: verify + commit.**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: compiles (dead-code warnings for the unused helpers are fine this task).

```bash
git add crates/yomu-ui/src/lib.rs
git commit -m "feat(ui): app-level local-downloads and device-marks stores"
```

### Task 2: Migrate local ring progress to the app store

The manga page's `progress: ProgressMap` currently holds both tiers. Split it: the **local** tier moves to `LocalDownloads` (Task 1); the **server** tier stays a page-local signal. Rows draw a ring from whichever has an entry.

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs`

- [ ] **Step 1: replace the page-local progress type with a server-only map.** At lines 12-28, replace the `ProgressTier` / `RowProgress` / `ProgressMap` definitions with:

```rust
/// Which download tier a live progress ring belongs to (its color).
#[derive(Clone, Copy, PartialEq)]
enum ProgressTier {
    Server,
    Local,
}

/// What a row renders as its perimeter ring, sourced from either the
/// page-local server map or the app-level `LocalDownloads` store.
#[derive(Clone, Copy)]
struct RowProgress {
    done: u32,
    total: u32,
    tier: ProgressTier,
    failed: bool,
}

/// Server-tier per-chapter progress, page-local (polled from /downloads).
type ServerProgress = RwSignal<std::collections::HashMap<Uuid, (u32, u32)>>;
```

- [ ] **Step 2: rename the MangaPage signal.** At line 46 replace:

```rust
    let progress: ProgressMap = RwSignal::new(std::collections::HashMap::new());
```

with:

```rust
    let server_progress: ServerProgress = RwSignal::new(std::collections::HashMap::new());
    let local_downloads = crate::use_local_downloads();
    let device_marks = crate::use_device_marks();
```

- [ ] **Step 3: update the server-progress poll.** In the busy-poll effect (lines 150-172), the block currently writes `progress`. Replace the two `progress.update(...)` blocks with server-only writes:

```rust
                    server_progress.update(|map| {
                        map.clear();
                        for entry in &downloads.queue {
                            if let Some(p) = &entry.progress {
                                map.insert(entry.chapter_id, (p.page, p.total));
                            }
                        }
                    });
```

and the `else if !busy` branch (lines 168-173):

```rust
        } else if !busy {
            // queue drained: no server ring should linger
            server_progress.update(|map| map.clear());
        }
```

- [ ] **Step 4: rewrite `save_locally`** (lines 426-475) to write the app store, carrying titles, and honoring cancel (the cancel predicate is wired in Task 3 — here it just uses the store):

```rust
async fn save_locally(
    client: &yomu_client::YomuClient,
    manga_id: Uuid,
    manga_title: String,
    id: Uuid,
    chapter_title: String,
    local: crate::LocalDownloads,
    device_marks: crate::DeviceMarks,
) -> Result<(), String> {
    local.update(|map| {
        map.insert(
            id,
            crate::LocalDownload {
                manga_id,
                manga_title: manga_title.clone(),
                chapter_title: chapter_title.clone(),
                done: 0,
                total: 0,
                failed: false,
                cancel_requested: false,
            },
        );
    });
    let should_cancel =
        move || local.with_untracked(|m| m.get(&id).is_some_and(|d| d.cancel_requested));
    let result = offline::save_chapter_with_progress(
        client,
        id,
        |done, total| {
            local.update(|map| {
                if let Some(d) = map.get_mut(&id) {
                    d.done = done;
                    d.total = total;
                }
            });
        },
        should_cancel,
    )
    .await;
    match result {
        Ok(offline::SaveOutcome::Done(count)) => {
            offline::mark_device_chapter(manga_id, id, count);
            device_marks.update(|m| {
                m.insert(id, offline::DeviceMark { manga: manga_id, pages: count });
            });
            local.update(|map| {
                map.remove(&id);
            });
            Ok(())
        }
        Ok(offline::SaveOutcome::Cancelled) => {
            local.update(|map| {
                map.remove(&id);
            });
            Ok(())
        }
        Err(err) => {
            local.update(|map| {
                if let Some(d) = map.get_mut(&id) {
                    d.failed = true;
                } else {
                    map.insert(
                        id,
                        crate::LocalDownload {
                            manga_id,
                            manga_title,
                            chapter_title,
                            done: 0,
                            total: 1,
                            failed: true,
                            cancel_requested: false,
                        },
                    );
                }
            });
            set_timeout(
                move || {
                    local.try_update(|map| {
                        map.remove(&id);
                    });
                },
                std::time::Duration::from_millis(1500),
            );
            Err(err)
        }
    }
}
```

(`offline::SaveOutcome` and the third `should_cancel` param are added in Task 3; write them here so the file is consistent, and Task 3 makes the loop match. Do NOT run the wasm check until the end of Task 3, since the signature won't line up until then.)

- [ ] **Step 5: thread the stores + titles through the components.** The `progress: ProgressMap` prop currently threads MangaPage → MangaDetail → ChapterList → ChapterItem. Replace it with `server_progress: ServerProgress`, and give `ChapterList` a `manga_title: String` prop. Concretely:

  In `MangaDetail`'s call site (line 226): change `progress` to `server_progress` (add `let server_progress = ...`? No — `server_progress` is a MangaPage local; pass it down). Update the `<MangaDetail .../>` invocation to pass `server_progress` and add nothing else (MangaDetail forwards it). MangaDetail prop (line 246) `progress: ProgressMap` → `server_progress: ServerProgress`. Its `<ChapterList .../>` (lines 400-411) passes `server_progress` and `manga_title=manga.title.clone()`.

  `ChapterList` signature (line 488): replace `progress: ProgressMap` with `server_progress: ServerProgress` and add `manga_title: String`. `ChapterItem` (line 786) replace `progress: ProgressMap` with `server_progress: ServerProgress`.

- [ ] **Step 6: build the per-chapter title lookup + update the two `save_locally` callers.**

  In `ChapterList`, after `let ids = StoredValue::new(...)` (line 507) add:

```rust
    let titles = StoredValue::new(
        chapters
            .iter()
            .map(|c| (c.id, c.title.clone()))
            .collect::<std::collections::HashMap<Uuid, String>>(),
    );
    let manga_title = StoredValue::new(manga_title);
    let local_downloads = crate::use_local_downloads();
    let device_marks = crate::use_device_marks();
```

  Rewrite the `Action::DownloadLocal` arm (lines 633-645):

```rust
            Action::DownloadLocal => {
                let pull = ids_where(|s| s.on_server && !s.on_device);
                let client = use_client();
                let mtitle = manga_title.get_value();
                let ctitles = titles.get_value();
                spawn_local(async move {
                    for id in pull {
                        let ct = ctitles.get(&id).cloned().unwrap_or_default();
                        if let Err(err) = save_locally(
                            &client, manga_id, mtitle.clone(), id, ct,
                            local_downloads, device_marks,
                        )
                        .await
                        {
                            status.set(Some(format!("Local save failed: {err}")));
                            leptos::logging::warn!("local download: {err}");
                        }
                    }
                    refresh.update(|n| *n += 1);
                });
            }
```

  In `MangaPage`'s pull-queue drain (lines 210-218), the closure has `d` (the detail) in scope at line 188. Replace the drain's spawn body to pass titles + stores:

```rust
            let client = client.clone();
            let mtitle = d.manga.title.clone();
            let ctitles: std::collections::HashMap<Uuid, String> =
                d.chapters.iter().map(|c| (c.id, c.title.clone())).collect();
            spawn_local(async move {
                for qid in ready {
                    let ct = ctitles.get(&qid).cloned().unwrap_or_default();
                    if let Err(err) = save_locally(
                        &client, id, mtitle.clone(), qid, ct,
                        local_downloads, device_marks,
                    )
                    .await
                    {
                        status.set(Some(format!("Local save failed: {err}")));
                        leptos::logging::warn!("local pull: {err}");
                    }
                }
                refresh.update(|n| *n += 1);
            });
```

  (`local_downloads` and `device_marks` were bound in MangaPage in Step 2; capture them into the effect closure the same way `client` is.)

- [ ] **Step 7: make the row ring read both sources.** In `ChapterItem`, replace `let row_progress = move || progress.with(|map| map.get(&id).copied());` (line 790) with:

```rust
    let local_downloads = crate::use_local_downloads();
    let row_progress = move || {
        if let Some(d) = local_downloads.with(|m| m.get(&id).cloned()) {
            Some(RowProgress {
                done: d.done,
                total: d.total,
                tier: ProgressTier::Local,
                failed: d.failed,
            })
        } else {
            server_progress
                .with(|m| m.get(&id).copied())
                .map(|(done, total)| RowProgress {
                    done,
                    total,
                    tier: ProgressTier::Server,
                    failed: false,
                })
        }
    };
```

  The existing ring `view!` block that consumes `row_progress()` and matches on `p.tier` / `p.failed` is unchanged.

- [ ] **Step 8:** deferred to end of Task 3 (signature of `save_chapter_with_progress` changes there). No standalone commit; Tasks 2+3 commit together.

### Task 3: Cancelable save loop

**Files:**
- Modify: `crates/yomu-ui/src/offline.rs`

- [ ] **Step 1: add the outcome type + cancel param.** Replace the signature and body head of `save_chapter_with_progress` (lines 176-196) so it takes `should_cancel` and returns `SaveOutcome`:

```rust
/// Result of a device save: how many pages, or that the caller cancelled.
pub enum SaveOutcome {
    Done(u32),
    Cancelled,
}

pub async fn save_chapter_with_progress(
    client: &yomu_client::YomuClient,
    chapter_id: Uuid,
    on_page: impl Fn(u32, u32),
    should_cancel: impl Fn() -> bool,
) -> Result<SaveOutcome, String> {
    let shell = shell_available();
    if !shell && !service_worker_active() {
        return Err(
            "offline cache unavailable (no service worker; first visit or unsupported browser)"
                .into(),
        );
    }
    if should_cancel() {
        return Ok(SaveOutcome::Cancelled);
    }
    let meta = client
        .chapter_pages(chapter_id)
        .await
        .map_err(|e| e.to_string())?;
    let total = meta.page_count;
    on_page(0, total);
    if shell {
        shell_chapter_command("device_begin_chapter", chapter_id, None).await?;
    }
```

- [ ] **Step 2: check between pages + finish.** Replace the page loop tail and finish (lines 197-216) so each iteration bails on cancel and cleans the partial:

```rust
    for n in 0..total {
        if should_cancel() {
            if shell {
                let _ = shell_delete_chapter(chapter_id).await;
            }
            return Ok(SaveOutcome::Cancelled);
        }
        if shell {
            let args = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&args, &"base".into(), &client.base().to_string().into());
            let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
            let _ = js_sys::Reflect::set(&args, &"page".into(), &(n as f64).into());
            shell_invoke("device_save_page", args)
                .await
                .map_err(|e| format!("page {n}: {e}"))?;
        } else {
            client
                .fetch_page(chapter_id, n)
                .await
                .map_err(|e| format!("page {n}: {e}"))?;
        }
        on_page(n + 1, total);
    }
    if shell {
        shell_chapter_command("device_finish_chapter", chapter_id, None).await?;
    }
    Ok(SaveOutcome::Done(total))
}
```

(`shell_delete_chapter` already exists in this file and invokes `device_delete_chapter`.)

- [ ] **Step 3: verify Tasks 2+3 together.**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: compiles.

Run: `cd crates/yomu-web && trunk build --release`
Expected: `✅ success`.

- [ ] **Step 4: commit.**

```bash
git add crates/yomu-ui/src/pages/manga.rs crates/yomu-ui/src/offline.rs
git commit -m "feat(ui): app-level local downloads with cancel; server ring stays page-local"
```

### Task 4: Live on-device row status (reactive marks)

The `mark`/`unmark` write-through into `DeviceMarks` is already done in Task 2's `save_locally` (mark) — this task adds it to the **remove** path and switches the row to read the signal.

**Files:**
- Modify: `crates/yomu-ui/src/pages/manga.rs`

- [ ] **Step 1: write-through on remove.** In `ChapterList`'s `Action::RemoveLocal` arm (lines 659-670), update `device_marks` when a chapter is removed:

```rust
            Action::RemoveLocal => {
                let rm = ids_where(|s| s.on_device);
                spawn_local(async move {
                    for id in rm {
                        match offline::shell_delete_chapter(id).await {
                            Ok(()) => {
                                offline::unmark_device_chapter(id);
                                device_marks.update(|m| {
                                    m.remove(&id);
                                });
                            }
                            Err(err) => leptos::logging::warn!("local remove: {err}"),
                        }
                    }
                    refresh.update(|n| *n += 1);
                });
            }
```

(`device_marks` was bound in ChapterList in Task 2 Step 6.)

- [ ] **Step 2: row reads the signal.** In `ChapterItem`, replace the seeded signal (line 858):

```rust
    let on_device = RwSignal::new(offline::device_chapters().contains_key(&id));
```

with a reactive read of the app store:

```rust
    let device_marks = crate::use_device_marks();
    let on_device = move || device_marks.with(|m| m.contains_key(&id));
```

Then update every `on_device.get()` in the `view!` (lines 878-887) to a call `on_device()`:

```rust
            class:unavailable=move || offline && !on_device()
            class:dl-server=move || on_server && !on_device()
            class:dl-local=move || on_device() && !on_server
            class:dl-both=move || on_server && on_device()
```

and in the `title` closure (line 886): `if offline && !on_device() {`.

- [ ] **Step 3: verify + commit.**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: compiles.

```bash
git add crates/yomu-ui/src/pages/manga.rs
git commit -m "feat(ui): chapter row flips to on-device style live on save/remove"
```

### Task 5: Downloads tab device section + cancel

**Files:**
- Modify: `crates/yomu-ui/src/pages/downloads.rs`
- Modify: `crates/yomu-web/styles.css`

- [ ] **Step 1: read the local store + render a device section.** In `Downloads` (the outer component, before the `view!` at line 45), add:

```rust
    let local = crate::use_local_downloads();
```

Pass it to `DownloadsView` by adding a prop. In `DownloadsView` (line 66), add `local: crate::LocalDownloads` to the signature, and thread it from the call site (line 58): `view! { <DownloadsView resp device_count local refetch/> }`.

- [ ] **Step 2: the device-section view.** In `DownloadsView`'s `view!`, after the server sections (after the failed-group block, before the closing of the fragment), add a labeled device section:

```rust
        <h3 class="shelf-title">"On this device"</h3>
        {move || {
            let items: Vec<_> = local.with(|m| {
                let mut v: Vec<_> = m.iter().map(|(id, d)| (*id, d.clone())).collect();
                v.sort_by(|a, b| a.1.manga_title.cmp(&b.1.manga_title));
                v
            });
            if items.is_empty() {
                view! {
                    <p class="muted">{format!("{device_count} chapters on this device")}</p>
                }
                .into_any()
            } else {
                view! {
                    <ul class="download-list">
                        {items
                            .into_iter()
                            .map(|(id, d)| view! { <LocalRow id d local/> })
                            .collect_view()}
                    </ul>
                }
                .into_any()
            }
        }}
```

`device_count` is `Copy` (u32); capturing it in the closure is fine. `local` is a `Copy` signal.

- [ ] **Step 3: the `LocalRow` component** (add at the end of downloads.rs):

```rust
/// One in-flight device save: manga · chapter, a page progress bar, and a
/// Cancel button that flags the save loop to stop.
#[component]
fn LocalRow(id: uuid::Uuid, d: crate::LocalDownload, local: crate::LocalDownloads) -> impl IntoView {
    let cancel = move |_| {
        local.update(|m| {
            if let Some(entry) = m.get_mut(&id) {
                entry.cancel_requested = true;
            }
        });
    };
    let pct = if d.total > 0 {
        (d.done as f64 / d.total as f64) * 100.0
    } else {
        0.0
    };
    view! {
        <li class="download-row" class:dl-failed=d.failed>
            <a class="download-title" href=format!("/manga/{}", d.manga_id)>
                <strong>{d.manga_title}</strong>
                " · " {d.chapter_title}
            </a>
            <div class="download-progress">
                <div class="download-progress-bar" style:width=format!("{pct}%")></div>
                <span class="muted download-progress-label">
                    {if d.cancel_requested {
                        "Cancelling…".to_string()
                    } else {
                        format!("{}/{}", d.done, d.total)
                    }}
                </span>
            </div>
            <button class="button" on:click=cancel disabled=d.cancel_requested>
                "Cancel"
            </button>
        </li>
    }
}
```

- [ ] **Step 4: CSS.** In `crates/yomu-web/styles.css`, near the existing `.download-row` rules, ensure the row lays the Cancel button out inline. Add:

```css
.download-row { align-items: center; }
.download-row .button { margin-left: auto; flex: 0 0 auto; }
```

(If `.download-row` already sets `display`, keep it; only add the two declarations above. If it is not flex, add `display: flex; gap: .5rem;` to it.)

- [ ] **Step 5: verify + commit.**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: compiles.

```bash
git add crates/yomu-ui/src/pages/downloads.rs crates/yomu-web/styles.css
git commit -m "feat(ui): device downloads section with cancel in the Downloads tab"
```

### Task 6: Phone tab-bar Downloads entry

**Files:**
- Modify: `crates/yomu-ui/src/lib.rs`
- Modify: `crates/yomu-web/styles.css`

- [ ] **Step 1: add the tab.** In the `.tabbar` nav (lib.rs lines 122-129), add a Downloads entry between Sources/Search and More. The current bar has Home / Library / Sources / Search / More. Insert after the Search `<A>`:

```rust
                    <A href="/downloads"><span class="tab-icon">"⭳"</span>"Downloads"</A>
```

- [ ] **Step 2: keep six items readable at 375px.** In styles.css, inside the `@media (max-width: 40rem)` block that styles `.tabbar` (around line 185), reduce per-item font so six fit. Add within that block:

```css
  .tabbar a { font-size: 0.62rem; }
  .tabbar .tab-icon { font-size: 1.05rem; }
```

(Only add if the existing rules use a larger size; match the existing selector nesting.)

- [ ] **Step 3: verify + commit.**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown`
Expected: compiles.

```bash
git add crates/yomu-ui/src/lib.rs crates/yomu-web/styles.css
git commit -m "feat(ui): Downloads entry in the phone tab bar"
```

### Task 7: Verify + E2E + screenshots

**Files:**
- Create: `<scratchpad>/vscroll/unified-dl-e2e.js`

- [ ] **Step 1: workspace checks.**

Run: `just check`
Expected: passes (fmt, clippy, wasm).

Run: `cargo test --workspace --exclude yomu-shell`
Expected: all pass.

- [ ] **Step 2: shell-sim E2E.** Write `unified-dl-e2e.js` modeled on `vscroll/ring-e2e.js` (same chromium path, `__TAURI__.core.invoke` fake with per-page `device_save_page` delays, static dist on 4799, real dev server on 4791). Assertions:
  - Long-press a server-only chapter → menu → "Download (local)"; the row gains `dl-active`; navigate to `/downloads`; the "On this device" section shows a row whose label grows (`0/…` → higher) — poll `.download-list .download-progress-label` text.
  - Click the device row's **Cancel** → label reads "Cancelling…", then the row disappears; the fake `invoke` recorded a `device_delete_chapter` call for that chapter id; `localStorage['yomu-device-chapters']` does NOT contain the chapter (no mark written).
  - The server queue still renders from a stubbed `/downloads` (reuse the ring-e2e server-tier stub with `state:{state:'downloading'}` and a `progress`).
  - Live status: start another local save, let it finish (no cancel), and assert the originating chapter row on the manga page gains class `dl-local` without navigation.

Run: `cd <scratchpad> && bun vscroll/unified-dl-e2e.js`
Expected: all PASS.

- [ ] **Step 3: screenshots.** In the E2E, screenshot (a) the Downloads tab with both sections populated (`vscroll/unified-dl.png`), and (b) the phone tab bar at 375px width showing six items (`vscroll/tabbar6.png`). Eyeball spacing; if the sixth item wraps or clips, tighten the font-size in Task 6 Step 2 and rebuild.

- [ ] **Step 4: commit any test-driven CSS tweaks.**

```bash
git add crates/yomu-web/styles.css
git commit -m "style(ui): tab bar spacing for six items"
```

(Skip if no tweak was needed.)
