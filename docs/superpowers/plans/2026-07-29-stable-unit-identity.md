# Stable Unit Identity & Unavailable Chapters — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** stop reporting premium chapters as failures, stop chapter ids changing when a source changes its URLs, and recover the device downloads already orphaned by that.

**Architecture:** Two independent tracks. **A** (tasks 1–4) adds an `Unavailable` download state, wire-encoded so 1.x clients still parse it, detected via an opt-in source selector. **B** (tasks 5–8) makes unit identity survive a URL change by re-keying rows in place, then recovers already-orphaned device files by content fingerprint. They touch different code and can run in parallel; only task 4 and task 8 share `crates/yomu-ui`.

**Tech Stack:** Rust workspace (yomu-domain, yomu-source, yomu-server, yomu-client, yomu-ui), sqlx/SQLite, Leptos CSR, Tauri v2 (Android).

**Spec:** `docs/superpowers/specs/2026-07-29-stable-unit-identity-design.md`

**Context that matters:** the wire is frozen at 1.x. `crates/yomu-domain/src/publication.rs` has a `mod wire` with golden tests pinning it. Never serialize a new `state` string; see task 1.

---

## File Structure

| file | responsibility | tasks |
| --- | --- | --- |
| `crates/yomu-domain/src/publication.rs` | `DownloadState::Unavailable` + its 1.x-compatible serde | 1 |
| `crates/yomu-source/src/lib.rs` | `SourceError::Unavailable` | 2 |
| `crates/yomu-source/src/selector.rs` (or wherever `[pages]` is parsed) | optional `unavailable` selector | 2 |
| `crates/yomu-server/src/db/downloads.rs` | persist/read the new state; retry-all skips it | 3 |
| `crates/yomu-server/src/downloader.rs` | map `SourceError::Unavailable` to the new state | 3 |
| `crates/yomu-server/src/updater.rs` | auto-download skips unavailable units | 3 |
| `crates/yomu-ui/src/pages/downloads.rs`, `manga.rs`, `yomu-web/styles.css` | "Not available" group, muted styling | 4 |
| `crates/yomu-server/src/db/units.rs` | re-key in place before upsert | 5 |
| `crates/yomu-server/src/api/library.rs` + `api/mod.rs` | `GET /manga/{id}/fingerprints` | 6 |
| `crates/yomu-shell/src/lib.rs` | list / fingerprint / rename device chapters | 7 |
| `crates/yomu-ui/src/offline.rs`, `pages/more.rs` | recovery action, `DeviceMark.number` | 8 |

---

### Task 1: `DownloadState::Unavailable`, wire-compatible

**Files:**
- Modify: `crates/yomu-domain/src/publication.rs`

**Why:** a premium chapter is not a failure, but 1.x clients reject an unknown `state` string, so the new state must ride inside the failed shape.

- [ ] **Step 1: Write the failing golden tests**

In the existing `mod wire` tests:

```rust
    /// A premium chapter must reach a 1.x client as a failure — that client
    /// rejects an unknown `state` — while a 2.x client sees the distinction.
    #[test]
    fn unavailable_serializes_inside_the_1x_failed_shape() {
        let state = DownloadState::Unavailable {
            at: "2026-07-29T00:00:00Z".parse().unwrap(),
            reason: "premium on this source".into(),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["state"], "failed");
        assert_eq!(json["unavailable"], true);
        assert_eq!(json["reason"], "premium on this source");
        assert_eq!(
            serde_json::from_value::<DownloadState>(json).unwrap(),
            state
        );
    }

    /// A 1.x payload has no flag and must stay an ordinary failure.
    #[test]
    fn a_1x_failure_without_the_flag_is_still_failed() {
        let json = serde_json::json!({
            "state": "failed",
            "at": "2026-07-29T00:00:00Z",
            "reason": "boom"
        });
        assert!(matches!(
            serde_json::from_value::<DownloadState>(json).unwrap(),
            DownloadState::Failed { .. }
        ));
    }
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p yomu-domain unavailable
```

Expected: no variant `Unavailable`.

- [ ] **Step 3: Add the variant and its serde**

Add to the enum:

```rust
    /// The source does not offer this chapter (premium, locked). Not a
    /// failure: nothing is broken and retrying changes nothing until the
    /// source frees it.
    Unavailable {
        at: DateTime<Utc>,
        reason: String,
    },
```

