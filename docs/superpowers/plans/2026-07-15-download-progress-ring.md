# Download Progress Ring Implementation Plan

> Spec: docs/superpowers/specs/2026-07-15-download-progress-ring-design.md

**Goal:** perimeter-tracing progress border on chapter rows for both
download tiers; per-page local saves.

### Task 1: shell per-page commands (crates/yomu-shell/src/lib.rs)

- [ ] `device_begin_chapter(chapter)`: reset `.partial-<id>`.
- [ ] `device_save_page(base, chapter, page)`: download one page into it
      (extension by content-type, `{page:04}.{ext}` name).
- [ ] `device_finish_chapter(chapter)`: rename `.partial-<id>` → `<id>`.
- [ ] Remove `device_save_chapter`; register the new commands.

### Task 2: per-page save loop (crates/yomu-ui/src/offline.rs)

- [ ] `shell_begin_chapter`, `shell_save_page`, `shell_finish_chapter`
      invoke wrappers; drop `shell_save_chapter`.
- [ ] `save_chapter_with_progress(client, chapter_id, on_page)`:
      fetch page count, then per page shell command or web `fetch_page`
      (service-worker path keeps the `service_worker_active` guard),
      calling `on_page(done, total)`; shell path finishes with the
      rename. Replaces `prefetch_chapter`.

### Task 3: progress store + wiring (crates/yomu-ui/src/pages/manga.rs)

- [ ] `RowProgress { done, total, tier, failed }`, store signal
      `RwSignal<HashMap<Uuid, RowProgress>>` created in `MangaPage`,
      passed through `MangaDetail` → `ChapterList` → rows.
- [ ] `save_locally` gains the store: seeds `{0,total}` on start, updates
      per page, removes on success (+ mark), on failure sets `failed`,
      removes after 1.5 s, writes `status`.
- [ ] Downloads poll: effect + 2 s interval while any listed chapter is
      Pending/Downloading and conn is Online; maps the active
      `DownloadProgress` into the store (tier Server), clears server
      entries otherwise; bumps `refresh` when a previously-busy chapter
      turns Downloaded.

### Task 4: row visual (manga.rs + styles.css)

- [ ] Row: `class:dl-active` when an entry exists; SVG `.dl-ring`
      overlay with `rect pathLength=100`, `stroke-dasharray: {p} 100`,
      tier class (`ring-local`/`ring-server`), `ring-failed` on red.
- [ ] CSS: `.chapter-item { position: relative }`; `.dl-ring` inset
      overlay, `stroke-width ~2px`, dash transition 0.3s, colors by
      class; `dl-busy` keeps the pulse as the remainder cue.

### Task 5: verification

- [ ] `just check`, `cargo test --workspace --exclude yomu-shell`.
- [ ] Headless shell-sim E2E: staged per-page delays → dash grows,
      settles to `dl-local`; failure stub → red flash + status; stubbed
      `/downloads` sequence → server ring + auto flip to blue.
      Screenshot mid-progress.
