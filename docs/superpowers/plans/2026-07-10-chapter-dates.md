# Chapter Release Dates + Sources-Tab Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scrape each chapter's release date from source listings, store it, and show it in the chapter list instead of the page count; align the source-card columns on the Sources tab.

**Architecture:** A new best-effort date parser in `yomu-source` feeds an optional `published_at` through `ChapterRef` → `chapters` table → `Chapter` → UI. Everything is `Option`: a source without a `chapter_date` selector, or unparseable text, yields `None` and never fails a sync. The UI formats the date with a pure helper (relative under a week, short absolute beyond). The Sources-tab fix is CSS-only (subgrid).

**Tech Stack:** Rust workspace — `yomu-source` (scraper, `scraper` + `chrono`), `yomu-domain` (shared types, serde), `yomu-server` (sqlx/SQLite), `yomu-ui` (Leptos), `yomu-web/styles.css`.

**Spec:** `docs/superpowers/specs/2026-07-10-chapter-dates-design.md`

**Repo-hygiene reminder:** no scan-site names in any committed file, ever (commit messages, comments, fixtures, this plan). Test fixtures use invented hosts like `example.com`. The real source definitions live in `~/.config/yomu/sources.d/` and are NEVER committed.

Branch: `feature/chapter-dates` (already exists, spec committed on it). Every commit message ends with:

```
Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
```

---

### Task 1: Date parser in yomu-source

**Files:**
- Create: `crates/yomu-source/src/dates.rs`
- Modify: `crates/yomu-source/src/lib.rs` (add `mod dates;` next to the existing `mod selector;`)
- Modify: `crates/yomu-source/Cargo.toml` (add `chrono.workspace = true` under `[dependencies]`)

- [ ] **Step 1: Write the failing tests**

Create `crates/yomu-source/src/dates.rs` with the module doc, an empty stub, and the tests:

```rust
//! Best-effort parsing of chapter release dates scraped from listings.
//! Sites print them three ways: machine-readable RFC 3339 (usually a
//! `<time datetime>` attribute), an absolute local convention like
//! "2026/05/19", or English relative phrases like "2 days ago".

use chrono::{DateTime, NaiveDate, Utc};

/// `text` is whitespace-normalized selector output. `format` is the
/// source's optional `chapter_date_format` (chrono syntax); date-only
/// formats resolve to midnight UTC. Relative phrases resolve against
/// `now`. Returns `None` rather than erroring: a missing or odd date
/// must never fail a sync.
pub(crate) fn parse_chapter_date(
    text: &str,
    format: Option<&str>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn rfc3339_with_fraction_and_zulu() {
        assert_eq!(
            parse_chapter_date("2026-07-09T21:11:00.205Z", None, now()),
            Some(at(2026, 7, 9, 21, 11, 0) + chrono::Duration::milliseconds(205)),
        );
    }

    #[test]
    fn configured_date_only_format_is_midnight_utc() {
        assert_eq!(
            parse_chapter_date("2026/05/19", Some("%Y/%m/%d"), now()),
            Some(at(2026, 5, 19, 0, 0, 0)),
        );
    }

    #[test]
    fn relative_phrases() {
        let n = now();
        assert_eq!(parse_chapter_date("just now", None, n), Some(n));
        assert_eq!(
            parse_chapter_date("42 minutes ago", None, n),
            Some(n - chrono::Duration::minutes(42)),
        );
        assert_eq!(
            parse_chapter_date("2 days ago", None, n),
            Some(n - chrono::Duration::days(2)),
        );
        assert_eq!(
            parse_chapter_date("an hour ago", None, n),
            Some(n - chrono::Duration::hours(1)),
        );
        assert_eq!(
            parse_chapter_date("3 months ago", None, n),
            Some(n - chrono::Duration::days(90)),
        );
        assert_eq!(
            parse_chapter_date("1 year ago", None, n),
            Some(n - chrono::Duration::days(365)),
        );
    }

    #[test]
    fn case_insensitive_relative() {
        assert_eq!(
            parse_chapter_date("2 Days Ago", None, now()),
            Some(now() - chrono::Duration::days(2)),
        );
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse_chapter_date("Chapter 12", None, now()), None);
        assert_eq!(parse_chapter_date("", None, now()), None);
        assert_eq!(parse_chapter_date("someday soon", None, now()), None);
        // configured format that doesn't match falls through to None
        assert_eq!(parse_chapter_date("19-05-2026", Some("%Y/%m/%d"), now()), None);
    }
}
```

