# Architecture

yomu follows the chaos template (domain = wire contract, all HTTP through the
client crate, UI platform-agnostic behind `AppConfig`). This file covers what
is specific to yomu.

## Sources (no extension system)

```
Source (trait): search / manga / pages / image
   └── SelectorSource — driven by a TOML spec (CSS selectors)
   └── LocalSource — series on the server's own disk (built-in "local")
   └── (future) native impls for API-based sites
```

- A scan site = one TOML file in `sources_dir` (`selector mini-syntax:
  `css selector[@attr]`, `{base}`/`{query}` templates, per-source rate limit
  and optional Referer). Most scan sites (Madara-style layouts) fit.
- Broken definitions fail at startup — a typo must not silently drop a
  source that library entries reference. That includes duplicate source ids
  and search-URL templates that don't substitute into a valid URL.
- Keys (`manga.source_key`, `chapter.source_key`) are the source's own page
  URLs, opaque to everything else, and validated to stay on the source's
  origin (scheme + host + port — keys are client input).
- Parsing is pure (`parse_search`/`parse_manga`/`parse_pages`), unit-tested
  against fixture HTML; fetching adds throttling on top.

### Local source (Suwayomi-style)

Series already on the server's disk, enabled by default under
`local.dir` (config `[local]`):

```
local/
  Solo Farming in the Tower/
    cover.jpg               (optional; else first page of first chapter)
    details.json            (optional {"title", "description"})
    Chapter 1/  001.png …   (directory of images)
    Chapter 2.cbz           (zip archive of images)
```

Keys are directory-relative paths, validated against escaping the local
dir; page URLs use a `local:` scheme only this source resolves. Chapter
numbers are parsed from directory names; "download to server" for local
chapters is pointless but harmless (it copies the files).

## Reading paths

One endpoint serves both modes, so clients don't care:

```
GET /api/v1/chapters/{id}/pages/{n}
    downloaded → file from data_dir/<manga>/<chapter>/000n.ext
                 (directory vanished → falls back to the live path)
    otherwise  → resolved live from the source and proxied (nothing stored;
                 the page-URL list is cached in memory with a 30 min TTL,
                 bounded, and re-resolved once when an image fetch fails —
                 scan sites serve expiring CDN URLs)
```

Downloads are a queue: chapters marked `pending` are picked up by a single
worker (Notify + safety poll, like the chaos archiver), written to a
`.partial` directory (removed on failure) and atomically renamed, so a
chapter directory is always complete. `downloading` rows are re-queued at
startup after a crash; a manga deleted mid-download has its just-written
files discarded when the outcome update matches no row.

## Progress = append-only journal

`progress_events(seq, id UUIDv7, manga_id, chapter_id, page, device, at)` —
never updated, never deleted (except manga cascade). Current position =
event with max `at`, id as tie-break; `yomu_domain::merge_position` is the
single definition of that rule, and the SQL `ORDER BY at DESC, id DESC
LIMIT 1` mirrors it (a db test asserts they agree).

Why: the future offline client (phone) keeps reading while disconnected. On
reconnect it POSTs its journal (`/progress/events`, idempotent by event id;
events for deleted manga are skipped, not errors) and pulls the server's
tail (`?since=<seq cursor>` — server arrival order, because event ids are
device-stamped and a late offline push would slip behind an id cursor).
Merge is associative and commutative — no conflict resolution UI, no clock
negotiation beyond last-write-wins at page granularity, which matches the
product decision (track chapter + page, nothing finer).

## Updater

One periodic task (`updater.interval_secs`, default 6h) re-syncs every
library manga: new chapters are inserted (existing ids stable), manga with
`auto_download` get them queued. The same `sync::refresh_manga` powers the
manual "check now" endpoint, so behavior can't drift.

## Deferred by design

- **Offline client + store crate** (phase 3): journal and API are ready;
  the client needs a local SQLite + download-to-device manager.
- **Auth**: LAN-only posture, like chaos.
- **Webtoon continuous-scroll reader mode**: the reader is paged today;
  vertical mode is a UI change only, tracking stays per page.
