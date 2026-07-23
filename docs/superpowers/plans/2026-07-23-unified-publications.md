# Unified Publications (2.0 slice 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize the library into a `Publication` model with a frozen 1.x wire, add a server-side streamer that turns files in a watched folder into library entries, and add a per-kind library switcher — per `docs/superpowers/specs/2026-07-23-unified-publications-design.md`.

**Architecture:** Full Rust/DB rename (Manga→Publication, Chapter→ReadingUnit, Position→Locator) with the HTTP surface frozen at 1.x names via a serde wire mirror. A new `streamer` module in yomu-server absorbs `yomu-source/src/local.rs` (CBZ + image dirs), scans a watched `books` dir on startup/interval/demand, and upserts `Origin::LocalFile` publications. `LocalSource` leaves the source registry.

**Tech Stack:** Rust workspace (axum + sqlx/SQLite server, Leptos CSR UI), zip crate, figment config.

**Branch:** work on the existing `feat/unified-publications` branch (spec already committed there). All commits unsigned (`git -c commit.gpgsign=false commit …`) and end with:

```
Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
```

**Standing constraint:** never mention real scan-site names anywhere (code, comments, commits). Use "fixture"/"some sites".

**Verification command for every task:** `just check` runs fmt + clippy (`-D warnings`) + wasm checks. Run `cargo test -p <crate>` per task as listed. Note: tasks 1 and 2 leave *other* crates uncompilable; run only the per-crate commands stated in those tasks, and `just check` from task 3 on.

---

## Naming map (single source of truth — later tasks must match exactly)

Domain types (yomu-domain):

| 1.x | 2.0 | wire |
|---|---|---|
| `Manga` | `Publication` | frozen via `PublicationWire` mirror |
| `Chapter` | `ReadingUnit` (field `publication_id`) | field serialized `manga_id` |
| `Position` | `Locator { unit_id, locations, at }` | `{chapter_id, page, at}` |
| — | `Kind` (`Comics`/`Novels`/`Pdf`) | `"comics"`/`"novels"`/`"pdf"` |
| — | `Origin` (`Source`/`LocalFile`) | flattened into `source_id`/`source_key`/`file_path` |
| `ProgressEvent.manga_id/.chapter_id` | `.publication_id`/`.unit_id` | `manga_id`/`chapter_id` |
| `MangaWithPosition` | `PublicationWithLocator` (fields `publication` flattened, `locator`, `unit_count`, …) | field names unchanged (`position`, `chapter_count`, …) |
| `MangaDetailResponse` | `PublicationDetailResponse { publication, units, locator }` | `manga`, `chapters`, `position` |
| `AddMangaRequest` | `AddPublicationRequest` | unchanged |
| `UpdateMangaRequest` | `UpdatePublicationRequest` | unchanged |
| `SetPositionRequest` | `SetLocatorRequest { unit_id, page, device }` | `chapter_id`, … |
| `DownloadChaptersRequest` | `DownloadUnitsRequest { unit_ids }` | `chapter_ids` |
| `MarkChaptersRequest` | `MarkUnitsRequest { unit_ids, read }` | `chapter_ids`, `read` |
| `BulkChaptersResponse` | `BulkUnitsResponse` | `affected` |
| `PagesResponse.chapter_id` | `.unit_id` | `chapter_id` |
| `RefreshResponse.new_chapters` | `.new_units` | `new_chapters` |
| `UpdateEvent.manga_id/.manga_title/.chapter_count` | `.publication_id/.publication_title/.unit_count` | `manga_id`/`manga_title`/`chapter_count` |
| `DownloadQueueEntry.chapter_id/.manga_id/.manga_title/.chapter_title` | `.unit_id/.publication_id/.publication_title/.unit_title` | 1.x names |
| `Backup.manga/.chapters/.read_chapter_ids` | `.publications/.units/.read_unit_ids` | `manga`/`chapters`/`read_chapter_ids` |
| — | `RescanResponse { added, updated, missing }` | new endpoint, new names |

**Kept unchanged** (source-scraping layer, not the library): `MangaSummary`, `MangaDetails`, `ChapterRef`, `SourceInfo`, `BrowseSort`, `SourceSearchResults`, the `Source` trait. Also `DownloadState`, `Category`, auth types.

DB (migration 0011): `manga`→`publications`, `chapters`→`reading_units`, `read_chapters`→`read_units`, `manga_genres`→`publication_genres`; columns `manga_id`→`publication_id`, `chapter_id`→`unit_id` everywhere; `publications` gains `kind`, `file_path`, `missing_since`, nullable `source_id`/`source_key`, origin CHECK.

Db methods: `insert_manga`→`insert_publication`, `get_manga`→`get_publication`, `list_manga`→`list_publications`, `list_manga_for_update`→`list_publications_for_update`, `delete_manga`→`delete_publication`, `genres_by_manga`→`genres_by_publication`, `manga_titles`→`publication_titles`, `sync_chapters`→`sync_units`, `list_chapters`→`list_units`, `get_chapter`→`get_unit`, `export_chapters`→`export_units`, `AppState::chapter_dir`→`unit_dir`, `sync::refresh_manga`→`sync::refresh_publication`, `Notifier::notify_new_chapters`→`notify_new_units`. New: `insert_local_publication`, `list_local_publications`, `repoint_local_publication`, `set_missing_since`, `update_local_metadata`.

Client methods: `add_manga`→`add_publication`, `manga`→`publication`, `update_manga`→`update_publication`, `delete_manga`→`delete_publication`, `refresh_manga`→`refresh_publication`, `chapter_pages`→`unit_pages`, `download_chapter(s)`→`download_unit(s)`, `mark_chapters`→`mark_units`. New: `rescan`. **Route paths inside the client stay byte-identical** (`/manga/{id}`, `/chapters/{id}/pages`, …).

Frozen and untouched: every route path, UI URLs (`/manga/:id`, `/read/:manga/:chapter`), UI page file names (`pages/manga.rs` etc. — they mirror routes), on-disk layout `data_dir/<publication id>/<unit id>/`.

---

### Task 1: Domain model — publications with a frozen wire

**Files:**
- Rename: `crates/yomu-domain/src/library.rs` → `crates/yomu-domain/src/publication.rs`
- Modify: `crates/yomu-domain/src/lib.rs`, `crates/yomu-domain/src/progress.rs`, `crates/yomu-domain/src/api.rs`, `crates/yomu-domain/src/backup.rs`
- Test: inline `#[cfg(test)] mod wire` in `publication.rs` (golden 1.x payloads)

- [ ] **Step 1: Write the failing golden wire tests**

`git mv crates/yomu-domain/src/library.rs crates/yomu-domain/src/publication.rs`, then append this test module (it won't compile yet — that's the failure):

```rust
#[cfg(test)]
mod wire {
    use super::*;

    /// A 1.x client's manga JSON must keep round-tripping unchanged.
    #[test]
    fn scraped_publication_keeps_1x_field_names() {
        let json = r#"{
            "id":"018f4a70-0000-7000-8000-000000000001",
            "source_id":"fixture","source_key":"solo-farming",
            "title":"Solo Farming","auto_download":true,
            "category":"reading","added_at":"2026-01-01T00:00:00Z"
        }"#;
        let p: Publication = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.origin,
            Origin::Source {
                source_id: "fixture".into(),
                source_key: "solo-farming".into()
            }
        );
        assert_eq!(p.kind, Kind::Comics); // additive default
        let out: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(out["source_id"], "fixture");
        assert_eq!(out["source_key"], "solo-farming");
        assert_eq!(out["kind"], "comics");
        assert!(out.get("file_path").is_none());
        assert!(out.get("origin").is_none(), "no nested origin object on the wire");
    }

    /// 1.x rows/backups carried local files as source_id="local"; they must
    /// deserialize into a LocalFile origin, and LocalFile must serialize the
    /// same required fields back so old clients keep parsing responses.
    #[test]
    fn local_publication_wire_compat() {
        let json = r#"{
            "id":"018f4a70-0000-7000-8000-000000000002",
            "source_id":"local","source_key":"Solo Farming",
            "title":"Solo Farming","auto_download":false,
            "category":"reading","added_at":"2026-01-01T00:00:00Z"
        }"#;
        let p: Publication = serde_json::from_str(json).unwrap();
        assert_eq!(p.origin, Origin::LocalFile { path: "Solo Farming".into() });

        let out: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(out["source_id"], "local", "old clients require source_id");
        assert_eq!(out["source_key"], "Solo Farming");
        assert_eq!(out["file_path"], "Solo Farming");
    }

    #[test]
    fn reading_unit_serializes_manga_id() {
        let unit = ReadingUnit {
            id: uuid::Uuid::from_u128(1),
            publication_id: uuid::Uuid::from_u128(2),
            source_key: "c1".into(),
            title: "Chapter 1".into(),
            number: Some(1.0),
            source_order: 0,
            scanlator: None,
            fetched_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            published_at: None,
            download: DownloadState::None,
            page_count: None,
            read: false,
        };
        let out: serde_json::Value = serde_json::to_value(&unit).unwrap();
        assert!(out.get("manga_id").is_some());
        assert!(out.get("publication_id").is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p yomu-domain`
Expected: FAIL to compile (`Publication`, `Origin`, `Kind`, `ReadingUnit` not defined).

- [ ] **Step 3: Rewrite `publication.rs`**

Replace the `Manga` and `Chapter` definitions (keep `Category`, `default_category`, `DownloadState` exactly as they are; update the module doc comment to say "publications" instead of "manga"):