Add `mod dates;` to `crates/yomu-source/src/lib.rs` and `chrono.workspace = true` to `crates/yomu-source/Cargo.toml`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p yomu-source dates:: 2>&1 | tail -5`
Expected: panics at `todo!()` (or compile error until deps added) — every `dates::` test fails.

- [ ] **Step 3: Implement the parser**

Replace the `todo!()` body:

```rust
pub(crate) fn parse_chapter_date(
    text: &str,
    format: Option<&str>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Some(fmt) = format {
        if let Ok(dt) = DateTime::parse_from_str(text, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(date) = NaiveDate::parse_from_str(text, fmt) {
            return date.and_hms_opt(0, 0, 0).map(|d| d.and_utc());
        }
    }
    relative(text, now)
}

/// "just now" / "N <unit>(s) ago" / "a(n) <unit> ago", English only —
/// what the deployed sites print. Months and years are approximate by
/// nature (the site already rounded).
fn relative(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let lower = text.to_ascii_lowercase();
    if lower == "just now" || lower == "now" {
        return Some(now);
    }
    let rest = lower.strip_suffix(" ago")?;
    let (amount, unit) = rest.split_once(' ')?;
    let n: i64 = match amount {
        "a" | "an" | "one" => 1,
        _ => amount.parse().ok()?,
    };
    let unit_seconds: i64 = match unit.trim_end_matches('s') {
        "second" | "sec" => 1,
        "minute" | "min" => 60,
        "hour" | "hr" => 3_600,
        "day" => 86_400,
        "week" => 604_800,
        "month" => 30 * 86_400,
        "year" => 365 * 86_400,
        _ => return None,
    };
    Some(now - chrono::Duration::seconds(n * unit_seconds))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p yomu-source dates:: 2>&1 | tail -5`
Expected: all `dates::` tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-source/src/dates.rs crates/yomu-source/src/lib.rs crates/yomu-source/Cargo.toml
git commit -m "feat(source): parse chapter release dates from listing text"
```

---

### Task 2: `published_at` through the domain types

**Files:**
- Modify: `crates/yomu-domain/src/source.rs` (`ChapterRef`, ~line 65)
- Modify: `crates/yomu-domain/src/library.rs` (`Chapter`, ~line 50)
- Modify: `crates/yomu-source/src/local.rs` (~line 141, `ChapterRef` literal)
- Modify: `crates/yomu-source/src/selector.rs` (~line 469, `ChapterRef` literal)
- Modify: `crates/yomu-server/src/db.rs` (test helper `ChapterRef` literal, ~line 1027, and `Chapter` construction in `TryFrom<ChapterRow>`, ~line 965)

This task is pure plumbing (no behavior yet), so the "test" is the compiler.

- [ ] **Step 1: Add the field to both types**

In `ChapterRef` (source.rs), after `scanlator`:

```rust
    /// Release date as printed by the site's listing; best-effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
```

Check the file's imports: it needs `chrono::{DateTime, Utc}` (add if absent).

In `Chapter` (library.rs), after `fetched_at`:

```rust
    /// Release date scraped from the source listing, when it prints one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
```

- [ ] **Step 2: Fix the four construction sites**

`cargo check --workspace` lists them; expected fixes:

- `crates/yomu-source/src/local.rs:141` — add `published_at: None,` (the local filesystem source has no dates; file mtime is NOT a release date, YAGNI).
- `crates/yomu-source/src/selector.rs:469` — add `published_at: None,` (wired for real in Task 3).
- `crates/yomu-server/src/db.rs` test helper (~1027) — add `published_at: None,`.
- `crates/yomu-server/src/db.rs` `TryFrom<ChapterRow>` (~965) — add `published_at: None,` with comment `// column arrives in the storage task` (replaced in Task 4).

- [ ] **Step 3: Verify workspace compiles and tests pass**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: PASS (no behavior changed).

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-domain crates/yomu-source crates/yomu-server
git commit -m "feat(domain): optional published_at on ChapterRef and Chapter"
```

---

### Task 3: `chapter_date` selector in the selector source

**Files:**
- Modify: `crates/yomu-source/src/selector.rs` — `MangaSpec` (~line 111), `CompiledSpec` (~line 232), the compile block (~line 286), the chapter loop in `parse_manga_parts` (~line 452), and the test module at the bottom of the file (find the existing fixture-based tests with `grep -n "mod tests" crates/yomu-source/src/selector.rs`).

- [ ] **Step 1: Write the failing test**

In selector.rs's existing `#[cfg(test)] mod tests`, locate how the existing tests build a `SelectorSource` from a TOML spec string and parse a fixture (read 30 lines around an existing `parse_manga` test and mirror its helpers exactly). Add:

```rust
#[test]
fn chapter_date_selector_parses_absolute_dates() {
    // Spec: mirror the existing minimal test spec, adding to [manga]:
    //   chapter_date = ".cdate"
    //   chapter_date_format = "%Y/%m/%d"
    // Fixture chapter list HTML:
    //   <ul>
    //     <li><a href="/c/2">Chapter 2</a><span class="cdate">2026/05/19</span></li>
    //     <li><a href="/c/1">Chapter 1</a><span class="cdate">not a date</span></li>
    //   </ul>
    // Parse via the same entry point the existing tests use, then:
    let chapters = details.chapters;
    assert_eq!(
        chapters[0].published_at,
        Some(chrono::Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap()),
    );
    assert_eq!(chapters[1].published_at, None); // unparseable → None, no error
}
```

(The comment block is guidance: write real spec/fixture strings in the style of the neighboring tests — invented hostnames only.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p yomu-source chapter_date 2>&1 | tail -5`
Expected: FAIL — `published_at` is `None` for chapters[0] (selector not wired) or compile error on unknown TOML key (`deny_unknown_fields`).

- [ ] **Step 3: Wire the selector**

`MangaSpec`, after `chapter_link`:

```rust
    /// Relative to chapter item; yields the chapter's release date as
    /// the site prints it. Parsed as RFC 3339, then
    /// `chapter_date_format`, then English relative phrases
    /// ("2 days ago"). Optional; unparseable text is ignored.
    #[serde(default)]
    pub chapter_date: Option<String>,
    /// chrono format string for sites printing a local absolute
    /// convention (e.g. "%Y/%m/%d").
    #[serde(default)]
    pub chapter_date_format: Option<String>,
```

`CompiledSpec`: add `chapter_date: Option<Rule>,` after `chapter_link: Rule,`.

Compile block (next to `chapter_link: Rule::parse(...)`):

```rust
            chapter_date: rule_opt(&spec.manga.chapter_date)?,
```

Chapter loop in `parse_manga_parts`, before `chapters.push`:

```rust
            let published_at = self
                .compiled
                .chapter_date
                .as_ref()
                .and_then(|r| r.extract(item))
                .and_then(|text| {
                    crate::dates::parse_chapter_date(
                        &text,
                        self.spec.manga.chapter_date_format.as_deref(),
                        chrono::Utc::now(),
                    )
                });
```

and change the literal's `published_at: None,` (from Task 2) to `published_at,`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p yomu-source 2>&1 | tail -5`
Expected: PASS, including the new `chapter_date_selector_parses_absolute_dates`.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-source/src/selector.rs
git commit -m "feat(source): optional chapter_date selector on listings"
```

---

### Task 4: Store and backfill `published_at`

**Files:**
- Create: `crates/yomu-server/migrations/0007_chapter_published_at.sql`
- Modify: `crates/yomu-server/src/db.rs` — `sync_chapters` upsert (~line 280), `insert_chapters` helper (~line 820), `ChapterRow` (~line 928), `TryFrom<ChapterRow>` (~line 973), tests at the bottom.

- [ ] **Step 1: Write the migration**

`crates/yomu-server/migrations/0007_chapter_published_at.sql`:

```sql
-- Release date as printed by the source listing; best-effort, NULL when
-- the source doesn't expose one.
ALTER TABLE chapters ADD COLUMN published_at TEXT;
```

- [ ] **Step 2: Write the failing test**

In db.rs's test module, next to the existing sync tests (reuse their setup helpers — look at `reuploaded_series_merges_twins_instead_of_duplicating` for the pattern of building a DB and calling `sync_chapters` with the `details(...)`-style helper). The test helper from Task 2 sets `published_at: None`; give the helper's callers a way to set dates — simplest is a second helper or mutating the built `ChapterRef`s in the test:

```rust
#[tokio::test]
async fn published_at_backfills_and_never_clears() {
    // setup: db + manga, mirror neighboring tests
    let day = |d: u32| chrono::Utc.with_ymd_and_hms(2026, 7, d, 0, 0, 0).unwrap();

    // 1. First sync without dates → rows have NULL published_at.
    let mut listing = /* two ChapterRefs "c/1", "c/2" via the existing helper */;
    db.sync_chapters(manga_id, &listing).await.unwrap();

    // 2. Source starts printing dates → same keys re-synced with Some(..)
    //    backfill the existing rows.
    listing[0].published_at = Some(day(1));
    listing[1].published_at = Some(day(2));
    db.sync_chapters(manga_id, &listing).await.unwrap();
    let chapters = db.chapters(manga_id).await.unwrap(); // use the real list-accessor name
    assert_eq!(chapters.iter().filter(|c| c.published_at.is_some()).count(), 2);

    // 3. Source stops printing dates → None must NOT clear stored values.
    listing[0].published_at = None;
    listing[1].published_at = None;
    db.sync_chapters(manga_id, &listing).await.unwrap();
    let chapters = db.chapters(manga_id).await.unwrap();
    assert_eq!(chapters.iter().filter(|c| c.published_at.is_some()).count(), 2);

    // 4. A changed date wins (site-side correction).
    listing[0].published_at = Some(day(5));
    db.sync_chapters(manga_id, &listing).await.unwrap();
    let chapters = db.chapters(manga_id).await.unwrap();
    let c1 = chapters.iter().find(|c| c.source_key.ends_with("c/1")).unwrap();
    assert_eq!(c1.published_at, Some(day(5)));
}
```

(Adapt setup lines and accessor names to the neighboring tests — do not invent new helpers if existing ones fit.)

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p yomu-server published_at 2>&1 | tail -5`
Expected: FAIL at step-2 assertion (count is 0 — column not written yet).

- [ ] **Step 4: Implement storage**

`sync_chapters` upsert becomes:

```rust
            sqlx::query(
                "INSERT INTO chapters (id, manga_id, source_key, title, number, source_order,
                                       scanlator, fetched_at, published_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (manga_id, source_key)
                 DO UPDATE SET title = excluded.title, number = excluded.number,
                               source_order = excluded.source_order,
                               published_at = COALESCE(excluded.published_at,
                                                       chapters.published_at)",
            )
```

with `.bind(chapter.published_at)` added after `.bind(now)`.

`insert_chapters` (the add-manga path, `DO NOTHING`): same column/placeholder/bind addition, no COALESCE needed.

`ChapterRow`: add `published_at: Option<DateTime<Utc>>,` after `fetched_at`. `TryFrom<ChapterRow>`: replace the Task-2 placeholder with `published_at: row.published_at,`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p yomu-server 2>&1 | tail -5`
Expected: PASS, including all pre-existing sync/merge tests.

- [ ] **Step 6: Commit**

```bash
git add crates/yomu-server/migrations/0007_chapter_published_at.sql crates/yomu-server/src/db.rs
git commit -m "feat(server): persist chapter published_at, backfill on sync"
```

---

### Task 5: Show the date in the chapter list

**Files:**
- Create: `crates/yomu-ui/src/format.rs`
- Modify: `crates/yomu-ui/src/lib.rs` (add `pub mod format;` next to `pub mod offline;` — match the existing module list style)
- Modify: `crates/yomu-ui/src/pages/manga.rs` (~line 616, the `"13 p."` span)
- Modify: `crates/yomu-web/styles.css` (~line 711, `.chapter-pages`)

- [ ] **Step 1: Write the failing tests**

`crates/yomu-ui/src/format.rs`:

```rust
//! Human formatting helpers shared by pages.

use chrono::{DateTime, Datelike, Utc};

/// Chapter release date, compact: relative under a week ("5 h. ago"),
/// short absolute beyond ("May 19", year appended when it isn't
/// `now`'s). Future dates (clock skew, site rounding) read "just now".
pub fn published_label(published: DateTime<Utc>, now: DateTime<Utc>) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
    }

    #[test]
    fn relative_tiers() {
        let n = now();
        assert_eq!(published_label(n - chrono::Duration::seconds(30), n), "just now");
        assert_eq!(published_label(n - chrono::Duration::minutes(42), n), "42 min. ago");
        assert_eq!(published_label(n - chrono::Duration::hours(5), n), "5 h. ago");
        assert_eq!(published_label(n - chrono::Duration::days(3), n), "3 d. ago");
    }

    #[test]
    fn absolute_beyond_a_week() {
        let n = now();
        assert_eq!(published_label(Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap(), n), "May 19");
        assert_eq!(
            published_label(Utc.with_ymd_and_hms(2025, 5, 19, 0, 0, 0).unwrap(), n),
            "May 19, 2025",
        );
    }

    #[test]
    fn future_reads_just_now() {
        let n = now();
        assert_eq!(published_label(n + chrono::Duration::hours(2), n), "just now");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p yomu-ui format:: 2>&1 | tail -5`
Expected: panics at `todo!()`.

- [ ] **Step 3: Implement the helper**

```rust
pub fn published_label(published: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let age = now.signed_duration_since(published);
    let mins = age.num_minutes();
    if mins < 1 {
        return "just now".into();
    }
    if mins < 60 {
        return format!("{mins} min. ago");
    }
    if age.num_hours() < 24 {
        return format!("{} h. ago", age.num_hours());
    }
    if age.num_days() < 7 {
        return format!("{} d. ago", age.num_days());
    }
    if published.year() == now.year() {
        published.format("%b %-d").to_string()
    } else {
        published.format("%b %-d, %Y").to_string()
    }
}
```

Run: `cargo test -p yomu-ui format:: 2>&1 | tail -5` — expected: PASS.

- [ ] **Step 4: Swap the chapter-row annotation**

In `pages/manga.rs`, replace

```rust
            {chapter
                .page_count
                .map(|c| view! { <span class="muted chapter-pages">{c} " p."</span> })}
```

with

```rust
            {chapter
                .published_at
                .map(|at| {
                    view! {
                        <span class="muted chapter-date">
                            {crate::format::published_label(at, chrono::Utc::now())}
                        </span>
                    }
                })}
```

(Check manga.rs's existing imports; it already uses chrono types elsewhere or the fully-qualified path above suffices.)

In `styles.css`, rename the selector `.chapter-pages` → `.chapter-date` (rule body unchanged: `white-space: nowrap; flex-shrink: 0;`).

- [ ] **Step 5: Verify workspace builds and tests pass**

Run: `cargo test --workspace 2>&1 | tail -5` and `cargo check -p yomu-ui --target wasm32-unknown-unknown 2>&1 | tail -3`
Expected: PASS / no errors. (If the wasm target check fails for pre-existing reasons unrelated to this change, note it and move on — the web build in Task 7 is the real gate.)

- [ ] **Step 6: Commit**

```bash
git add crates/yomu-ui crates/yomu-web/styles.css
git commit -m "feat(ui): show chapter release date instead of page count"
```

---

### Task 6: Sources-tab column alignment (CSS only)

**Files:**
- Modify: `crates/yomu-web/styles.css:521-537` (`.source-list`, `.source-card`)

- [ ] **Step 1: Apply the grid**

Replace the two rules:

```css
.source-list {
  display: grid;
  /* name | host | spacer | listing labels — subgrid on the cards makes
     the columns line up across all of them */
  grid-template-columns: max-content max-content 1fr max-content;
  gap: 0.6rem;
}

.source-card {
  display: grid;
  grid-template-columns: subgrid;
  grid-column: 1 / -1;
  align-items: baseline;
  column-gap: 0.75rem;
  padding: 0.8rem 1rem;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  color: inherit;
  text-decoration: none;
}
```

(The card's four children — name, host, `.grow` spacer, sorts — map onto the four tracks; no Rust change. `gap` on `.source-list` keeps the vertical rhythm between cards; `column-gap` on the card replaces the old flex `gap`.)

- [ ] **Step 2: Commit**

```bash
git add crates/yomu-web/styles.css
git commit -m "fix(web): align source-card columns across the sources list"
```

---

### Task 7: End-to-end verification against the web build

This is the runtime gate for both features (per the verify discipline: drive the affected flow, don't stop at unit tests).

- [ ] **Step 1: Build and run the server with real source definitions**

Check `justfile` for the dev/build recipe (`grep -n "" justfile | head -30`) and use it if one exists; otherwise:

```bash
just build-web 2>/dev/null || (cd crates/yomu-web && trunk build)
YOMU_CONFIG=<scratchpad>/verify.toml cargo run -p yomu-server &
```

with `<scratchpad>/verify.toml` pointing `sources_dir` at `~/.config/yomu/sources.d/`, `db_path`/`data_dir` inside the scratchpad, and a free port (e.g. 4791). NOTE: the sources under `~/.config` are the real, uncommitted definitions — fine to *use*, never to copy into the repo.

- [ ] **Step 2: Add `chapter_date` to the staged definitions (NOT the repo)**

Edit the three files in `~/.config/yomu/sources.d/` — these edits are deliberately not written down here because this plan is committed; the mapping:

- the definition whose chapter rows print **relative phrases** (its `chapter_item` is the `<a>` row itself): `chapter_date = "div.flex-shrink-0 span"`
- the definition with `.chapterdate` **absolute text**: `chapter_date = ".chapterdate"` and `chapter_date_format = "%Y/%m/%d"`
- the definition whose rows carry a **`<time>` element**: `chapter_date = "time@datetime"`

Restart the verify server after editing.

- [ ] **Step 3: Drive the flow**

For each of the three sources: add (or fetch details for) one manga through the API and confirm chapters carry `published_at`:

```bash
curl -s localhost:4791/api/v1/... # search/browse → add → GET /api/v1/manga/{id}
```

Expected: `"published_at":"20..."` on chapters from all three sources; sensible values (recent chapters ≈ today).

Then load the web UI in a browser (or curl the page + check via the API) and confirm the chapter list shows dates, not page counts. Screenshot if a display is available (see memory: grim under Hyprland works on this box).

- [ ] **Step 4: Sources tab visual check**

Open `/sources` in the web UI: name / host / listing-label columns aligned across cards. Screenshot for the user.

- [ ] **Step 5: Clean up**

Kill the verify server; delete the scratchpad db/config. Leave the edited staged TOMLs in place (they're the deployment payload).

---

### Task 8: PR

- [ ] **Step 1: Final full run**

Run: `cargo test --workspace 2>&1 | tail -5` and `cargo clippy --workspace 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 2: Push and open the PR into develop**

```bash
git push -u origin feature/chapter-dates
gh pr create --base develop --title "feat: chapter release dates + sources-tab alignment" --body "..."
```

Body: summary of both features, the never-clear/overwrite semantics, the rollout note (needs web + desktop + APK builds; staged source definitions gain a date selector). End with the standard "🤖 Generated with Claude Code" line and the session URL. No site names in the body.

---

## Self-review notes

- Spec coverage: parser (T1), domain (T2), selector (T3), DB + backfill (T4), UI (T5), CSS alignment (T6), live verification + TOML staging (T7), rollout starts at T8's PR. Release/tag/APK happens after merge, outside this plan (same pipeline as 1.3.1 plus `just apk` — check the justfile / release workflow when cutting it).
- The `read: false` / per-user fields pattern in `TryFrom<ChapterRow>` is untouched.
- Type names consistent: `published_at: Option<DateTime<Utc>>` everywhere; UI helper is `format::published_label`.