The enum is `#[serde(tag = "state", rename_all = "lowercase")]`. A new variant would emit `"state":"unavailable"`, which 1.x rejects, so `Unavailable` needs a manual mapping: give the variant `#[serde(rename = "failed")]` plus a flag field, or implement the mapping through a private mirror type the way `PublicationWire` already does in this file. Follow whichever the file's existing style supports; the tests above define the contract.

Whatever the mechanism, `Failed` and `Unavailable` must both serialize with `"state":"failed"` and be told apart on the way back in by the presence of `"unavailable":true`.

- [ ] **Step 4: Tests pass, plus the whole crate**

```bash
cargo test -p yomu-domain
```

- [ ] **Step 5: Commit**

```bash
git commit -m "domain: an unavailable chapter is not a failed one"
```

---

### Task 2: Detect it in the source layer

**Files:**
- Modify: `crates/yomu-source/src/lib.rs` (error enum)
- Modify: the selector source's `[pages]` parsing and `pages()` implementation
- Test: alongside the existing selector tests, with a fixture in `crates/yomu-source/tests/fixtures/`

**Why:** only the source definition knows what a paywall looks like on that site, and site specifics must stay out of this repo.

- [ ] **Step 1: Write the failing test**

Add a fixture `crates/yomu-source/tests/fixtures/premium_chapter.html` — a page with **no** page images and a marker element, e.g.

```html
<html><head><title>Chapter 173 - Premium</title></head>
<body><div class="premium-lock">This chapter is premium</div></body></html>
```

Then a test asserting that with `unavailable = "div.premium-lock"` configured, parsing yields `SourceError::Unavailable`, and that **without** the key the same page still yields `SourceError::Parse` (existing sources must not change behaviour).

- [ ] **Step 2: Run it, watch it fail**

```bash
cargo test -p yomu-source unavailable
```

- [ ] **Step 3: Implement**

Add the error:

```rust
    /// The source served the chapter but does not offer it (premium,
    /// locked). Distinct from `Parse`, which means our selectors are wrong.
    #[error("chapter not available from this source: {0}")]
    Unavailable(String),
```

Add an optional `unavailable: Option<String>` to the `[pages]` config, and in the page-parsing path: when no images matched **and** the configured selector matches the document, return `Unavailable` with a short reason naming what matched. Otherwise the existing `Parse` error, unchanged.

- [ ] **Step 4: Tests pass, commit**

```bash
cargo test -p yomu-source
git commit -m "source: tell a paywalled chapter apart from a broken selector"
```

---

### Task 3: Persist and honour the state server-side

**Files:**
- Modify: `crates/yomu-server/src/db/downloads.rs`, `crates/yomu-server/src/downloader.rs`, `crates/yomu-server/src/updater.rs`
- Test: `crates/yomu-server/src/db/mod.rs` tests

**Why:** the state has to survive a restart, stay out of the retry path, and stop the updater re-queueing it every sweep.

- [ ] **Step 1: Write the failing tests**

In the db tests: a unit finished with an unavailable outcome reads back as `DownloadState::Unavailable`; `retry_downloads` over that unit does **not** move it to pending; the download queue lists it.

- [ ] **Step 2: Run, watch fail, then implement**

`finish_download` currently takes `Result<u32, String>`; give it a third outcome (an enum, or `Result<u32, DownloadFailure>` where the failure carries whether it was unavailable) and write `download_state = 'unavailable'` with the reason in `download_error`. The row parser in `db/mod.rs` must map `'unavailable'` back, and reject unknown states as it already does.

Then:
- `downloader.rs`: map `SourceError::Unavailable` to that outcome.
- `retry_downloads` (`db/downloads.rs`, the `WHERE … download_state = 'failed'` query): leave `'unavailable'` alone.
- `updater.rs`: when queueing auto-downloads, skip units already `'unavailable'`.
- `download_queue`: include them, so the UI can show the group.

- [ ] **Step 3: Full suite and commit**

```bash
cargo test --workspace --exclude yomu-shell && just check
git commit -m "server: keep unavailable chapters out of the retry path"
```

---

### Task 4: Show it as a state, not a fault

**Files:**
- Modify: `crates/yomu-ui/src/pages/downloads.rs`, `crates/yomu-ui/src/pages/manga.rs`, `crates/yomu-web/styles.css`

