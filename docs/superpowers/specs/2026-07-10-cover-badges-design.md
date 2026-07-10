# Library cover count badges — design

## Problem

A library card shows only the unread count. The reader can't see at a
glance how big a series is, how much of it the server has downloaded,
or how much is stored on this device.

## Approach

Keep the unread badge exactly as it is (top-right, accent). Add a slim
semi-transparent strip along the cover's bottom edge with up to three
numbers, each hidden when zero, the whole strip hidden when all are:

```
┌────────────┐
│        [12]│   unread (unchanged)
│   cover    │
│▓▓▓▓▓▓▓▓▓▓▓▓│
│ 148 ⇩45 ⇣12│   total · server-downloaded · device-downloaded
└────────────┘
```

Glyphs match the chapter-row buttons (`⇩` server tier, `⇣` device tier
— reuse whatever glyphs the chapter list uses today for those two
states; total has none).

## Data

- **total**: `chapter_count`, already in the library list response.
- **server-downloaded**: new `downloaded_count` on the library list
  entry — one `GROUP BY manga_id` over
  `chapters WHERE download_state = 'downloaded'`, joined into the
  existing library query. Serde-defaulted so cached/old payloads parse.
- **device-downloaded**: client-side, from the existing localStorage
  device marks (`offline::device_chapters()` already records the owning
  manga per mark) — counted per manga at render.

## UI

- library.rs cards (grid) get the strip; the Home shelves keep their
  current compact look (unread badge only) — shelf cards are small and
  already crowded.
- New CSS: `.count-strip` over the cover bottom (absolute, gradient
  scrim, 0.7rem font), `.count-strip span` per number.

## Testing

- db test: `downloaded_count` aggregates only 'downloaded' rows and is
  0 (not absent) for manga without downloads.
- UI formatting: none needed beyond compile (pure markup); visual pass
  in the E2E run, including the all-zero (no strip) case.

## Rollout

Server + web/APK in the next release; old clients ignore the new field.
