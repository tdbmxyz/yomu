# Cover Count Badges Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Library covers show total / server-downloaded / device-downloaded chapter counts in a slim bottom strip; the unread badge stays as-is.

**Architecture:** One new server aggregate (`downloaded_count`) on the library list entry; device count from existing localStorage marks; pure CSS/markup strip.

**Tech Stack:** Rust — sqlx aggregate, Leptos UI, CSS.

**Spec:** `docs/superpowers/specs/2026-07-10-cover-badges-design.md`

Branch `feature/cover-badges` off develop (independent of the other two; rebase if api types moved). Standard commit trailer.

---

### Task 1: `downloaded_count` server-side

**Files:**
- Modify: `crates/yomu-server/src/db.rs` (library list query + row struct), `crates/yomu-domain/src/api.rs` (the library entry type — find it: `grep -n "unread_count" crates/yomu-domain/src/api.rs`)

- [ ] **Step 1: Failing test** (db.rs tests, reuse `details` helper)

```rust
#[tokio::test]
async fn library_list_counts_downloaded_chapters() {
    let db = Db::in_memory().await.unwrap();
    let manga = db
        .insert_manga("fixture", &details("m1", &[("c2", Some(2.0)), ("c1", Some(1.0))]), false)
        .await
        .unwrap();
    // Promote one chapter to downloaded (mirror how existing tests set
    // download state — check the downloader-related tests for the helper
    // or UPDATE directly).
    let chapters = db.list_chapters(manga.id).await.unwrap();
    db.mark_pending(&[chapters[0].id]).await.unwrap();
    // …then the same state transition the downloader uses to 'downloaded'
    // (grep "download_state = 'downloaded'" in db.rs for the setter).
    let entries = db.library_entries(SHARED).await.unwrap(); // real list fn name: check what /library calls
    let entry = entries.iter().find(|e| e.id == manga.id).unwrap();
    assert_eq!(entry.downloaded_count, 1);
    assert_eq!(entry.chapter_count, 2);
}
```

(Resolve the real function/type names first — the API layer's `/library` handler shows them; adapt the test to the existing shape, do not invent parallel types.)

- [ ] **Step 2: Implement** — add to the library entry struct:

```rust
    /// Chapters fully downloaded on the server.
    #[serde(default)]
    pub downloaded_count: u32,
```

and extend the library list SQL with the aggregate, mirroring how `unread_count`/`chapter_count` are computed today (read that query and add a parallel `COUNT(...) FILTER (WHERE download_state = 'downloaded')` or equivalent subquery in the same style).

- [ ] **Step 3: Run + commit** — `cargo test -p yomu-server` PASS; `feat(server): downloaded chapter count in the library list`.

---

### Task 2: Cover strip UI

**Files:**
- Modify: `crates/yomu-ui/src/pages/library.rs`, `crates/yomu-web/styles.css`

- [ ] **Step 1: Device counts** — in library.rs, once per page render:

```rust
let device_counts: std::collections::HashMap<uuid::Uuid, u32> = {
    let mut counts = std::collections::HashMap::new();
    for (_, mark) in offline::device_chapters() {
        *counts.entry(mark.manga).or_insert(0) += 1;
    }
    counts
};
```

- [ ] **Step 2: Strip markup** — inside the card's `.cover-wrap` (after the unread badge):

```rust
{
    let device = device_counts.get(&entry.id).copied().unwrap_or(0);
    (entry.chapter_count > 0 || entry.downloaded_count > 0 || device > 0).then(|| view! {
        <span class="count-strip">
            {(entry.chapter_count > 0).then(|| view! { <span>{entry.chapter_count}</span> })}
            {(entry.downloaded_count > 0)
                .then(|| view! { <span class="count-server">"⬇" {entry.downloaded_count}</span> })}
            {(device > 0).then(|| view! { <span class="count-device">"↓" {device}</span> })}
        </span>
    })
}
```

Use the SAME glyphs the chapter rows use for server/device download buttons (read manga.rs's buttons; the literals above are placeholders to replace with the real ones).

- [ ] **Step 3: CSS**

```css
.count-strip {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  display: flex;
  gap: 0.5rem;
  justify-content: flex-start;
  padding: 0.9rem 0.4rem 0.2rem;
  font-size: 0.7rem;
  font-weight: 600;
  color: #fff;
  background: linear-gradient(transparent, rgba(0, 0, 0, 0.75));
  border-radius: 0 0 0.5rem 0.5rem; /* match .manga-cover's radius */
}

.count-strip .count-server {
  color: var(--accent);
}

.count-strip .count-device {
  color: var(--saved);
}
```

- [ ] **Step 4: Run + commit** — wasm check + workspace tests PASS; `feat(ui): chapter count strip on library covers`.

---

### Task 3: Live verification + PR

- [ ] Scratch server, add a manga, download 2 chapters server-side (POST the download endpoint or mark via API — check routes), open the library headless, screenshot: strip shows total + server count; unread badge unchanged; a manga with zero everything shows no strip. Send screenshot. PR into develop, auto-merge, standard body/footer.
