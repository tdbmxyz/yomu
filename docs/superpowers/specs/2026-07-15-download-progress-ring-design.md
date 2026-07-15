# Download Progress Ring Design

Date: 2026-07-15

## Problem

A running server download pulses the chapter row's border (nice), but a
local (device) save shows nothing until the row flips green at the end —
and neither tier shows how far along it is. Wanted: the same animated
treatment for local saves, plus a border that traces the cell's perimeter
proportionally to the % of pages downloaded, for both tiers.

## Decisions (user)

- Progress drawn as a growing perimeter trace (exact, not a conic sweep):
  starts top-left, clockwise, green for local saves, blue (`--server`)
  for server downloads; the uncovered remainder keeps the soft pulse.
- Server downloads get the % in this release too. (No server change
  needed: `GET /downloads` already carries `DownloadProgress{page,total}`
  for the active chapter.)
- A failed local save flashes the ring red (~1.5 s), the row returns to
  its previous state, and the error lands in the page status line.

## Design

### 1. Progress store (manga page)

`RwSignal<HashMap<Uuid, RowProgress>>` where
`RowProgress { done: u32, total: u32, tier: Tier (Server|Local), failed: bool }`.
Owned by the manga page, passed to `ChapterList` → each row.

### 2. Local saves report per page

`save_locally(client, manga_id, id, on_page: impl Fn(u32, u32))` loops
pages itself and calls back after each:

- Shell: three commands replace `device_save_chapter` —
  `device_begin_chapter(chapter)` (reset the `.partial-<id>` dir),
  `device_save_page(base, chapter, page)` (download one page into it),
  `device_finish_chapter(chapter)` (atomic rename). Same
  completeness guarantee as today; the UI and shell ship together, so
  the old command is removed rather than kept alongside.
- Web: the existing per-page `fetch_page` loop (service-worker cache),
  inlined here; `offline::prefetch_chapter` folds into this path.

Callers (row pull, selection-menu "download locally", the pull queue)
update the store from the callback; completion removes the entry and
marks the device chapter; an error sets `failed`, schedules the entry's
removal after ~1.5 s, and writes the page status line.

### 3. Server download % by polling

While any listed chapter is `Pending`/`Downloading` and connectivity is
Online, the manga page polls `GET /downloads` every 2 s:

- the active entry's `DownloadProgress` maps into the store (tier
  Server) for its chapter;
- entries that leave the queue as `Downloaded` bump the chapter-list
  `refresh` signal, so rows flip blue on their own (today they need a
  manual refresh);
- polling stops when no listed chapter is busy or the app goes offline.

### 4. Row visual

When the store has an entry for the row, the row (position: relative)
overlays:

```html
<svg class="dl-ring" aria-hidden="true">
  <rect pathLength="100" rx=".." stroke-dasharray="P 100"/>
</svg>
```

- `P = done / total * 100`; the rect insets by half the stroke width and
  matches the row's border radius; SVG rect strokes start after the
  top-left corner and run clockwise.
- Stroke color by tier (`--saved` green / `--server` blue); `failed`
  turns it `--down` red. A `stroke-dasharray` transition smooths steps.
- The row keeps a pulsing faint border underneath (existing `dl-pulse`
  rhythm) as the "remainder" cue; `dl-busy`'s current styling folds into
  this (a server download without a store entry yet still pulses).
- The ring wins visually over the static state border; selection still
  wins over everything (declared after, as today).

### 5. Error handling

- Local save: red flash + status line (above); the partial `.partial`
  dir is reset by the next attempt's `device_begin_chapter`.
- Poll failures while online are ignored (next tick retries); going
  offline stops the poll and local pulls fail fast with the request
  deadline.

### 6. Testing

- Headless Blink shell-sim (fake `__TAURI__.invoke` with per-page
  delays): the row's dash value grows monotonically during a save,
  settles into `dl-local` at 100%, screenshots at mid-progress; a failure
  stub shows the red flash + status line.
- Stubbed `/downloads` response sequence drives the server-tier ring the
  same way.
- `just check` + workspace tests; desktop-shell smoke for the new
  commands.
