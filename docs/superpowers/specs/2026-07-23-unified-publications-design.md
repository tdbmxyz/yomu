# Unified Publications (2.0 slice 1: Foundation + Comics) — Design

**Date:** 2026-07-23
**Status:** Approved for planning

## Goal

Generalize yomu's library into a unified `Publication` model (inspired by
Readium's publication/locator architecture), add a server-side *streamer*
that turns user-supplied comic files into library entries, and add a
subtle per-kind library switcher. Proven end-to-end with **comics from
files**, read through the existing image reader.

This is the first slice of the 2.0 roadmap. It lays the model that later
slices (EPUB/novels reader, PDF, upload-from-device) plug into, while
shipping standalone value: drop a CBZ or an image folder into the watched
directory and it appears in the library, readable, with synced progress.

## Roadmap context (later slices, NOT in scope here)

1. **This slice** — unified model + streamer + comics-from-files.
2. EPUB/novels: reflowable reader (webview navigator, pagination, e-ink
   friendly themes), full Web Publication Manifest endpoint,
   `progression`-based locators.
3. PDF rendering.
4. Upload-from-device; possibly online book sources.

Primary usage: comics/manhwa/manga on phone; novels + manga on an e-ink
tablet.

## Non-goals (explicit, so slice 3 doesn't get half-built)

- No serialized Readium Web Publication Manifest JSON endpoint. For
  comics the reading order *is* the units, already modeled; the manifest
  earns its place when a reflowable navigator consumes it.
- No `progression`/`fragment` locator storage in the DB. The domain enum
  is shaped for it; no columns or sync changes yet.
- No EPUB or PDF parsing/rendering. Such files in the watched folder are
  ignored with a log line (see Streamer).
- No CBR (RAR) support: proprietary format needing bundled unrar
  bindings. CBR files are ignored-with-log; converting CBR→CBZ is
  lossless if needed. May join a later slice.
- No upload endpoint or in-app file picker.
- No wire-format v2. The HTTP surface keeps 1.x names (see
  Compatibility).

## Domain model (yomu-domain)

Full rename — the unified intent is expressed in the names. Existing
concepts generalize; no speculative fields.

```rust
pub struct Publication {            // was Manga
    pub id: Uuid,
    pub kind: Kind,                 // drives the library switcher
    pub origin: Origin,             // where content comes from
    pub title: String,
    pub description: Option<String>,
    pub cover_url: Option<Url>,
    pub auto_download: bool,        // Source-origin only; hidden for LocalFile
    pub category: String,
    pub genres: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub missing_since: Option<DateTime<Utc>>, // LocalFile whose file vanished
}

pub enum Kind { Comics, Novels, Pdf }        // Novels/Pdf reserved, unused this slice

pub enum Origin {
    Source { source_id: String, source_key: String }, // scraped
    LocalFile { path: String },                       // streamer-managed, relative to books dir
}

pub struct ReadingUnit {            // was Chapter — same fields, kind-agnostic name
    pub id: Uuid,
    pub publication_id: Uuid,
    pub source_key: String,         // unit key within its origin
    pub title: String,
    pub number: Option<f64>,
    pub source_order: u32,
    pub scanlator: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub download: DownloadState,    // meaningful for Source origin; LocalFile is inherently present
    pub page_count: Option<u32>,
    pub read: bool,
}

pub struct Locator {                // was Position
    pub unit_id: Uuid,
    pub locations: Locations,
    pub at: DateTime<Utc>,
}

pub enum Locations {
    Page { page: u32 },             // image-based kinds (all of this slice)
    // Progression { progression: f64, fragment: Option<String> } — arrives with EPUB
}
```

- Scraped manga become `kind = Comics`, `origin = Source` — they group
  under Comics exactly as today.
- `ReadingUnit` keeps every `Chapter` field: downloads, updates,
  read-marks, device-saved chapters (unit ids are stable), and the
  reader keep working.
- The progress journal (`ProgressEvent`) and `merge_position` logic are
  untouched apart from renames; sync semantics are identical.

## Streamer (yomu-server)

