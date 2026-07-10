# Chapter release dates + sources-tab alignment — design

## Problem

The chapter list annotates each row with its page count ("13 p.",
crates/yomu-ui/src/pages/manga.rs). Page count is only known after a
download or first read, and it isn't what a reader scans the list for —
the chapter's release date is more useful. yomu currently has no release
date anywhere: the selector source parses no date from listings, and
`Chapter` only carries `fetched_at` (when *our* sync first saw the row),
which collapses to one identical timestamp for the whole backlog of a
newly added manga.

Secondary nit, same release: on the Sources tab each source card is an
independent flex row, so the host/label "columns" start at a different x
on every card (`.source-card`, crates/yomu-web/styles.css). They should
align across cards.

## Feature 1: chapter release dates

### Source layer (yomu-source, selector source)

New **optional** keys in the `[manga]` section of a source definition:

- `chapter_date` — selector relative to `chapter_item`, same `css@attr`
  syntax as the existing selectors (so `time@datetime` works).
- `chapter_date_format` — optional chrono format string (e.g. `"%Y/%m/%d"`)
  for sites printing absolute dates in a local convention.

Parsing tries, in order, on the trimmed text:

1. RFC 3339 (covers `<time datetime>` attributes),
2. `chapter_date_format` when configured — date-only formats resolve to
   midnight UTC,
3. English relative phrases anchored at fetch time: `just now`, and
   `N second(s)/minute(s)/hour(s)/day(s)/week(s)/month(s)/year(s) ago`
   (months = 30 days, years = 365 days — approximate by nature).

Absent selector, no match, or unparseable text → `None`. A missing date
must never fail a sync; unparseable text is debug-logged.

This covers the three deployed source definitions (kept outside the repo,
per policy): one site prints relative phrases, one prints `YYYY/MM/DD`
text, one exposes `<time datetime="...">` RFC 3339.

### Domain & API

`ChapterRef` (yomu-domain/src/source.rs) and `Chapter`
(yomu-domain/src/library.rs) gain

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub published_at: Option<DateTime<Utc>>,
```

Serde-optional on both ends, so old clients, the offline cache and stored
JSON are unaffected.

### DB (yomu-server)

- Migration: `ALTER TABLE chapters ADD COLUMN published_at TEXT` (nullable).
- `sync_chapters` writes `published_at` on insert **and** on the
  existing-row update path, whenever the listing provides `Some(..)` —
  so the whole backlog backfills itself on the first sync after the
  source definitions gain the selector, and later site-side edits win.
  A listing `None` never clears a stored date (a site dropping its date
  column shouldn't erase data).
- The re-upload twin-merge logic is untouched: the surviving live row
  already carries the listing's date.

### UI (yomu-ui)

The `"13 p."` span in the chapter row (pages/manga.rs) is replaced by the
date, `class="muted chapter-date"`:

- younger than 7 days → relative: `"42 min. ago"`, `"5 h. ago"`,
  `"3 d. ago"` (floor; `"just now"` under a minute),
- otherwise short absolute: `"May 19"`, with the year (`"May 19, 2025"`)
  when it isn't the current year,
- `published_at == None` → no annotation at all.

Page count disappears from the row; download state still marks what's
fetched. Formatting is a pure helper (`format_date(published_at, now)`)
so it's unit-testable without a DOM.

## Feature 2: sources-tab column alignment

CSS-only, crates/yomu-web/styles.css:

- `.source-list` becomes `display: grid` with
  `grid-template-columns: max-content max-content 1fr max-content`,
- `.source-card` becomes `display: grid; grid-template-columns: subgrid;
  grid-column: 1 / -1;` keeping its existing padding/border/gap,
- the card's four children (name, host, `.grow` spacer, sorts) map onto
  the four tracks; no Rust change.

`subgrid` is supported by every engine the app ships on (WebKitGTK,
Android WebView/Chromium, Firefox). Worst case on an ancient engine the
cards fall back to per-card grid — same as today's misalignment, not
worse.

## Testing

- Date parser unit tests: RFC 3339, `%Y/%m/%d` via config, each relative
  phrase family, garbage → `None`.
- Selector fixture test: a chapter list fragment with a date node,
  asserting `ChapterRef::published_at`.
- Sync test: re-sync with dates on a manga whose rows have NULL
  `published_at` backfills them; a later `None` listing doesn't clear.
- UI formatting helper tests: relative/absolute cutover, year display.
- Visual check of the sources tab via the web build.

## Rollout

This touches yomu-ui, which is compiled into every shell — unlike 1.3.1
the release needs web + desktop + **Android APK** builds. After deploy,
the staged source definitions gain their `chapter_date` lines and are
copied to the live `sources.d/` (user sudo step); dates appear after the
next sync of each manga.