**Why:** the user asked for this explicitly — a premium chapter should read as a different kind of thing, not as red breakage.

- [ ] **Step 1: Downloads page**

Add a group after the server queues: `Not available (n)`, listing the units with their reason, with a `Dismiss` action (reuse the existing dismiss path). It must **not** be included in `Retry all`.

- [ ] **Step 2: Chapter list**

An unavailable unit renders as an ordinary undownloadable chapter with a small muted marker (e.g. a `·` prefixed label "not available"), never the error styling.

- [ ] **Step 3: CSS**

A `.download-unavailable` rule using `--muted` for text and border. Do not use `--down` (the failure red).

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p yomu-ui && just check
git commit -m "ui: an unavailable chapter reads as a state, not an error"
```

---

### Task 5: Keep a unit's id when the source changes its URLs

**Files:**
- Modify: `crates/yomu-server/src/db/units.rs`
- Test: `crates/yomu-server/src/db/mod.rs`

**Why:** this is the actual bug. 1132 chapters across six publications were re-keyed on 2026-07-27 because `sync_units` treats the URL as the identity; every client keyed by unit id silently lost its downloads.

- [ ] **Step 1: Write the failing test — the real regression**

```rust
    /// A source can change every chapter URL at once (a slug hash changed on
    /// a real site, re-keying 1132 chapters across six publications). The
    /// chapters are the same chapters, so their ids must not move: clients
    /// key device downloads by unit id and nothing tells them otherwise.
    #[tokio::test]
    async fn a_wholesale_url_change_keeps_unit_ids() {
        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication(
                "fixture",
                &details("m1", &[("old/c1", Some(1.0)), ("old/c2", Some(2.0))]),
                false,
            )
            .await
            .unwrap();
        let before = db.list_units(publication.id).await.unwrap();
        let ids_before: Vec<_> = before.iter().map(|u| u.id).collect();
        db.finish_download(before[0].id, Ok(12)).await.unwrap();

        // The same two chapters, every key different.
        db.sync_units(
            publication.id,
            &details("m1", &[("new/c1", Some(1.0)), ("new/c2", Some(2.0))]).chapters,
        )
        .await
        .unwrap();

        let after = db.list_units(publication.id).await.unwrap();
        let ids_after: Vec<_> = after.iter().map(|u| u.id).collect();
        assert_eq!(ids_before, ids_after, "ids must survive a re-key");
        assert!(matches!(
            after[0].download.state,
            DownloadState::Downloaded { .. }
        ));
        assert_eq!(after[0].source_key, "new/c1");
    }
```

Adapt the helper names to whatever `db/mod.rs` tests already use (`details`, `sync_units`'s real signature, how `finish_download` is called).

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test -p yomu-server a_wholesale_url_change_keeps_unit_ids
```

Expected: ids differ — that is today's behaviour.

- [ ] **Step 3: Re-key in place, before the upsert**

In `sync_units`, after the listing is deduped and before the insert loop:

- read the publication's existing `(id, source_key, number, title)` rows;
- `stale` = stored keys absent from the listing; `fresh` = listing keys absent from storage;
- for each `fresh` key, find `stale` rows matching by `number` (or by `title` when `number` is `None`); if exactly one matches **and** that stale row is matched by exactly one fresh key, `UPDATE reading_units SET source_key = ? WHERE id = ?`;
- then run the existing upsert loop unchanged — the re-keyed rows now match by key and update in place, so their ids, download state, files, read marks and journal are untouched.

Ambiguous matches are left to the existing reconcile pass, which is lossless server-side.

- [ ] **Step 4: Test passes; check the neighbours still do**

```bash
cargo test -p yomu-server
```

The existing duplicate-merge tests must still pass — this task narrows what reaches that path, it does not replace it.

- [ ] **Step 5: Add the guard tests**

Ambiguity must not guess: two stale chapters with the same number and two fresh keys → no re-key, fall through to today's behaviour. A genuine duplicate (old key still in the listing alongside a new one) → still merges as before.

- [ ] **Step 6: Commit**

```bash
git commit -m "server: a chapter keeps its id when the source moves its URL"
```

---

### Task 6: Fingerprints for recovery

**Files:**
- Modify: `crates/yomu-server/src/api/library.rs` (or a new `api/fingerprints.rs`), `crates/yomu-server/src/api/mod.rs`, `crates/yomu-client/src/lib.rs`
- Test: `crates/yomu-server/src/api/mod.rs` tests