New server module owning all local-file parsing. Absorbs
`yomu-source/src/local.rs` logic (CBZ via zip + image directories,
`spawn_blocking`, path-traversal guards, chapter/page ordering regexes);
`LocalSource` is removed from the source registry and the sources UI.

**Watched folder.** Config key `books_dir`; defaults to the existing
local-source directory so nothing moves on disk for current deployments.
Scanned on server startup, on a periodic interval (config, default 1h),
and on demand via `POST /api/v1/library/rescan` (surfaced as a "Rescan
files" action in More, next to Backup).

**Folder conventions** (carried over from LocalSource):

- `<books_dir>/<Series>/<chapter dir or chapter.cbz>` → one Publication
  with one ReadingUnit per chapter entry.
- `<books_dir>/<name>.cbz` or a loose image directory at the root → a
  single-unit Publication.
- Cover: `cover.<ext>` in the series dir if present, else the first page
  of the first unit.
- Image extensions: jpg/jpeg/png/webp/gif/avif. Anything else
  (.epub/.pdf/.cbr/…) is skipped with one `tracing::info` line per file
  per scan — the folder will legitimately hold future-format files.

**Scan is an upsert, never destructive:**

- New series/file → Publication inserted with `origin = LocalFile`,
  `kind = Comics`, deduped by path.
- New units inside a known publication → units inserted **and recorded
  in the updates feed** (same table the scraper updater writes), so they
  appear on the Updates page and trigger ntfy like new scraped chapters.
- Vanished file/dir → publication kept, `missing_since` set. Progress
  survives. Flag clears if the path reappears.
- **Rename self-heal:** before inserting a new LocalFile publication,
  if exactly one *missing* LocalFile publication has the same title, its
  path is re-pointed instead (units re-keyed by matching titles/numbers,
  unmatched ones added). Ambiguous title matches never guess — a new
  publication is created and the missing one stays flagged.

**Serving pages.** Comics keep the existing
`GET /chapters/{id}/pages` + `/pages/{n}` endpoints. For LocalFile
units the handler resolves images from disk through the streamer instead
of proxying a scraper source; response shape identical. Device-download
(Tauri offline save) works unchanged since it consumes those endpoints.

**Covers.** `GET /manga/{id}/cover` currently falls back to
`source.image(cover_url)` through the source registry when the cover
isn't cached. For LocalFile publications that fallback resolves through
the streamer (cover file or first page from disk) instead — the
registry no longer knows "local".

**Per-publication refresh.** `POST /manga/{id}/refresh` (the Refresh
button on the publication page) calls `refresh_manga` through the
source registry today. For LocalFile origin it instead runs a targeted
streamer rescan of that publication's path and returns the new-unit
count in the same response shape. The button stays visible.

**Updater.** `list_manga_for_update` (renamed accordingly) excludes
LocalFile origins — the rescan is their updater. `auto_download` and
download states are not offered for LocalFile publications in the UI.

**Catalog cache.** Migration 0011 deletes catalog-cache rows with
`source_id='local'`; the cache only serves scraper search/browse.

## Database (migration 0011)

One migration, additive in data, renaming in schema:

- Rename tables: `manga → publications`, `chapters → reading_units`.
- Rename FK columns where they leak the old name
  (`progress_events.manga_id → publication_id`,
  `progress_events.chapter_id → unit_id`, `read_chapters.chapter_id →
  unit_id`, `manga_genres → publication_genres`, `updates` columns).
- Add to `publications`: `kind TEXT NOT NULL DEFAULT 'comics'`,
  `file_path TEXT NULL`, `missing_since TEXT NULL`. Keep
  `source_id`/`source_key` as nullable columns. CHECK: exactly one of
  (`source_id` AND `source_key`) / `file_path` is set. UNIQUE
  (`source_id`,`source_key`) kept; UNIQUE(`file_path`) added.
- Backfill: every existing row → `kind='comics'`, Source origin
  (columns already populated).
- **Existing local-source rows:** rows with `source_id='local'` are
  converted to `origin = LocalFile` — their `local:` scheme source_keys
  resolve to paths (`?entry=`-style cbz keys map to the archive path).
  Progress, read-marks, and categories survive via unchanged ids.

Progress journal data is untouched; UUIDs never change.

## UI (yomu-ui)

- **Library kind switcher:** the page title becomes a dropdown —
  `Comics ▾`. Tapping it lists kinds; selecting filters the library.
  Kinds with zero publications are hidden (Comics always shown). The
  selected kind is cached per device (localStorage) and restored on
  relaunch, so the phone reopens straight into Comics and the e-ink
  tablet (later) into Novels. No segmented bar, no extra vertical space.
  Category tabs, title search, and genre chips are kind-agnostic and
  compose with the kind filter — they filter within the selected kind.
- **Home:** Continue-reading and Updates remain cross-kind (they are
  activity feeds, not library views).
- **Sources page:** scrapers only; the local source entry disappears
  (files now arrive in the library directly).
- **Publication page:** for LocalFile origin, hide auto-download and
  per-unit download actions; show a "file missing" badge (dimmed cover)
  when `missing_since` is set. Library cards for missing publications
  are dimmed likewise.
- **More:** add "Rescan files" next to Backup.
- **Copy pass:** user-visible strings saying "manga" become neutral or
  kind-aware where now wrong (empty states, add-to-library surfaces,
  about text).
- **Reader:** unchanged.

## Compatibility (frozen wire, renamed internals)

The full rename applies to Rust types, modules, and DB schema. The HTTP
surface is **frozen at 1.x names** so deployed APKs keep working:

- Route paths unchanged: `/api/v1/manga/{id}`, `/chapters/{id}/pages`,
  `/chapters/download`, etc. UI URLs unchanged (`/manga/:id`,
  `/read/:manga/:chapter`).
- JSON field names unchanged via explicit `#[serde(rename)]`:
  `manga_id`, `chapter_id`, `chapters`, … on both serialization and
  deserialization. New fields (`kind`, `origin`, `missing_since`) are
  additive; old clients ignore them.
- `source_id`/`source_key` are **required** fields in the 1.x `Manga`
  JSON, so LocalFile publications serialize them as `source_id:
  "local"` and `source_key: <1.x-style local: key>` on the frozen wire
  (exactly what converted rows carried before migration). Old clients
  keep deserializing and rendering them; new clients read `origin`.
- Progress sync payloads byte-compatible with 1.x clients.

**Backup/restore:**

- New backups serialize the new shape (kind/origin/missing_since
  included).
- `POST /restore` accepts both: a 1.x backup restores by applying the
  same mapping as migration 0011 (backfill Comics/Source, convert
  `local` rows).

## Error handling

- Streamer scan errors (unreadable file, corrupt zip) skip the entry
  with a `tracing::warn` and continue the scan; one bad file never
  aborts a rescan or fails startup.
- Page requests for a missing publication return 404 with a clear
  message.
- Rescan endpoint returns a summary `{ added, updated, missing }`.

## Testing

- **Migration test:** seed a 1.x-shaped DB (scraped manga + progress +
  read marks + a `local`-source row), run 0011, assert: publications
  have `kind='comics'`/correct origin, local rows converted with paths,
  progress and read marks intact, ids unchanged.
- **Streamer tests:** fixture tree with a multi-chapter series (dirs +
  cbz), a root-level cbz, a cover file, an unsupported `.epub`, a
  corrupt zip. Assert publications/units/page counts, skip behavior,
  update-feed rows for newly appeared units, `missing_since` set/cleared,
  rename self-heal (unique title re-points; ambiguous does not).
- **Wire-compat tests:** golden 1.x JSON payloads (manga detail,
  progress push, backup) deserialize; responses serialize with 1.x
  field names.
- **Restore test:** a 1.x backup file restores into the new schema.
- Existing reader/downloader/updater test suites pass post-rename.

## Rollout

- Server migration is automatic on first 2.0 start; recommend a backup
  first (standing practice on zeus).
- Old clients (1.x APKs) keep working against the 2.0 server thanks to
  the frozen wire.
- `books_dir` defaults to the current local dir; zeus needs no config
  change.