```rust
//! The library: publications the user tracks — scraped series and local
//! files alike — and their reading units as known to the server.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// What a publication is, for the library's kind switcher. Only `Comics`
/// exists in this slice; the others are reserved for later slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Comics,
    Novels,
    Pdf,
}

impl Default for Kind {
    fn default() -> Self {
        Kind::Comics
    }
}

/// Where a publication's content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Scraped from a configured source.
    Source { source_id: String, source_key: String },
    /// Streamer-managed file/dir, path relative to the server's books dir.
    LocalFile { path: String },
}

/// A tracked publication (was `Manga`). The wire shape is frozen at 1.x —
/// see [`PublicationWire`]: `source_id`/`source_key` are always emitted
/// (`"local"` + the path for [`Origin::LocalFile`]), new fields are additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PublicationWire", into = "PublicationWire")]
pub struct Publication {
    pub id: Uuid,
    pub kind: Kind,
    pub origin: Origin,
    pub title: String,
    pub description: Option<String>,
    pub cover_url: Option<Url>,
    /// Download new units automatically (Source origin only).
    pub auto_download: bool,
    /// [`Category`] id ("reading" by default).
    pub category: String,
    pub genres: Vec<String>,
    pub added_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    /// Set while a LocalFile publication's file has vanished from disk.
    pub missing_since: Option<DateTime<Utc>>,
}

/// The frozen 1.x JSON shape of a publication. One place defines the wire:
/// old clients read `source_id`/`source_key` as required strings, so
/// LocalFile serializes them as `"local"` + the path; deserializing a 1.x
/// payload with `source_id == "local"` heals into a LocalFile origin.
#[derive(Serialize, Deserialize)]
struct PublicationWire {
    id: Uuid,
    #[serde(default)]
    kind: Kind,
    source_id: String,
    source_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cover_url: Option<Url>,
    auto_download: bool,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    genres: Vec<String>,
    added_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing_since: Option<DateTime<Utc>>,
}

impl From<Publication> for PublicationWire {
    fn from(p: Publication) -> Self {
        let (source_id, source_key, file_path) = match p.origin {
            Origin::Source { source_id, source_key } => (source_id, source_key, None),
            Origin::LocalFile { path } => ("local".into(), path.clone(), Some(path)),
        };
        PublicationWire {
            id: p.id,
            kind: p.kind,
            source_id,
            source_key,
            file_path,
            title: p.title,
            description: p.description,
            cover_url: p.cover_url,
            auto_download: p.auto_download,
            category: p.category,
            genres: p.genres,
            added_at: p.added_at,
            last_checked_at: p.last_checked_at,
            missing_since: p.missing_since,
        }
    }
}

impl TryFrom<PublicationWire> for Publication {
    type Error = String;

    fn try_from(w: PublicationWire) -> Result<Self, String> {
        let origin = match w.file_path {
            Some(path) => Origin::LocalFile { path },
            None if w.source_id == "local" => Origin::LocalFile { path: w.source_key },
            None => Origin::Source { source_id: w.source_id, source_key: w.source_key },
        };
        Ok(Publication {
            id: w.id,
            kind: w.kind,
            origin,
            title: w.title,
            description: w.description,
            cover_url: w.cover_url,
            auto_download: w.auto_download,
            category: w.category,
            genres: w.genres,
            added_at: w.added_at,
            last_checked_at: w.last_checked_at,
            missing_since: w.missing_since,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingUnit {
    pub id: Uuid,
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    pub source_key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<f64>,
    pub source_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanlator: Option<String>,
    pub fetched_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    pub download: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    #[serde(default)]
    pub read: bool,
}
```

(All doc comments from the old `Chapter` fields may be carried over; body above is authoritative for names/attributes.)

- [ ] **Step 4: Rewrite `progress.rs` positions as locators**

In `crates/yomu-domain/src/progress.rs`, rename fields on `ProgressEvent` and replace `Position`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub id: Uuid,
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    /// 0-based page within the unit.
    pub page: u32,
    pub device: String,
    pub at: DateTime<Utc>,
}

/// Merged current position for a publication (was `Position`). Wire shape
/// stays `{chapter_id, page, at}`: `locations` flattens and the enum is
/// untagged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locator {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    #[serde(flatten)]
    pub locations: Locations,
    pub at: DateTime<Utc>,
}

/// Where within a unit. Page-based for image kinds (all of this slice);
/// a `Progression` variant arrives with the EPUB slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Locations {
    Page { page: u32 },
}

