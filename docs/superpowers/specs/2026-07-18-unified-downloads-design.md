# Unified Downloads Tab Design

Date: 2026-07-18

## Problem

Local (device) downloads and server downloads live in different places.
Server downloads have a proper home — the Downloads tab polls the
server's `/downloads` queue and shows Downloading / Pending / Failed with
retry and dismiss. Local saves only draw progress rings on the manga
page, tracked in a signal owned by that page: leave the page and the
in-flight state is gone, and the Downloads tab never shows them. On the
phone the Downloads tab is reachable only through the More page.

Wanted: in-flight local saves appear in the Downloads tab in their own
section (mirroring the server queue), are cancelable there, and the tab
is one tap away on the phone.

## Decisions (user)

- The device section shows which chapters are **downloading to the
  device** (in-flight local saves), as its own section next to the
  server queue — not a catalog of already-saved chapters.
- Local saves are **cancelable** from the Downloads tab.
- The Downloads tab gets a **sixth entry in the phone tab bar**.
- A chapter's row in the chapter list updates to its **on-device**
  style the moment a local save completes — no navigating away and back.

## Design

### 1. Shared local-download store

Local progress is currently a `ProgressMap`
(`RwSignal<HashMap<Uuid, RowProgress>>`) created inside `MangaPage`, so
nothing outside that page can see it. Lift the **local** tier into an
app-level context, provided in `App` (same pattern as `Connectivity`):

```rust
struct LocalDownload { done: u32, total: u32, failed: bool, cancel_requested: bool }
type LocalDownloads = RwSignal<HashMap<Uuid, LocalDownload>>;
```

- Provided once in `App`; read via a `use_local_downloads()` helper.
- The manga page's rings read the local tier from this store instead of
  a private signal (server-tier ring progress stays page-local, polled
  from `/downloads` as today — it is not moved).
- The Downloads tab reads the same store, so an in-flight local save
  shows there regardless of which page started it and survives
  navigation away from the manga page.

The manga page keeps a small page-local map for the **server** tier
(what it polls to draw blue rings); only the local tier is lifted.

### 2. Cancelable save loop

`save_chapter_with_progress(client, chapter_id, on_page)` gains a
cancel check:

```rust
pub enum SaveOutcome { Done(u32), Cancelled }

pub async fn save_chapter_with_progress(
    client, chapter_id,
    on_page: impl Fn(u32, u32),
    should_cancel: impl Fn() -> bool,
) -> Result<SaveOutcome, String>
```

- Checked once before `device_begin_chapter` and after each page.
- On cancel: stop the loop, and on the shell path call
  `device_delete_chapter(chapter_id)` to drop the `.partial-<id>`
  directory; return `Ok(SaveOutcome::Cancelled)`. The web/SW path stops
  the same way — orphaned cache entries are harmless because "on
  device" is driven by the device mark, which is only written on a
  completed save.
- No device mark is written on cancel, so the chapter is simply not
  saved.

Callers (`save_locally` on the manga page) pass
`|| store.with(|m| m.get(&id).is_some_and(|d| d.cancel_requested))`.
On `Done` they write the mark and remove the store entry (as today); on
`Cancelled` they remove the entry without a mark; on `Err` they set
`failed` and schedule removal (as today).

### 3. Reactive device marks (live row status)

Today a chapter row seeds `on_device` from a one-time
`offline::device_chapters()` read at mount, and `mark_device_chapter`
writes localStorage without notifying any row — so a freshly saved
chapter only flips to its on-device style after leaving the list and
coming back.

Introduce an app-level reactive mirror of the device marks:

```rust
type DeviceMarks = RwSignal<BTreeMap<Uuid, DeviceMark>>;
```

- Provided in `App`, seeded from localStorage once; read via
  `use_device_marks()`.
- `mark_device_chapter` and `unmark_device_chapter` write localStorage
  **and** update this signal (single write-through path). Existing
  callers are unchanged; the functions gain the signal update.
- Chapter rows derive `on_device` reactively:
  `move || marks.with(|m| m.contains_key(&id))`, replacing the seeded
  `RwSignal`. When a local save completes and writes its mark, every
  affected row flips to `dl-local` / `dl-both` immediately; "remove
  from device" flips them back live too.

Because both the local-download store (§1) and these marks live at
app level, the completion path is: loop returns `Done` → caller writes
the mark (updates `DeviceMarks`) → removes the local-download entry
(clears the ring). One update, both the ring and the row react.

### 4. Downloads tab layout

Storage tiles (server chapters, device chapters) stay at the top as the
overview. Below, two labeled sections:

- **Server** — the existing queue: Downloading / Pending / Failed with
  the current retry/dismiss actions. Unchanged.
- **On this device** — one row per in-flight local save (manga ·
  chapter, page `x/y` progress bar), driven by the shared store. Each
  row has a **Cancel** button that sets `cancel_requested`; the row
  reads "Cancelling…" until the loop exits and removes the entry. When
  no local save is running, this section shows the resting line
  ("N chapters on this device").

Each section owns its empty state; there is no single global "nothing
here" line.

Rows link to `/manga/{id}` like the server rows. The manga-page rings
react to the same store, so a cancel here also clears the ring there.

### 5. Phone tab-bar access

Add a **Downloads** entry to the phone tab bar (`.tabbar` in
lib.rs/styles.css), alongside Home / Library / Sources / Search / More —
six items within the 40rem breakpoint. The More-page "Downloads →" link
stays for desktop parity. Icon spacing at six items is verified with a
phone-width screenshot.

### 6. Error handling

- Cancel while a page request is in flight: that page resolves (or
  errors) and the next loop check exits; partial dir is cleaned on the
  shell path.
- A failed local save keeps its existing behavior (red ring on the
  manga page, `failed` flag, removed after ~1.5 s) and, in the device
  section, shows the error text before it clears.
- Store entries are keyed by chapter id, so the manga page and the tab
  never double-render the same save.

### 7. Testing

- Headless shell-sim: start a local save on the manga page (per-page
  invoke delays), navigate to Downloads, assert the device section
  shows the row with growing `done/total`; the server queue still
  renders from a stubbed `/downloads`.
- Cancel: click the device row's Cancel → row shows "Cancelling…", then
  disappears; `device_delete_chapter` was invoked; no device mark
  written.
- Live row status: on the manga page, run a local save to completion
  without navigating; assert the chapter row gains the `dl-local` class
  (and loses `unavailable` when offline) as soon as the mark is written.
- Screenshot the two-section layout and the six-item phone tab bar
  (375 px wide) to confirm spacing.
- `just check`, `cargo test --workspace --exclude yomu-shell`.
