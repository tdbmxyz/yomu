# Architecture

yomu follows the chaos template (domain = wire contract, all HTTP through the
client crate, UI platform-agnostic behind `AppConfig`). This file covers what
is specific to yomu.

## Sources (no extension system)

```
Source (trait): search / manga / pages / image
   └── SelectorSource — driven by a TOML spec (CSS selectors)
   └── (future) native impls for API-based sites
```

Files on the server's own disk are not a source: the server's streamer
scans them straight into the library (next section).

- A scan site = one TOML file in `sources_dir` (`selector mini-syntax:
  `css selector[@attr]`, `{base}`/`{query}` templates, per-source rate limit
  and optional Referer). Most scan sites (Madara-style layouts) fit.
- Broken definitions are skipped individually and reported loudly at startup;
  one definition written for a newer engine must not crashloop every healthy
  source. Duplicate ids and invalid URL templates are rejected and included in
  that broken-definition report.
- Keys (`manga.source_key`, `chapter.source_key`) are the source's own page
  URLs, opaque to everything else, and validated to stay on the source's
  origin (scheme + host + port — keys are client input).
- Parsing is pure (`parse_search`/`parse_manga`/`parse_pages`), unit-tested
  against fixture HTML; fetching adds throttling on top.

### Streamer (local books dir)

Files already on the server's disk are library entries, not a source
(they were one in 1.x — the built-in "local" source; migration 0011
renames manga → publications / chapters → reading_units, splits the
origin into source columns vs `file_path`, and converts the old local
rows in place, ids untouched). The streamer watches the dir configured
under `books.dir` (`[books]`; the legacy `[local]` section name still
works) and scans it on startup, on an interval, and on demand via
`POST /api/v1/library/rescan`:

```
books/
  Solo Farming in the Tower/    (series → one Publication, units inside)
    cover.jpg                   (optional; else first page of first unit)
    details.json                (optional {"title", "description"})
    Chapter 1/  001.png …       (directory of images)
    Chapter 2.cbz               (zip archive of images)
  One Shot.cbz                  (root-level archive or loose image dir
  Loose Pages/  001.png …        → single-unit publication; cover is the
                                 first page)
```

A scan upserts publications and their reading units, feeds the updates
feed (and ntfy) for new units in known publications, flags vanished
files with `missing_since` instead of deleting anything, and self-heals
unambiguous renames (a new path whose title matches exactly one missing
publication re-points that row, so ids and progress survive). Keys are
dir-relative paths, validated against escaping the books dir; page and
cover URLs use the 1.x-compatible `local:` scheme only the streamer
resolves.

## Reading paths

One endpoint serves both modes, so clients don't care:

```
GET /api/v1/units/{id}/pages/{n}
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

The browser PWA and native shells already keep reading while disconnected. On
reconnect the client POSTs its local journal (`/progress/events`, idempotent by
event id; events for deleted manga are skipped, not errors) and pulls the
server's tail (`?since=<seq cursor>` — server arrival order, because event ids
are device-stamped and a late offline push would slip behind an id cursor).
Merge is associative and commutative — no conflict resolution UI, no clock
negotiation beyond last-write-wins at page granularity, which matches the
product decision (track chapter + page, nothing finer).

## Updater & categories

One periodic task (`updater.interval_secs`, default 6h) re-syncs library
manga: new chapters are inserted (existing ids stable), manga with
`auto_download` get them queued. The same `sync::refresh_manga` powers the
manual "check now" endpoint, so behavior can't drift.

Every manga belongs to one **category** (`categories` table, seeded
Reading / Paused / Finished; manga default to `reading`). Each category has
an `update_enabled` flag and the periodic sweep only checks manga in
enabled categories — paused/finished series stop hammering their sources.
Manual per-manga refresh always works regardless of category.
`GET/PUT /api/v1/categories`, `UpdateMangaRequest.category` to move manga;
the library UI filters by category and exposes the per-category toggle.

## Auth (ADR-0003)

Two modes, chosen by config. `[auth]` with an OIDC issuer (authentik):
sign-in via authorization-code + PKCE, claims from the userinfo endpoint,
users upserted by `sub`, sessions à la chaos (opaque token, sha256 at rest,
HttpOnly cookie or bearer). No `[auth]`: single-account mode — every request
is the seeded shared "Everyone" user, no login UI. Reading progress and read
marks are per-user; library, downloads and categories stay server-wide. In
OIDC mode content APIs default to requiring identity. When a former
single-account installation has exactly one OIDC/proxy account, that account
receives a one-time transfer of the shared progress journal and read marks;
the old shared state is cleared so there is one authoritative owner. This
keeps an authentication upgrade from making the existing history disappear on
new devices. Only liveness/readiness, metrics, and the sign-in surface are
public; image routes may instead carry a short-lived media token.

## Client persistence

The browser adapter deliberately uses Web Storage plus the Service Worker
Cache API: those are the platform's native offline stores. Native shells save
chapter pages under the app data directory and are evolving metadata, journal,
and sync state toward the `yomu-store` SQLite crate. Both adapters use the same
domain journal and HTTP synchronization rules; durable-store work does not
replace working PWA offline support.