impl Locator {
    /// The page for image-based kinds.
    pub fn page(&self) -> u32 {
        match self.locations {
            Locations::Page { page } => page,
        }
    }
}
```

`merge_position` keeps its name and signature (it merges journal events). Update its tests' field names (`manga_id:`→`publication_id:`, `chapter_id:`→`unit_id:`).

Add a wire test in `progress.rs`'s test module:

```rust
#[test]
fn locator_wire_is_1x_position() {
    let json = r#"{"chapter_id":"00000000-0000-0000-0000-000000000002","page":7,"at":"2026-01-01T00:00:00Z"}"#;
    let l: Locator = serde_json::from_str(json).unwrap();
    assert_eq!(l.page(), 7);
    let out = serde_json::to_value(&l).unwrap();
    assert_eq!(out["chapter_id"], "00000000-0000-0000-0000-000000000002");
    assert_eq!(out["page"], 7);
    assert!(out.get("locations").is_none());
}
```

- [ ] **Step 5: Rename api.rs envelopes per the naming map**

In `crates/yomu-domain/src/api.rs`: apply the naming-map renames. Field-level serde attributes where the Rust name changes:

```rust
use crate::{Kind, Locator, Publication, ReadingUnit};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddPublicationRequest {
    pub source_id: String,
    pub source_key: String,
    #[serde(default)]
    pub auto_download: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePublicationRequest {
    pub auto_download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationWithLocator {
    #[serde(flatten)]
    pub publication: Publication,
    #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
    #[serde(rename = "chapter_count")]
    pub unit_count: u32,
    #[serde(default)]
    pub unread_count: u32,
    #[serde(default)]
    pub downloaded_count: u32,
    #[serde(rename = "latest_chapter_at", default, skip_serializing_if = "Option::is_none")]
    pub latest_unit_at: Option<DateTime<Utc>>,
    #[serde(rename = "position_chapter_title", default, skip_serializing_if = "Option::is_none")]
    pub locator_unit_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicationDetailResponse {
    #[serde(rename = "manga")]
    pub publication: Publication,
    #[serde(rename = "chapters")]
    pub units: Vec<ReadingUnit>,
    #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetLocatorRequest {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    pub page: u32,
    #[serde(default = "default_device")]
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadUnitsRequest {
    #[serde(rename = "chapter_ids")]
    pub unit_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkUnitsRequest {
    #[serde(rename = "chapter_ids")]
    pub unit_ids: Vec<Uuid>,
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkUnitsResponse {
    pub affected: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadQueueEntry {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    #[serde(rename = "manga_title")]
    pub publication_title: String,
    #[serde(rename = "chapter_title")]
    pub unit_title: String,
    pub state: crate::DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<DownloadProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEvent {
    #[serde(rename = "manga_id")]
    pub publication_id: Uuid,
    #[serde(rename = "manga_title")]
    pub publication_title: String,
    #[serde(rename = "chapter_count")]
    pub unit_count: u32,
    pub first_title: String,
    pub last_title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagesResponse {
    #[serde(rename = "chapter_id")]
    pub unit_id: Uuid,
    pub page_count: u32,
    pub downloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshResponse {
    #[serde(rename = "new_chapters")]
    pub new_units: u32,
    pub checked_at: DateTime<Utc>,
}

/// `POST /library/rescan` outcome: what the streamer scan changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RescanResponse {
    pub added: u32,
    pub updated: u32,
    /// LocalFile publications newly flagged missing by this scan.
    pub missing: u32,
}
```

Everything else in api.rs (`HealthResponse`, `ApiErrorBody`, `UpdateCategoryRequest`, `PushEventsRequest/Response`, `EventsResponse`, `SourceSearchResults`, `DownloadProgress`, `DownloadsResponse`, `UpdatesResponse`, `default_device`) is unchanged except the `use` line. Suppress the unused-`Kind` import if nothing references it yet (it will in later slices; just don't import it if unused).

- [ ] **Step 6: Rewrite backup.rs**

```rust
use crate::{Category, ProgressEvent, Publication, ReadingUnit};

pub const BACKUP_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub categories: Vec<Category>,
    #[serde(rename = "manga")]
    pub publications: Vec<Publication>,
    #[serde(rename = "chapters")]
    pub units: Vec<ReadingUnit>,
    #[serde(rename = "read_chapter_ids")]
    pub read_unit_ids: Vec<Uuid>,
    pub progress: Vec<ProgressEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreSummary {
    #[serde(rename = "manga")]
    pub publications: u32,
    #[serde(rename = "chapters")]
    pub units: u32,
    pub categories: u32,
    pub read_marks: u32,
    pub progress_events: u32,
}
```

`BACKUP_VERSION` stays 1: the 2.0 shape is a strict superset and 1.x files parse through the wire mirror.

- [ ] **Step 7: Update lib.rs module list**

In `crates/yomu-domain/src/lib.rs`: `pub mod library;` → `pub mod publication;`, `pub use library::*;` → `pub use publication::*;`.

- [ ] **Step 8: Run domain tests**

Run: `cargo test -p yomu-domain`
Expected: PASS (golden wire tests + updated merge tests). Fix compile fallout inside yomu-domain only.

- [ ] **Step 9: Commit**

```bash
git add crates/yomu-domain
git -c commit.gpgsign=false commit -m "refactor(domain)!: publication model with frozen 1.x wire"
```

---

### Task 2: Server — migration 0011, DB layer, workspace-wide rename in yomu-server

**Files:**
- Create: `crates/yomu-server/migrations/0011_publications.sql`
- Rename: `crates/yomu-server/src/db/manga.rs` → `db/publications.rs`, `db/chapters.rs` → `db/units.rs`
- Modify: every file in `crates/yomu-server/src/` that names the old types (db/*, api/*, sync.rs, updater.rs, downloader.rs, state.rs, notifier.rs, catalog.rs)

- [ ] **Step 1: Write migration 0011**

Create `crates/yomu-server/migrations/0011_publications.sql`:

```sql
-- 2.0: manga generalizes to publications, chapters to reading units.
-- Renames + an origin split: scraped rows keep source_id/source_key,
-- streamer-managed files live in file_path (exactly one side set).
-- The old built-in "local" source's rows (source_id='local', source_key =
-- path relative to its dir) convert to the file origin, ids untouched.
-- sqlx wraps this file in one transaction; deferring FK checks lets the
-- publications rebuild drop/recreate the parent table mid-transaction.
PRAGMA defer_foreign_keys = ON;

ALTER TABLE manga RENAME TO publications;
ALTER TABLE chapters RENAME TO reading_units;
ALTER TABLE reading_units RENAME COLUMN manga_id TO publication_id;
ALTER TABLE progress_events RENAME COLUMN manga_id TO publication_id;
ALTER TABLE progress_events RENAME COLUMN chapter_id TO unit_id;
ALTER TABLE read_chapters RENAME TO read_units;
ALTER TABLE read_units RENAME COLUMN chapter_id TO unit_id;
ALTER TABLE manga_genres RENAME TO publication_genres;
ALTER TABLE publication_genres RENAME COLUMN manga_id TO publication_id;
ALTER TABLE updates RENAME COLUMN manga_id TO publication_id;

-- Rebuild publications: nullable source columns, the three new columns,
-- and the exactly-one-origin CHECK (SQLite can't ALTER those in).
CREATE TABLE publications_new (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL DEFAULT 'comics',
    source_id       TEXT,
    source_key      TEXT,
    file_path       TEXT,
    title           TEXT NOT NULL,
    description     TEXT,
    cover_url       TEXT,
    auto_download   INTEGER NOT NULL DEFAULT 0,
    category        TEXT NOT NULL DEFAULT 'reading',
    added_at        TEXT NOT NULL,
    last_checked_at TEXT,
    missing_since   TEXT,
    CHECK (
        (source_id IS NOT NULL AND source_key IS NOT NULL AND file_path IS NULL)
        OR (source_id IS NULL AND source_key IS NULL AND file_path IS NOT NULL)
    ),
    UNIQUE (source_id, source_key),
    UNIQUE (file_path)
);
INSERT INTO publications_new (id, kind, source_id, source_key, file_path, title,
                              description, cover_url, auto_download, category,
                              added_at, last_checked_at)
SELECT id, 'comics',
       CASE WHEN source_id = 'local' THEN NULL ELSE source_id END,
       CASE WHEN source_id = 'local' THEN NULL ELSE source_key END,
       CASE WHEN source_id = 'local' THEN source_key ELSE NULL END,
       title, description, cover_url, auto_download, category,
       added_at, last_checked_at
FROM publications;
DROP TABLE publications;
ALTER TABLE publications_new RENAME TO publications;

-- Index names carry the old words; recreate under the new ones.
DROP INDEX idx_chapters_manga;
CREATE INDEX idx_units_publication ON reading_units(publication_id);
DROP INDEX idx_chapters_pending;
CREATE INDEX idx_units_pending ON reading_units(download_state)
    WHERE download_state = 'pending';
DROP INDEX idx_read_chapters_chapter;
CREATE INDEX idx_read_units_unit ON read_units(unit_id);
DROP INDEX idx_progress_manga;
CREATE INDEX idx_progress_publication ON progress_events(publication_id, at DESC, id DESC);

-- The catalog cache only serves scraper search/browse; drop stale rows the
-- removed built-in local source left behind.
DELETE FROM catalog_entries WHERE source_id = 'local';
DELETE FROM catalog_pages WHERE source_id = 'local';
```

- [ ] **Step 2: Write the failing migration test**

Append to the test module in `crates/yomu-server/src/db/mod.rs`:

```rust
/// Build a 1.x database raw (migrations 0001–0010), seed it like a deployed
/// instance — a scraped manga and a local-source one, with progress and read
/// marks — then apply 0011 and assert nothing was lost in the conversion.
#[tokio::test]
async fn migration_0011_converts_a_1x_database() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    for sql in [
        include_str!("../../migrations/0001_library.sql"),
        include_str!("../../migrations/0002_progress_seq.sql"),
        include_str!("../../migrations/0003_categories.sql"),
        include_str!("../../migrations/0004_auth.sql"),
        include_str!("../../migrations/0005_read_marks.sql"),
        include_str!("../../migrations/0006_progress_user_seq_index.sql"),
        include_str!("../../migrations/0007_chapter_published_at.sql"),
        include_str!("../../migrations/0008_catalog.sql"),
        include_str!("../../migrations/0009_genres.sql"),
        include_str!("../../migrations/0010_updates.sql"),
    ] {
        sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    }

    let shared = Uuid::nil().to_string();
    sqlx::raw_sql(
        "INSERT INTO manga (id, source_id, source_key, title, auto_download, added_at)
         VALUES ('00000000-0000-0000-0000-00000000000a', 'fixture', 'm1', 'Scraped', 1,
                 '2026-01-01T00:00:00Z'),
                ('00000000-0000-0000-0000-00000000000b', 'local', 'Solo Farming', 'Solo Farming',
                 0, '2026-01-01T00:00:00Z');
         INSERT INTO chapters (id, manga_id, source_key, title, source_order, fetched_at)
         VALUES ('00000000-0000-0000-0000-0000000000a1',
                 '00000000-0000-0000-0000-00000000000a', 'c1', 'Chapter 1', 0,
                 '2026-01-01T00:00:00Z'),
                ('00000000-0000-0000-0000-0000000000b1',
                 '00000000-0000-0000-0000-00000000000b', 'Solo Farming/Chapter 1',
                 'Chapter 1', 0, '2026-01-01T00:00:00Z');
         INSERT INTO manga_genres (manga_id, genre)
         VALUES ('00000000-0000-0000-0000-00000000000a', 'Action');",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO progress_events (id, user_id, manga_id, chapter_id, page, device, at)
         VALUES (?, ?, ?, ?, 4, 'test', '2026-01-02T00:00:00Z')",
    )
    .bind("00000000-0000-0000-0000-0000000000e1")
    .bind(&shared)
    .bind("00000000-0000-0000-0000-00000000000b")
    .bind("00000000-0000-0000-0000-0000000000b1")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO read_chapters (user_id, chapter_id, at)
         VALUES (?, '00000000-0000-0000-0000-0000000000a1', '2026-01-02T00:00:00Z')",
    )
    .bind(&shared)
    .execute(&pool)
    .await
    .unwrap();

    // 0011 runs inside one transaction under the real migrator; replicate
    // that here so defer_foreign_keys spans the publications rebuild.
    let migration = include_str!("../../migrations/0011_publications.sql");
    sqlx::raw_sql(&format!("BEGIN; {migration} COMMIT;"))
        .execute(&pool)
        .await
        .unwrap();

    let (kind, source_id, file_path): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT kind, source_id, file_path FROM publications WHERE id = ?")
            .bind("00000000-0000-0000-0000-00000000000a")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), source_id.as_deref(), file_path), ("comics", Some("fixture"), None));

    let (source_id, file_path): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT source_id, file_path FROM publications WHERE id = ?")
            .bind("00000000-0000-0000-0000-00000000000b")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((source_id, file_path.as_deref()), (None, Some("Solo Farming")));

    // Progress, read marks and genres survived under the renamed columns.
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM progress_events
         WHERE publication_id = '00000000-0000-0000-0000-00000000000b'
           AND unit_id = '00000000-0000-0000-0000-0000000000b1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
    let marks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM read_units").fetch_one(&pool).await.unwrap();
    assert_eq!(marks, 1);
    let genres: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM publication_genres WHERE publication_id = '00000000-0000-0000-0000-00000000000a'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(genres, 1);
}
```

(If the workspace sqlx version lacks `sqlx::raw_sql`, use `use sqlx::Executor; pool.execute(sql)` instead — same semantics.)

- [ ] **Step 3: Rename the DB row types and mapping in db/mod.rs**

In `crates/yomu-server/src/db/mod.rs`:
- `MangaRow` → `PublicationRow` with `kind: String`, `source_id: Option<String>`, `source_key: Option<String>`, `file_path: Option<String>`, `missing_since: Option<DateTime<Utc>>` added, and this mapping:

```rust
impl TryFrom<PublicationRow> for Publication {
    type Error = DbError;

    fn try_from(row: PublicationRow) -> Result<Self> {
        let kind = match row.kind.as_str() {
            "comics" => Kind::Comics,
            "novels" => Kind::Novels,
            "pdf" => Kind::Pdf,
            other => return Err(DbError::Corrupt(format!("kind {other:?}"))),
        };
        let origin = match (row.source_id, row.source_key, row.file_path) {
            (Some(source_id), Some(source_key), None) => Origin::Source { source_id, source_key },
            (None, None, Some(path)) => Origin::LocalFile { path },
            _ => return Err(DbError::Corrupt(format!("publication {} origin", row.id))),
        };
        Ok(Publication {
            id: parse_uuid(row.id)?,
            kind,
            origin,
            title: row.title,
            description: row.description,
            cover_url: parse_url_opt(row.cover_url)?,
            auto_download: row.auto_download,
            category: row.category,
            genres: Vec::new(),
            added_at: row.added_at,
            last_checked_at: row.last_checked_at,
            missing_since: row.missing_since,
        })
    }
}
```

- `ChapterRow` → `UnitRow` (`manga_id` field → `publication_id`), mapping to `ReadingUnit`.
- `EventRow` fields `manga_id`/`chapter_id` → `publication_id`/`unit_id`.
- `insert_chapters` → `insert_units` writing `INSERT INTO reading_units (id, publication_id, …)`; `write_genres` targets `publication_genres(publication_id, genre)`.
- The crash-recovery query in `with_options` becomes `UPDATE reading_units SET download_state = 'pending' WHERE download_state = 'downloading'`.
- `ChapterSync` → `UnitSync { new_units, file_ops }`; `ChapterFileOp` → `UnitFileOp { Remove { unit }, Rename { from, to } }`; `LibraryRollup.chapter_count` → `unit_count`, `latest_chapter_at` → `latest_unit_at`.

- [ ] **Step 4: Mechanical rename across yomu-server**

```bash
git mv crates/yomu-server/src/db/manga.rs crates/yomu-server/src/db/publications.rs
git mv crates/yomu-server/src/db/chapters.rs crates/yomu-server/src/db/units.rs
cd /projects/rust/yomu
grep -rl 'Manga\|Chapter\|Position' crates/yomu-server/src | xargs sed -i \
  -e 's/\bMangaDetailResponse\b/PublicationDetailResponse/g' \
  -e 's/\bMangaWithPosition\b/PublicationWithLocator/g' \
  -e 's/\bAddMangaRequest\b/AddPublicationRequest/g' \
  -e 's/\bUpdateMangaRequest\b/UpdatePublicationRequest/g' \
  -e 's/\bSetPositionRequest\b/SetLocatorRequest/g' \
  -e 's/\bDownloadChaptersRequest\b/DownloadUnitsRequest/g' \
  -e 's/\bMarkChaptersRequest\b/MarkUnitsRequest/g' \
  -e 's/\bBulkChaptersResponse\b/BulkUnitsResponse/g' \
  -e 's/\bChapterRow\b/UnitRow/g' \
  -e 's/\bMangaRow\b/PublicationRow/g' \
  -e 's/\bChapterSync\b/UnitSync/g' \
  -e 's/\bChapterFileOp\b/UnitFileOp/g'
```

`Manga`, `Chapter`, `Position` themselves collide with kept source-layer names (`MangaSummary`, `MangaDetails`, `ChapterRef`) — do those by hand, guided by the compiler: replace standalone `Manga` → `Publication`, `Chapter` → `ReadingUnit`, `Position` → `Locator` only where they refer to the library types. Then rename the SQL: in `db/publications.rs`, `db/units.rs`, `db/backup.rs`, `db/read_marks.rs`, `db/progress.rs`, `db/downloads.rs`, `db/updates.rs`, every SQL string changes table/column names per the DB map (e.g. `FROM manga` → `FROM publications`, `manga_id` → `publication_id`, `chapters` → `reading_units`, `read_chapters` → `read_units`, `manga_genres` → `publication_genres`, `chapter_id` → `unit_id`). Rename Db methods per the naming map, and rename `AppState::chapter_dir` → `unit_dir`.

`db/publications.rs` specifics:
- `insert_publication(&self, source_id: &str, details: &MangaDetails, auto_download: bool) -> Result<Publication>` — the INSERT keeps writing `source_id`/`source_key` (Source origin); error message becomes `"publication already in library"`.
- `list_publications_for_update` gains the LocalFile exclusion:

```sql
SELECT m.* FROM publications m
JOIN categories c ON c.id = m.category
WHERE c.update_enabled = 1 AND m.file_path IS NULL
ORDER BY m.title COLLATE NOCASE
```

- `latest_positions`/`latest_position` build `Locator { unit_id, locations: Locations::Page { page }, at }`.
- Add the streamer-facing helpers (used from Task 5, written now so the module is complete):

```rust
/// LocalFile publications, keyed for the streamer's upsert.
pub async fn list_local_publications(&self) -> Result<Vec<Publication>> {
    let rows = sqlx::query_as::<_, PublicationRow>(
        "SELECT * FROM publications WHERE file_path IS NOT NULL ORDER BY title COLLATE NOCASE",
    )
    .fetch_all(&self.pool)
    .await?;
    rows.into_iter().map(Publication::try_from).collect()
}

/// Insert a streamer-discovered publication with its units.
pub async fn insert_local_publication(
    &self,
    path: &str,
    details: &MangaDetails,
) -> Result<Publication> {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let mut tx = self.pool.begin().await?;
    sqlx::query(
        "INSERT INTO publications (id, kind, file_path, title, description, cover_url,
                                   auto_download, added_at)
         VALUES (?, 'comics', ?, ?, ?, ?, 0, ?)",
    )
    .bind(id.to_string())
    .bind(path)
    .bind(&details.summary.title)
    .bind(&details.description)
    .bind(details.summary.cover_url.as_deref())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            DbError::Constraint("file already in library".into())
        }
        _ => DbError::Sqlx(e),
    })?;
    insert_units(&mut tx, id, &details.chapters, now).await?;
    write_genres(&mut tx, id, &details.genres).await?;
    tx.commit().await?;
    self.get_publication(id).await
}