**Why:** the July 27 mapping is gone from the database, so the only thing that can still identify an orphaned directory is its content.

- [ ] **Step 1: Write the failing test**

`GET /api/v1/manga/{id}/fingerprints` returns one entry per **downloaded** unit — `{unit_id, page_count, page0_sha256}` — and omits units that are not downloaded.

- [ ] **Step 2: Implement**

Hash the first page file of each downloaded unit (`sha2` is already a dependency; the downloader uses it). Reading every first page is acceptable for a per-publication call. Add the matching `fingerprints(publication_id)` method to `yomu-client` in the house style.

- [ ] **Step 3: Verify against real data**

Against the live server on `localhost:4700`:

```bash
curl -s localhost:4700/api/v1/manga/019f3442-1719-7b71-b157-d756a30c9ce0/fingerprints | head -c 300
```

Expect ~178 entries. Report the count.

- [ ] **Step 4: Commit**

```bash
git commit -m "server: expose page fingerprints so a client can re-key"
```

---

### Task 7: Device-side commands

**Files:**
- Modify: `crates/yomu-shell/src/lib.rs`

**Why:** the app cannot see its own storage without these, and the recovery has to rename directories.

- [ ] **Step 1: Add three commands, registered in the handler**

- `device_list_chapters() -> Vec<String>` — directory names under `chapters/`, skipping `.partial-*`.
- `device_chapter_fingerprint(chapter) -> {page_count, sha256}` — count the page files, hash the lowest-numbered one. Same `checked_id` guard as the existing commands.
- `device_rename_chapter(from, to)` — refuses if `to` exists; both ids `checked_id`-guarded.

**Note:** `yomu-shell` cannot compile on this machine (no GTK) and is excluded from `just check`. Get it right by inspection and say in your report that it is unbuilt here.

- [ ] **Step 2: Commit**

```bash
git commit -m "shell: let the app inspect and re-key its stored chapters"
```

---

### Task 8: The recovery action

**Files:**
- Modify: `crates/yomu-ui/src/offline.rs`, `crates/yomu-ui/src/pages/more.rs`

- [ ] **Step 1: `DeviceMark.number`**

Add `number: Option<f64>`, serde-defaulted so existing marks keep loading, and record it when saving. It is the cheap match for a future recovery.

- [ ] **Step 2: The action**

In More, under Files: **Recover device downloads**. For each device mark whose unit id is not a current unit of its publication: fingerprint the local directory, match against that publication's server fingerprints on `(page_count, sha256)`, and on a unique match rename the directory and re-key the mark. Count matched / ambiguous / unmatched and report all three in the status line.

Leave anything ambiguous alone. Say plainly in the status text that the browser tier is not covered — pages there are keyed by URL in the service-worker cache.

- [ ] **Step 3: Unit-test the pure part**

The matching itself (given local fingerprints and server fingerprints, produce the rename set) must be a pure function with tests: unique match, ambiguous pair left alone, no match left alone.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p yomu-ui && just check
git commit -m "ui: recover device downloads orphaned by a source re-key"
```

---

### Task 9: Verify against the real library

**Files:**
- Modify: `docs/superpowers/specs/2026-07-29-stable-unit-identity-design.md` (results)

- [ ] **Step 1: Fingerprints on the live server**

Confirm each of the six affected publications returns fingerprints for its downloaded units. Report counts.

- [ ] **Step 2: Sanity-check the re-key against a copy**

Do **not** touch the production database. Copy it if it is readable, or build a fixture that reproduces the shape: a publication whose chapters all change key at once, with downloads and read marks, then confirm ids survive.

- [ ] **Step 3: Record results in the spec and commit**

---

## Notes for implementers

- **The wire is frozen.** Adding a `state` string breaks 1.x clients. Task 1 defines the encoding; nothing else may invent one.
- **`yomu-shell` does not compile here** (no GTK) and is excluded from `just check` and the test run.
- **Never write a scan-site name** into the repo, a commit message, or a PR. Use "a source"/"fixture". The live data contains real ones; do not paste them.
- **Do not touch the production database** at `/var/lib/private/yomu`. Read-only API calls against `localhost:4700` are fine.
- Commit with `git -c commit.gpgsign=false commit`; every message ends with the `Co-Authored-By:` and `Claude-Session:` footers used throughout this repo.
