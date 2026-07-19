# Home & More Cleanup Design

Date: 2026-07-19

## Problem

Two small UI cleanups:

1. The More tab still lists a "Downloads →" link, now redundant —
   Downloads is a first-class entry in the desktop top bar and the phone
   tab bar (since 1.13.0).
2. The Home "Continue reading" row shows every library entry that has a
   saved reading position, including titles the reader has fully caught
   up on. A finished title has nothing to continue and shouldn't sit
   there.

## Decisions (user)

- "Finished" = **`unread_count == 0`** (everything currently available
  has been read), not "position on the last page" or the "Finished"
  category. A title reappears in the row when a new chapter arrives
  (unread goes back above 0).

## Design

### 1. Remove Downloads from More

Delete the `<a href="/downloads">"Downloads →"</a>` list item in
`crates/yomu-ui/src/pages/more.rs`. The desktop top-bar link and the
phone tab-bar link are untouched, so Downloads stays reachable
everywhere it already is.

### 2. Filter finished titles from "Continue reading"

In `crates/yomu-ui/src/pages/home.rs`, the resume shelf builds from:

```rust
list.iter().filter(|e| e.position.is_some())
```

Add the unread guard:

```rust
list.iter().filter(|e| e.position.is_some() && e.unread_count > 0)
```

`unread_count` is already present on `MangaWithPosition` (the "New
chapters" shelf uses it), so no new data or query. Sorting, truncation
(12), and card rendering are unchanged.

### 3. Error handling

No new failure modes: both are pure view-filter/markup changes over data
already loaded for the page.

### 4. Testing

Headless (bun + chromium against the dev server, existing harness):

- Seed/library state with one in-progress title (`unread_count > 0`,
  has a saved position) and one finished title (`unread_count == 0`,
  has a saved position). Load Home; assert the "Continue reading" row
  contains the in-progress title and not the finished one.
- Load the More page; assert there is no `a[href="/downloads"]`.

If the fixture library lacks a clean finished-with-position title, drive
the state by marking a title's chapters read via the API before loading
Home.