/// Re-point a missing LocalFile publication at a renamed path (self-heal).
pub async fn repoint_local_publication(&self, id: Uuid, path: &str) -> Result<()> {
    sqlx::query(
        "UPDATE publications SET file_path = ?, missing_since = NULL WHERE id = ?",
    )
    .bind(path)
    .bind(id.to_string())
    .execute(&self.pool)
    .await?;
    Ok(())
}

/// Flag (Some) or clear (None) a vanished LocalFile publication.
pub async fn set_missing_since(&self, id: Uuid, at: Option<DateTime<Utc>>) -> Result<()> {
    sqlx::query("UPDATE publications SET missing_since = ? WHERE id = ?")
        .bind(at)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
    Ok(())
}

/// Refresh scan-derived metadata (cover/description) without touching title.
pub async fn update_local_metadata(
    &self,
    id: Uuid,
    description: Option<&str>,
    cover_url: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE publications SET description = ?, cover_url = ? WHERE id = ?")
        .bind(description)
        .bind(cover_url)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
    Ok(())
}
```

`db/backup.rs` `import_backup` writes the origin split:

```rust
for publication in &backup.publications {
    let (source_id, source_key, file_path) = match &publication.origin {
        Origin::Source { source_id, source_key } => {
            (Some(source_id.as_str()), Some(source_key.as_str()), None)
        }
        Origin::LocalFile { path } => (None, None, Some(path.as_str())),
    };
    let r = sqlx::query(
        "INSERT INTO publications (id, kind, source_id, source_key, file_path, title,
                                   description, cover_url, auto_download, category,
                                   added_at, last_checked_at, missing_since)
         VALUES (?, 'comics', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(publication.id.to_string())
    .bind(source_id)
    .bind(source_key)
    .bind(file_path)
    .bind(&publication.title)
    .bind(&publication.description)
    .bind(publication.cover_url.as_ref().map(|u| u.as_str()))
    .bind(publication.auto_download)
    .bind(&publication.category)
    .bind(publication.added_at)
    .bind(publication.last_checked_at)
    .bind(publication.missing_since)
    .execute(&mut *tx)
    .await?;
    write_genres(&mut tx, publication.id, &publication.genres).await?;
    summary.publications += r.rows_affected() as u32;
}
```

(The rest of import_backup: table/column renames only.)

api/* handlers: renames only in this task — behavior changes come in Task 6. `api/library.rs` `list()` builds `PublicationWithLocator { locator, unit_count, …, locator_unit_title, publication }` from the renamed rollups.

- [ ] **Step 5: Run server tests**

Run: `cargo test -p yomu-server`
Expected: PASS, including `migration_0011_converts_a_1x_database` and the whole pre-existing suite under the new names. Iterate on compile errors until green.

- [ ] **Step 6: Commit**

```bash
git add -A crates/yomu-server
git -c commit.gpgsign=false commit -m "refactor(server)!: publications schema (migration 0011) and rename"
```

---

### Task 3: Client, UI, web, shell rename — workspace green

**Files:**
- Modify: `crates/yomu-client/src/lib.rs`, all of `crates/yomu-ui/src/`, `crates/yomu-web/src/main.rs`, `crates/yomu-shell/src/lib.rs` (whatever names the old types)

- [ ] **Step 1: Apply the same type renames**

Run the Task 2 Step 4 sed over `crates/yomu-client crates/yomu-ui crates/yomu-web crates/yomu-shell`, then hand-fix `Manga`/`Chapter`/`Position` occurrences (again: `MangaSummary`/`MangaDetails`/`ChapterRef` stay). Rename client methods and struct fields per the naming map (`entry.manga` → `entry.publication`, `detail.manga` → `detail.publication`, `detail.chapters` → `detail.units`, `position` → `locator`, `chapter_count` → `unit_count`, `latest_chapter_at` → `latest_unit_at`, `position_chapter_title` → `locator_unit_title`, `ProgressEvent.manga_id/.chapter_id` → `.publication_id`/`.unit_id`). **Do not** touch route strings, UI URL paths, localStorage keys, or user-visible strings in this task — pure rename.

Where the reader uses `Position`/`.page`, the new `Locator::page()` accessor gives the page; constructing one is `Locator { unit_id, locations: Locations::Page { page }, at }`.

- [ ] **Step 2: Compile-fix loop**

Run: `cargo check -p yomu-client && just check`
Expected: PASS (fmt, clippy, wasm checks). Iterate.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace --exclude yomu-shell`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "refactor(client,ui)!: publication naming across clients (wire unchanged)"
```

---

### Task 4: Streamer file layer — move local.rs into the server

**Files:**
- Create: `crates/yomu-server/src/streamer/mod.rs`, `crates/yomu-server/src/streamer/files.rs`
- Modify: `crates/yomu-server/src/main.rs` (add `mod streamer;`), `crates/yomu-server/Cargo.toml`, workspace `Cargo.toml` if needed
- Source of moved code: `crates/yomu-source/src/local.rs` (deleted in Task 6)

- [ ] **Step 1: Add dependencies**

In `crates/yomu-server/Cargo.toml` add `zip.workspace = true` and `regex.workspace = true` (both already in the workspace table — verify with `grep -n 'zip\|regex' Cargo.toml`).

- [ ] **Step 2: Create `streamer/files.rs`**

Copy from `crates/yomu-source/src/local.rs` (leave local.rs in place until Task 6): the constants `IMAGE_EXTENSIONS`, `COVER_STEMS`, the `Details` struct, and the functions `io_err`, `is_image_name`, `content_type_of`, `chapter_number`, `sort_pages` plus their unit tests, and rework the `LocalSource` methods into a plain struct:

```rust
//! File resolution for the streamer: CBZ archives and image directories
//! under the books dir, addressed by dir-relative keys and `local:` URLs.
//! Moved from the retired built-in local source; the `local:` URL scheme is
//! kept verbatim so cover/page URLs stored by 1.x keep resolving.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use url::Url;
use yomu_source::{ImageData, SourceError};

pub type Result<T> = std::result::Result<T, SourceError>;

/// Serves and inspects the books dir. Cheap to clone-free share behind
/// `Arc` in `AppState`.
pub struct Streamer {
    pub books_dir: PathBuf,
    base: Url,
}

impl Streamer {
    pub fn new(books_dir: PathBuf) -> Self {
        Self {
            books_dir,
            base: Url::parse("local:///").expect("valid local base url"),
        }
    }
}
```

Then move these `LocalSource` methods onto `Streamer` **unchanged in body** except `self.dir` → `self.books_dir`: `resolve`, `local_url`, `series_details`, `find_cover`, `chapter_pages` (rename to `pages`, `pub async fn pages(&self, unit_key: &str)`), `read_local` (rename to `pub async fn image(&self, url: &Url)`). `find_cover`'s recursive call site changes from `self.chapter_pages(...)` to `self.pages(...)`. `series_details` stays `pub(super)` (the scan uses it). Carry over local.rs's `#[cfg(test)]` tests that exercise these (`chapter_numbers_from_names`, `pages_sort_numerically`, `keys_cannot_escape_the_local_dir`, `symlinked_keys_cannot_escape_the_local_dir`, `series_chapters_and_pages_from_disk` — the last one rewritten against `Streamer::new(root)` and `streamer.series_details(..)`/`.pages(..)`/`.image(..)` instead of the `Source` trait).

- [ ] **Step 3: Create a minimal `streamer/mod.rs`**

```rust
//! Server-side streamer: turns user-supplied comic files (CBZ archives,
//! image directories) in the configured books dir into library entries and
//! serves their pages. The scan half lives here; file resolution in `files`.

mod files;

pub use files::Streamer;
```

Add `mod streamer;` to `main.rs`.

- [ ] **Step 4: Verify**

Run: `cargo test -p yomu-server streamer`
Expected: PASS (moved tests green under the new home).

- [ ] **Step 5: Commit**

```bash
git add -A crates/yomu-server Cargo.toml Cargo.lock
git -c commit.gpgsign=false commit -m "feat(server): streamer file layer (books dir, cbz + image dirs)"
```

---

### Task 5: Streamer scan — discovery, upsert, missing, self-heal, updates feed

**Files:**
- Modify: `crates/yomu-server/src/streamer/mod.rs`, `crates/yomu-server/src/streamer/files.rs`
- Test: `#[cfg(test)]` in `streamer/mod.rs`

- [ ] **Step 1: Write the failing scan tests**

Append to `streamer/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use uuid::Uuid;
    use yomu_domain::Origin;

    struct Fixture {
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("yomu-scan-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn page(&self, rel: &str) {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"png").unwrap();
        }

        fn cbz(&self, rel: &str, entries: &[&str]) {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let file = std::fs::File::create(path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::SimpleFileOptions = Default::default();
            for entry in entries {
                use std::io::Write;
                zip.start_file(*entry, options).unwrap();
                zip.write_all(b"png").unwrap();
            }
            zip.finish().unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn scan_discovers_series_root_cbz_and_skips_unsupported() {
        let fx = Fixture::new("discover");
        fx.page("Solo Farming/Chapter 1/001.png");
        fx.page("Solo Farming/Chapter 1/002.png");
        fx.cbz("Solo Farming/Chapter 2.cbz", &["001.png"]);
        fx.page("Solo Farming/cover.png");
        fx.cbz("One Shot.cbz", &["p1.png", "p2.png"]);
        fx.page("Loose Pages/001.png");
        std::fs::write(fx.root.join("novel.epub"), b"nope").unwrap();
        std::fs::write(fx.root.join("broken.cbz"), b"not a zip").unwrap();

        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        let outcome = scan(&streamer, &db, None).await.unwrap();
        // Solo Farming + One Shot + Loose Pages; epub skipped, corrupt cbz
        // skipped with a warning, neither aborts the scan.
        assert_eq!((outcome.added, outcome.missing), (3, 0));

        let pubs = db.list_local_publications().await.unwrap();
        assert_eq!(pubs.len(), 3);
        let solo = pubs.iter().find(|p| p.title == "Solo Farming").unwrap();
        assert_eq!(solo.origin, Origin::LocalFile { path: "Solo Farming".into() });
        assert_eq!(db.list_units(solo.id).await.unwrap().len(), 2);
        let one_shot = pubs.iter().find(|p| p.title == "One Shot").unwrap();
        assert_eq!(db.list_units(one_shot.id).await.unwrap().len(), 1);

        // Idempotent: nothing new the second time.
        let again = scan(&streamer, &db, None).await.unwrap();
        assert_eq!((again.added, again.updated, again.missing), (0, 0, 0));
    }

    #[tokio::test]
    async fn new_units_in_known_publications_feed_updates() {
        let fx = Fixture::new("updates");
        fx.page("Series/Chapter 1/001.png");
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();
        // The initial add must NOT announce a backlog.
        assert!(db.updates_since(chrono::DateTime::<chrono::Utc>::MIN_UTC, 100).await.unwrap().is_empty());

        fx.page("Series/Chapter 2/001.png");
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!(outcome.updated, 1);
        let updates = db.updates_since(chrono::DateTime::<chrono::Utc>::MIN_UTC, 100).await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].unit_count, 1);
    }

    #[tokio::test]
    async fn vanished_files_flag_missing_and_reappearing_clears() {
        let fx = Fixture::new("missing");
        fx.page("Series/Chapter 1/001.png");
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();

        std::fs::rename(fx.root.join("Series"), fx.root.join(".hidden-away")).unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!(outcome.missing, 1);
        let p = &db.list_local_publications().await.unwrap()[0];
        assert!(p.missing_since.is_some());
        // Progress-carrying row survives; re-flagging is not double-counted.
        assert_eq!(scan(&streamer, &db, None).await.unwrap().missing, 0);

        std::fs::rename(fx.root.join(".hidden-away"), fx.root.join("Series")).unwrap();
        scan(&streamer, &db, None).await.unwrap();
        assert!(db.list_local_publications().await.unwrap()[0].missing_since.is_none());
    }

    #[tokio::test]
    async fn rename_self_heals_by_unique_title_only() {
        let fx = Fixture::new("heal");
        fx.page("Old Name/Chapter 1/001.png");
        std::fs::write(
            fx.root.join("Old Name/details.json"),
            br#"{"title": "Kept Title"}"#,
        )
        .unwrap();
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();
        let original = db.list_local_publications().await.unwrap()[0].clone();

        // Rename the dir, keep the details title: unique missing-title match.
        std::fs::rename(fx.root.join("Old Name"), fx.root.join("New Name")).unwrap();
        std::fs::write(
            fx.root.join("New Name/details.json"),
            br#"{"title": "Kept Title"}"#,
        )
        .unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!((outcome.added, outcome.updated, outcome.missing), (0, 1, 0));
        let healed = db.list_local_publications().await.unwrap();
        assert_eq!(healed.len(), 1, "re-pointed, not duplicated");
        assert_eq!(healed[0].id, original.id, "id (and thus progress) survives");
        assert_eq!(healed[0].origin, Origin::LocalFile { path: "New Name".into() });
        assert!(healed[0].missing_since.is_none());
    }

    #[tokio::test]
    async fn ambiguous_title_match_never_guesses() {
        let fx = Fixture::new("ambiguous");
        fx.page("A/Chapter 1/001.png");
        fx.page("B/Chapter 1/001.png");
        std::fs::write(fx.root.join("A/details.json"), br#"{"title": "Same"}"#).unwrap();
        std::fs::write(fx.root.join("B/details.json"), br#"{"title": "Same"}"#).unwrap();
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();

        std::fs::rename(fx.root.join("A"), fx.root.join(".gone-a")).unwrap();
        std::fs::rename(fx.root.join("B"), fx.root.join(".gone-b")).unwrap();
        scan(&streamer, &db, None).await.unwrap();
        fx.page("C/Chapter 1/001.png");
        std::fs::write(fx.root.join("C/details.json"), br#"{"title": "Same"}"#).unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        // Two missing candidates share the title: C is a NEW publication and
        // both stay flagged.
        assert_eq!(outcome.added, 1);
        let pubs = db.list_local_publications().await.unwrap();
        assert_eq!(pubs.len(), 3);
        assert_eq!(pubs.iter().filter(|p| p.missing_since.is_some()).count(), 2);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p yomu-server streamer`
Expected: FAIL (`scan` not defined, `ScanOutcome` missing).

- [ ] **Step 3: Implement discovery in `files.rs`**

Add to `impl Streamer`:

```rust
/// One publication found on disk, ready to upsert.
pub(super) struct Discovered {
    /// Books-dir-relative path — the publication's identity.
    pub path: String,
    pub details: yomu_domain::MangaDetails,
}

impl Streamer {
    /// Walk the books dir top level. Series directories (holding chapter
    /// dirs / .cbz) become multi-unit publications; root-level .cbz files
    /// and loose image directories become single-unit ones. Anything else
    /// is skipped with one info line — the folder will legitimately hold
    /// future-format files (.epub, .pdf, .cbr).
    pub(super) async fn discover(&self) -> Vec<Discovered> {
        let mut out = Vec::new();
        let mut reader = match tokio::fs::read_dir(&self.books_dir).await {
            Ok(reader) => reader,
            Err(_) => return out, // missing dir = empty library, not an error
        };
        while let Some(entry) = reader.next_entry().await.ok().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "details.json" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            match self.discover_entry(&name, is_dir).await {
                Ok(Some(found)) => out.push(found),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(entry = %name, %err, "streamer: skipping unreadable entry");
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    async fn discover_entry(&self, name: &str, is_dir: bool) -> Result<Option<Discovered>> {
        if !is_dir {
            if !name.to_lowercase().ends_with(".cbz") {
                tracing::info!(file = %name, "streamer: unsupported file type, skipping");
                return Ok(None);
            }
            // Root-level archive: single-unit publication. Probing the page
            // list up front surfaces corrupt archives at scan time.
            self.pages(name).await?;
            let title = name.trim_end_matches(".cbz").trim_end_matches(".CBZ").to_string();
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: single_unit_details(name, &title),
            }));
        }

        // A directory is a series when it holds chapter dirs or archives;
        // a directory of loose images is a single-unit publication.
        let dir = self.books_dir.join(name);
        let mut has_chapters = false;
        let mut has_images = false;
        let mut reader = tokio::fs::read_dir(&dir).await.map_err(io_err)?;
        while let Some(entry) = reader.next_entry().await.map_err(io_err)? {
            let child = entry.file_name().to_string_lossy().into_owned();
            if child.starts_with('.') {
                continue;
            }
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                || child.to_lowercase().ends_with(".cbz")
            {
                has_chapters = true;
            } else if is_image_name(&child) {
                has_images = true;
            }
        }
        if has_chapters {
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: self.series_details(name).await?,
            }));
        }
        if has_images {
            self.pages(name).await?;
            return Ok(Some(Discovered {
                path: name.to_string(),
                details: single_unit_details(name, name),
            }));
        }
        tracing::info!(dir = %name, "streamer: no readable content, skipping");
        Ok(None)
    }
}

/// A one-shot: the publication and its only unit share the path as key.
fn single_unit_details(path: &str, title: &str) -> yomu_domain::MangaDetails {
    yomu_domain::MangaDetails {
        summary: yomu_domain::MangaSummary {
            key: path.to_string(),
            title: title.to_string(),
            cover_url: None,
            in_library: None,
        },
        description: None,
        genres: Vec::new(),
        chapters: vec![yomu_domain::ChapterRef {
            key: path.to_string(),
            title: title.to_string(),
            number: chapter_number(title),
            source_order: 0,
            scanlator: None,
            published_at: None,
        }],
    }
}
```

Note: `series_details` already reads `details.json`, orders chapters, and resolves the cover — reuse as-is.

- [ ] **Step 4: Implement scan/upsert in `mod.rs`**

```rust
mod files;

use std::collections::HashMap;

use chrono::Utc;
use yomu_domain::Origin;

pub use files::Streamer;

use crate::db::{Db, DbError};
use crate::notifier::Notifier;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Db(#[from] DbError),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanOutcome {
    pub added: u32,
    /// Known publications that changed: new units found or path re-pointed.
    pub updated: u32,
    /// Publications newly flagged missing by this scan.
    pub missing: u32,
}

/// One full scan of the books dir: upsert publications and units, feed the
/// updates feed (and ntfy when a notifier is passed) for new units in known
/// publications, flag vanished files, self-heal unambiguous renames.
/// Never destructive: rows and progress always survive.
pub async fn scan(
    streamer: &Streamer,
    db: &Db,
    notifier: Option<&Notifier>,
) -> Result<ScanOutcome, ScanError> {
    let discovered = streamer.discover().await;
    let existing = db.list_local_publications().await?;
    let mut by_path: HashMap<String, &yomu_domain::Publication> = existing
        .iter()
        .filter_map(|p| match &p.origin {
            Origin::LocalFile { path } => Some((path.clone(), p)),
            Origin::Source { .. } => None,
        })
        .collect();

    let mut outcome = ScanOutcome::default();
    let mut seen = std::collections::HashSet::new();

    for found in &discovered {
        seen.insert(found.path.clone());
        if let Some(publication) = by_path.get(found.path.as_str()) {
            let changed = sync_known(streamer, db, notifier, publication, found).await?;
            if changed {
                outcome.updated += 1;
            }
            continue;
        }

        // New path. Before inserting, an unambiguous title match against a
        // *missing* publication is the same book renamed on disk: re-point
        // it so ids (and progress) survive. Two candidates → never guess.
        let candidates: Vec<_> = existing
            .iter()
            .filter(|p| p.missing_since.is_some() && p.title == found.details.summary.title)
            .filter(|p| match &p.origin {
                Origin::LocalFile { path } => !seen.contains(path),
                Origin::Source { .. } => false,
            })
            .collect();
        match candidates.as_slice() {
            [only] => {
                db.repoint_local_publication(only.id, &found.path).await?;
                // sync_units re-keys units by number/title twin-matching.
                db.sync_units(only.id, &found.details.chapters).await?;
                db.update_local_metadata(
                    only.id,
                    found.details.description.as_deref(),
                    found.details.summary.cover_url.as_deref(),
                )
                .await?;
                if let Origin::LocalFile { path } = &only.origin {
                    seen.insert(path.clone());
                }
                outcome.updated += 1;
            }
            _ => {
                match db.insert_local_publication(&found.path, &found.details).await {
                    Ok(_) => outcome.added += 1,
                    Err(DbError::Constraint(err)) => {
                        tracing::warn!(path = %found.path, %err, "streamer: insert skipped");
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        // Keep the borrow map coherent for later duplicates in this pass.
        by_path.remove(found.path.as_str());
    }

    // Anything known that the walk didn't see has vanished from disk.
    for publication in &existing {
        let Origin::LocalFile { path } = &publication.origin else { continue };
        if !seen.contains(path) && publication.missing_since.is_none() {
            db.set_missing_since(publication.id, Some(Utc::now())).await?;
            outcome.missing += 1;
            tracing::info!(title = %publication.title, "streamer: file missing, flagged");
        }
    }

    Ok(outcome)
}

/// Re-sync a known publication: new units feed the updates feed + ntfy
/// (the rescan is the local updater), a cleared missing flag heals.
async fn sync_known(
    _streamer: &Streamer,
    db: &Db,
    notifier: Option<&Notifier>,
    publication: &yomu_domain::Publication,
    found: &files::Discovered,
) -> Result<bool, ScanError> {
    let sync = db.sync_units(publication.id, &found.details.chapters).await?;
    db.update_local_metadata(
        publication.id,
        found.details.description.as_deref(),
        found.details.summary.cover_url.as_deref(),
    )
    .await?;
    db.set_genres(publication.id, &found.details.genres).await?;
    let mut changed = false;
    if publication.missing_since.is_some() {
        db.set_missing_since(publication.id, None).await?;
        changed = true;
    }
    if !sync.new_units.is_empty() {
        db.add_update(publication.id, &sync.new_units).await?;
        if let Some(notifier) = notifier {
            notifier
                .notify_new_units(&publication.title, &sync.new_units)
                .await;
        }
        changed = true;
    }
    Ok(changed)
}
```

(`notify_new_units` is `notifier.rs`'s `notify_new_chapters` after the Task 2 rename — a title plus the new `&[ReadingUnit]`.)

- [ ] **Step 5: Run the scan tests**

Run: `cargo test -p yomu-server streamer`
Expected: PASS, all five scenarios.

- [ ] **Step 6: Commit**

```bash
git add -A crates/yomu-server
git -c commit.gpgsign=false commit -m "feat(server): streamer scan — upsert, missing flags, rename self-heal, updates feed"
```

---

### Task 6: Wire the streamer in — config, startup/interval scan, rescan endpoint, origin branches, LocalSource removal

**Files:**
- Modify: `crates/yomu-server/src/config.rs`, `main.rs`, `state.rs`, `streamer/mod.rs` (spawn), `api/mod.rs`, `api/library.rs`, `api/chapters.rs`
- Delete: `crates/yomu-source/src/local.rs`; modify `crates/yomu-source/src/lib.rs`, `crates/yomu-source/Cargo.toml`

- [ ] **Step 1: Config — `[books]` replaces `[local]`**

In `config.rs`, replace `LocalConfig`/`local` with:

```rust
/// The streamer's watched folder: user-supplied comic files as
/// `<dir>/<Series>/<Chapter>/*.png`, `<Series>/<Chapter>.cbz`, or
/// root-level `.cbz` / image dirs (see `crate::streamer`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BooksConfig {
    pub enabled: bool,
    /// Directory holding the files. Defaults to the 1.x local-source dir so
    /// nothing moves on disk for existing deployments.
    pub dir: PathBuf,
    /// Seconds between periodic rescans (clamped to ≥ 60).
    pub scan_interval_secs: u64,
}

impl Default for BooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: PathBuf::from("local"),
            scan_interval_secs: 60 * 60,
        }
    }
}
```

In `Config`: field `pub local: LocalConfig` becomes `#[serde(alias = "local")] pub books: BooksConfig` (the alias keeps a deployed `[local]\ndir=…` TOML binding; `enabled`/`dir` field names are unchanged).

- [ ] **Step 2: State + startup**

`state.rs`: add `pub streamer: Arc<crate::streamer::Streamer>` to `AppState`, constructed in `AppState::new` from `config.books.dir` (`Arc::new(crate::streamer::Streamer::new(config.books.dir.clone()))` — build before `config` moves into the `Arc`).

`main.rs`: delete the `LocalSource` import and the whole `if config.local.enabled { sources.insert(...) }` block. After `updater::spawn(state.clone());` add `streamer::spawn(state.clone());`.

Add to `streamer/mod.rs`:

```rust
/// Startup + periodic scan. Unlike the scraper updater this scans
/// immediately: touching the disk is cheap and files added while the server
/// was down should appear right away.
pub fn spawn(state: crate::state::AppState) {
    if !state.config.books.enabled {
        return;
    }
    tokio::spawn(async move {
        let notifier = Notifier::new(state.config.notify.clone());
        let interval =
            std::time::Duration::from_secs(state.config.books.scan_interval_secs.max(60));
        loop {
            match scan(&state.streamer, &state.db, Some(&notifier)).await {
                Ok(outcome) => tracing::info!(
                    added = outcome.added,
                    updated = outcome.updated,
                    missing = outcome.missing,
                    "streamer scan complete"
                ),
                Err(err) => tracing::warn!(%err, "streamer scan failed"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}
```

- [ ] **Step 3: Rescan endpoint**

`api/library.rs`:

```rust
/// Manual "Rescan files" from the More page.
pub async fn rescan(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<RescanResponse>, ApiError> {
    let notifier = crate::notifier::Notifier::new(state.config.notify.clone());
    let outcome = crate::streamer::scan(&state.streamer, &state.db, Some(&notifier))
        .await
        .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    Ok(Json(RescanResponse {
        added: outcome.added,
        updated: outcome.updated,
        missing: outcome.missing,
    }))
}
```

`api/mod.rs` route table, next to the library routes:

```rust
.route("/library/rescan", axum::routing::post(library::rescan))
```

Extend the `mutating_routes_require_a_session_in_oidc_mode` test with:

```rust
assert_eq!(
    status_of("POST", "/api/v1/library/rescan").await,
    StatusCode::UNAUTHORIZED
);
```

- [ ] **Step 4: Origin branches in serving paths**

`api/chapters.rs` — `live_pages()` branches on origin:

```rust
async fn live_pages(state: &AppState, unit: &ReadingUnit) -> Result<Vec<Url>, ApiError> {
    if let Some(urls) = state.live_pages.get(unit.id).await {
        return Ok(urls);
    }
    let publication = state.db.get_publication(unit.publication_id).await?;
    let urls = match &publication.origin {
        Origin::LocalFile { .. } => state.streamer.pages(&unit.source_key).await?,
        Origin::Source { source_id, .. } => {
            let source = state
                .sources
                .get(source_id)
                .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
            source.pages(&unit.source_key).await?
        }
    };
    let _ = state.db.set_page_count(unit.id, urls.len() as u32).await;
    state.live_pages.put(unit.id, urls.clone()).await;
    Ok(urls)
}
```

`page_image()` — replace the source lookup + fetch with an origin match; the local arm has no CDN-expiry retry:

```rust
let publication = state.db.get_publication(unit.publication_id).await?;
let urls = live_pages(&state, &unit).await?;
let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
match &publication.origin {
    Origin::LocalFile { .. } => {
        let image = state.streamer.image(url).await?;
        Ok(image_response(image.bytes.to_vec(), image.content_type))
    }
    Origin::Source { source_id, .. } => {
        let source = state
            .sources
            .get(source_id)
            .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
        match source.image(url).await {
            Ok(image) => Ok(image_response(image.bytes.to_vec(), image.content_type)),
            Err(_) => {
                state.live_pages.invalidate(unit.id).await;
                let urls = live_pages(&state, &unit).await?;
                let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
                let image = source.image(url).await?;
                Ok(image_response(image.bytes.to_vec(), image.content_type))
            }
        }
    }
}
```

`api/library.rs` `cover()` — the uncached fallback branches the same way:

```rust
let image = match &publication.origin {
    Origin::LocalFile { .. } => state.streamer.image(&cover_url).await?,
    Origin::Source { source_id, .. } => {
        let source = state
            .sources
            .get(source_id)
            .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
        source.image(&cover_url).await?
    }
};
```

`api/library.rs` `refresh()` — LocalFile refresh is a targeted rescan (implemented as a full scan filtered to this publication's outcome; a full disk scan is cheap and reuses tested code):

```rust
pub async fn refresh(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let new_units = match &publication.origin {
        Origin::LocalFile { .. } => {
            let before = state.db.list_units(id).await?.len();
            crate::streamer::scan(&state.streamer, &state.db, None)
                .await
                .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
            (state.db.list_units(id).await?.len().saturating_sub(before)) as u32
        }
        Origin::Source { .. } => sync::refresh_publication(&state, &publication).await?.len() as u32,
    };
    Ok(Json(RefreshResponse {
        new_units,
        checked_at: chrono::Utc::now(),
    }))
}
```

(`sync::refresh_publication` is Task 2's rename of `sync::refresh_manga`; it must now destructure `Origin::Source` instead of reading `manga.source_id`, returning `SyncError::UnknownSource("local".into())` if a LocalFile publication is ever passed in.)

- [ ] **Step 5: Remove LocalSource**

```bash
git rm crates/yomu-source/src/local.rs
```

Remove `pub mod local;` from `crates/yomu-source/src/lib.rs`. Check `grep -rn "zip" crates/yomu-source/src` — if nothing remains, drop `zip.workspace = true` from `crates/yomu-source/Cargo.toml`.

- [ ] **Step 6: End-to-end check**

Run: `cargo test --workspace --exclude yomu-shell && just check`
Expected: PASS.

Manual smoke (optional but cheap): `mkdir -p /tmp/claude-1000/-projects-rust-yomu/*/scratchpad/books/Demo/Chapter\ 1 && cp` a png in, run `YOMU_BOOKS__DIR=<that dir> cargo run -p yomu-server`, then `curl localhost:4700/api/v1/library` — the Demo publication appears with `"source_id":"local"`.

- [ ] **Step 7: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "feat(server): watched books dir replaces the built-in local source"
```

---

### Task 7: Restore compatibility — 1.x backups into the new schema

**Files:**
- Test: append to the test module in `crates/yomu-server/src/db/mod.rs`

- [ ] **Step 1: Write the golden-restore test**

The wire mirror already converts 1.x JSON at deserialization; this test pins the whole path with a literal 1.x backup file:

```rust
/// A backup exported by a 1.x server (literal JSON, incl. a local-source
/// manga) must restore into the 2.0 schema with origins converted and the
/// user's reading state intact.
#[tokio::test]
async fn restore_accepts_a_1x_backup_file() {
    let json = r#"{
        "version": 1,
        "exported_at": "2026-01-01T00:00:00Z",
        "categories": [
            {"id":"reading","name":"Reading","position":0,"update_enabled":true}
        ],
        "manga": [
            {"id":"00000000-0000-0000-0000-00000000000a","source_id":"fixture",
             "source_key":"m1","title":"Scraped","auto_download":false,
             "category":"reading","added_at":"2026-01-01T00:00:00Z"},
            {"id":"00000000-0000-0000-0000-00000000000b","source_id":"local",
             "source_key":"Solo Farming","title":"Solo Farming","auto_download":false,
             "category":"reading","added_at":"2026-01-01T00:00:00Z"}
        ],
        "chapters": [
            {"id":"00000000-0000-0000-0000-0000000000a1",
             "manga_id":"00000000-0000-0000-0000-00000000000a","source_key":"c1",
             "title":"Chapter 1","source_order":0,
             "fetched_at":"2026-01-01T00:00:00Z","download":{"state":"none"},"read":false},
            {"id":"00000000-0000-0000-0000-0000000000b1",
             "manga_id":"00000000-0000-0000-0000-00000000000b",
             "source_key":"Solo Farming/Chapter 1","title":"Chapter 1","source_order":0,
             "fetched_at":"2026-01-01T00:00:00Z","download":{"state":"none"},"read":false}
        ],
        "read_chapter_ids": ["00000000-0000-0000-0000-0000000000a1"],
        "progress": [
            {"id":"00000000-0000-0000-0000-0000000000e1",
             "manga_id":"00000000-0000-0000-0000-00000000000b",
             "chapter_id":"00000000-0000-0000-0000-0000000000b1",
             "page":4,"device":"phone","at":"2026-01-02T00:00:00Z"}
        ]
    }"#;
    let backup: yomu_domain::Backup = serde_json::from_str(json).unwrap();

    let db = Db::in_memory().await.unwrap();
    let summary = db.import_backup(SHARED, &backup).await.unwrap();
    assert_eq!((summary.publications, summary.units, summary.read_marks, summary.progress_events),
               (2, 2, 1, 1));

    let local = db
        .get_publication(Uuid::parse_str("00000000-0000-0000-0000-00000000000b").unwrap())
        .await
        .unwrap();
    assert_eq!(
        local.origin,
        yomu_domain::Origin::LocalFile { path: "Solo Farming".into() }
    );
    let position = db
        .latest_position(SHARED, local.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(position.page(), 4);
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p yomu-server restore_accepts`
Expected: PASS immediately (the mirror + Task 2's import handle it). If it fails, fix `import_backup`'s origin write — do not touch the wire mirror without re-running the domain golden tests.

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-server/src/db/mod.rs
git -c commit.gpgsign=false commit -m "test(server): 1.x backup restores into the publications schema"
```

---

### Task 8: UI — kind switcher, cached per device

**Files:**
- Modify: `crates/yomu-ui/src/offline.rs`, `crates/yomu-ui/src/pages/library.rs`, `crates/yomu-web/styles.css`

- [ ] **Step 1: Kind cache in offline.rs**

Next to `theme()`/`set_theme()` (around `crates/yomu-ui/src/offline.rs:677`), using the same `storage()` helper:

```rust
const LIBRARY_KIND_KEY: &str = "yomu-library-kind";

/// The library kind this device last viewed; restored on relaunch so a
/// phone reopens straight into Comics.
pub fn library_kind() -> yomu_domain::Kind {
    match storage()
        .and_then(|s| s.get_item(LIBRARY_KIND_KEY).ok().flatten())
        .as_deref()
    {
        Some("novels") => yomu_domain::Kind::Novels,
        Some("pdf") => yomu_domain::Kind::Pdf,
        _ => yomu_domain::Kind::Comics,
    }
}

pub fn set_library_kind(kind: yomu_domain::Kind) {
    let key = match kind {
        yomu_domain::Kind::Comics => "comics",
        yomu_domain::Kind::Novels => "novels",
        yomu_domain::Kind::Pdf => "pdf",
    };
    if let Some(storage) = storage() {
        let _ = storage.set_item(LIBRARY_KIND_KEY, key);
    }
}
```

- [ ] **Step 2: The switcher in library.rs**

In `Library()`:
- Add signals after `let selected = RwSignal::new(None::<String>);`:

```rust
let selected_kind = RwSignal::new(offline::library_kind());
// A cached kind that no longer has content falls back to Comics rather
// than showing a confusing empty library.
Effect::new(move |_| {
    if let Some(Ok(entries)) = library.get() {
        let kind = selected_kind.get_untracked();
        if kind != yomu_domain::Kind::Comics
            && !entries.iter().any(|e| e.publication.kind == kind)
        {
            selected_kind.set(yomu_domain::Kind::Comics);
        }
    }
});
```

- Replace `<h2>"Library"</h2>` with:

```rust
{move || {
    let entries = library.get().and_then(|r| r.ok()).unwrap_or_default();
    view! { <KindSwitcher entries selected_kind/> }
}}
```

- Add the kind filter as the first `.filter` over `list`:

```rust
.filter(|entry| entry.publication.kind == selected_kind.get())
```

- Add at the bottom of the file:

```rust
fn kind_label(kind: yomu_domain::Kind) -> &'static str {
    match kind {
        yomu_domain::Kind::Comics => "Comics",
        yomu_domain::Kind::Novels => "Novels",
        yomu_domain::Kind::Pdf => "PDF",
    }
}

/// The page title is the kind switcher: "Comics ▾". Kinds with nothing in
/// them are hidden (Comics always shows); with one kind the title is inert.
#[component]
fn KindSwitcher(
    entries: Vec<PublicationWithLocator>,
    selected_kind: RwSignal<yomu_domain::Kind>,
) -> impl IntoView {
    use yomu_domain::Kind;
    let open = RwSignal::new(false);
    let mut kinds = vec![Kind::Comics];
    for kind in [Kind::Novels, Kind::Pdf] {
        if entries.iter().any(|e| e.publication.kind == kind) {
            kinds.push(kind);
        }
    }
    let multiple = kinds.len() > 1;
    view! {
        <div class="kind-switcher">
            <button
                class="kind-title"
                on:click=move |_| {
                    if multiple {
                        open.update(|o| *o = !*o);
                    }
                }
            >
                <h2>{move || kind_label(selected_kind.get())}</h2>
                {multiple.then(|| view! { <span class="kind-chevron">"▾"</span> })}
            </button>
            {move || {
                open.get()
                    .then(|| {
                        view! {
                            <div class="kind-menu">
                                {kinds
                                    .clone()
                                    .into_iter()
                                    .map(|kind| {
                                        view! {
                                            <button
                                                class:active=move || selected_kind.get() == kind
                                                on:click=move |_| {
                                                    selected_kind.set(kind);
                                                    offline::set_library_kind(kind);
                                                    open.set(false);
                                                }
                                            >
                                                {kind_label(kind)}
                                            </button>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
            }}
        </div>
    }
}
```

Import `PublicationWithLocator` in the `use yomu_domain::{…}` line.

- [ ] **Step 3: Styles**

Append to `crates/yomu-web/styles.css` (match the file's existing custom-property names — inspect how `.category-tabs` colors its buttons and reuse the same variables):

```css
/* Library kind switcher: the title is the control. */
.kind-switcher { position: relative; display: inline-block; }
.kind-title {
  display: inline-flex; align-items: center; gap: 0.35rem;
  background: none; border: none; padding: 0; cursor: pointer;
  color: inherit; font: inherit;
}
.kind-title h2 { margin: 0; }
.kind-chevron { font-size: 0.7em; opacity: 0.55; }
.kind-menu {
  position: absolute; top: 100%; left: 0; z-index: 30; min-width: 9rem;
  display: flex; flex-direction: column; padding: 0.25rem;
  background: var(--card, #1c1c1e); border: 1px solid var(--border, #444);
  border-radius: 10px; box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
}
.kind-menu button {
  text-align: left; padding: 0.45rem 0.6rem; border: none; border-radius: 6px;
  background: none; color: inherit; font: inherit; cursor: pointer;
}
.kind-menu button.active { background: var(--accent, #5b8def); color: #fff; }
```

- [ ] **Step 4: Verify**

Run: `just check`
Expected: PASS. Then `cd crates/yomu-web && trunk serve` (or the repo's usual dev command) and eyeball: title reads "Comics" (no chevron with one kind), selection survives a reload via localStorage.

- [ ] **Step 5: Commit**

```bash
git add -A crates/yomu-ui crates/yomu-web
git -c commit.gpgsign=false commit -m "feat(ui): library kind switcher in the title, cached per device"
```

---

### Task 9: UI — LocalFile treatment, rescan action, copy pass

**Files:**
- Modify: `crates/yomu-client/src/lib.rs`, `crates/yomu-ui/src/chapter_actions.rs`, `crates/yomu-ui/src/pages/manga.rs`, `crates/yomu-ui/src/pages/library.rs`, `crates/yomu-ui/src/pages/more.rs`, `crates/yomu-web/styles.css`

- [ ] **Step 1: Failing test — server actions disappear for LocalFile units**

In `crates/yomu-ui/src/chapter_actions.rs`, add `server_downloads: bool` to `Caps` ("false for LocalFile publications: their content *is* the server copy") and a test:

```rust
#[test]
fn local_file_publications_offer_no_server_actions() {
    // LocalFile units present as on_server (content is inherently there).
    let caps = Caps { online: true, local_tier: true, local_remove: true, server_downloads: false };
    let a = menu_actions(&[S10], caps);
    assert!(has(&a, Action::DownloadLocal), "device save must stay available");
    assert!(!has(&a, Action::RemoveServer));
    assert!(!has(&a, Action::DownloadServer) && !has(&a, Action::DownloadBoth));
}
```

Update the existing `Caps` constants in the test module with `server_downloads: true`.

- [ ] **Step 2: Run to verify failure, then implement**

Run: `cargo test -p yomu-ui chapter_actions` → FAIL (missing field). Then in `menu_actions` gate the three server actions:

```rust
if caps.online && caps.server_downloads && any_missing_server {
    out.push(Action::DownloadServer);
    if caps.local_tier {
        out.push(Action::DownloadBoth);
    }
}
if caps.online && caps.local_tier && any_server_not_local {
    out.push(Action::DownloadLocal);
}
if caps.online && caps.server_downloads && any_server {
    out.push(Action::RemoveServer);
}
```

Re-run: PASS (including the old matrix tests, updated for the new field).

- [ ] **Step 3: Publication page gating (`pages/manga.rs`)**

Where the detail is available (the `MangaDetail`-equivalent component that receives `detail`, around the auto-download button at `manga.rs:317` and the caps construction near `manga.rs:510`):

- Compute once: `let is_local = matches!(detail.publication.origin, yomu_domain::Origin::LocalFile { .. });` and `let missing = detail.publication.missing_since.is_some();`
- Wrap the Auto-download toggle button block in `(!is_local).then(|| view! { … })`.
- Where `Caps` is built for the selection menu, set `server_downloads: !is_local`; where each unit's `ChapterState` is built (`on_server: matches!(…)` at ~`manga.rs:519`), make it `on_server: is_local || matches!(c.download, DownloadState::Downloaded { .. })`.
- In the header (next to the title / status line), add:

```rust
{missing.then(|| view! {
    <span class="missing-badge" title="The file behind this entry is no longer in the books folder">
        "file missing"
    </span>
})}
```

The Refresh button stays for both origins (server side already branches).

- [ ] **Step 4: Library cards dim when missing**

In `library.rs`, on the `<a class="manga-card" …>` element add:

```rust
class:missing=entry.publication.missing_since.is_some()
```

And in `styles.css`:

```css
.manga-card.missing .cover-wrap { opacity: 0.45; }
.missing-badge {
  display: inline-block; padding: 0.1rem 0.5rem; border-radius: 999px;
  font-size: 0.75rem; background: rgba(200, 80, 80, 0.18); color: #d08080;
}
```

- [ ] **Step 5: Client rescan + More button**

`crates/yomu-client/src/lib.rs`, next to `restore`:

```rust
/// Trigger a streamer rescan of the server's books folder.
pub async fn rescan(&self) -> Result<RescanResponse> {
    let url = self.base.join("api/v1/library/rescan")?;
    let response = self.http.post(url).send().await?;
    Self::json(response).await
}
```

(Match the surrounding style: if other POSTs go through a helper like `self.post(...)`, use it; import `RescanResponse`.)

`pages/more.rs` — inside the Backup section, after the export/restore buttons `div` and before the status line, add a sibling action group and status:

```rust
let rescan_status = RwSignal::new(None::<String>);
let rescan = {
    let client = client.clone();
    move |_| {
        let client = client.clone();
        rescan_status.set(Some("Rescanning files…".into()));
        spawn_local(async move {
            match client.rescan().await {
                Ok(r) => rescan_status.set(Some(format!(
                    "Scan done: {} added, {} updated, {} missing.",
                    r.added, r.updated, r.missing
                ))),
                Err(e) => rescan_status.set(Some(format!("Rescan failed: {e}"))),
            }
        });
    }
};
```

```rust
<h3 class="shelf-title">"Files"</h3>
<p class="muted">
    "Titles dropped into the server's books folder appear in the library "
    "automatically; rescan to pick up changes right away."
</p>
<div class="backup-actions">
    <button class="button" on:click=rescan>"Rescan files"</button>
</div>
{move || {
    rescan_status
        .get()
        .map(|msg| view! { <p class="muted backup-status">{msg}</p> })
}}
```

- [ ] **Step 6: Copy pass**

`grep -rn '"[^"]*[Mm]anga[^"]*"' crates/yomu-ui/src --include=*.rs` and fix only user-visible strings (leave identifiers, routes, storage keys). Known targets:
- `library.rs:82` empty state → `"Nothing here yet — use "<a href="/search">"Search"</a>", browse the "<a href="/sources">"Sources"</a>" catalogs, or drop files into the server's books folder."`
- `library.rs:111` → `"Nothing matches these filters."`
- `more.rs:60` export status → `format!("Exported {} titles.", backup.publications.len())`
- `more.rs:104` restore status → `"Restored {} titles, {} chapters, {} read marks."` (fields are `s.publications`, `s.units`, `s.read_marks`)
- Check `pages/home.rs`, `pages/search.rs`, `pages/downloads.rs`, `pages/about.rs` hits case by case; anything reading "manga" where a local file could sit becomes "title(s)" or "publication(s)". Chapter-related reader strings may stay ("chapter" is right for comics).

- [ ] **Step 7: Verify**

Run: `cargo test -p yomu-ui && just check`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git -c commit.gpgsign=false commit -m "feat(ui): local-file publications — no server downloads, missing badges, rescan action"
```

---

### Task 10: Final verification and version bump

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace --exclude yomu-shell && just check`
Expected: PASS, zero warnings.

- [ ] **Step 2: End-to-end smoke against a real DB copy**

Copy a pre-2.0 `yomu.db` if one is at hand (or build one by running a 1.x checkout briefly); start the server against a scratch copy with `YOMU_DB_PATH`/config pointing at it, and confirm: startup migrates cleanly, `/api/v1/library` returns the old library with `"kind":"comics"`, any old local-source rows show `"source_id":"local"` + `"file_path"`, and a CBZ dropped into the books dir appears after `POST /api/v1/library/rescan`. Report the outcome honestly; skip if no old DB is available and say so.

- [ ] **Step 3: Version bump to 2.0.0**

- `Cargo.toml` **line 14** (`version = "…"` under `[workspace.package]` — NOT line 13).
- `crates/yomu-shell/tauri.conf.json` line 4.
- Run `cargo update -w`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/yomu-shell/tauri.conf.json
git -c commit.gpgsign=false commit -m "chore: version 2.0.0"
```

Release (PR into develop → develop→main → tag) follows the repo's standing flow and is done with the user, not by this plan.

---

## Self-review checklist (run after writing, before executing)

- Spec coverage: domain model ✓ (T1), streamer ✓ (T4–T6), migration ✓ (T2), UI switcher ✓ (T8), LocalFile UI ✓ (T9), frozen wire ✓ (T1 goldens), backup/restore ✓ (T2/T7), updater exclusion ✓ (T2), covers/refresh branches ✓ (T6), copy pass ✓ (T9), non-goals untouched ✓.
- Known mid-branch inconsistency: between T3 and T6 the registry still contains LocalSource while local rows already have LocalFile origins — harmless (nothing resolves "local" through the registry once T2's exclusion lands, and pages of local units break only until T6). Don't ship the branch between those tasks.
