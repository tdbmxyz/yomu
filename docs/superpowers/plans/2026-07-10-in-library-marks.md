# In-Library Marks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browse/search results show when a title is already tracked, and open it instead of re-adding.

**Architecture:** Serde-optional `in_library: Option<Uuid>` on `MangaSummary`, filled by the API layer from one per-response library lookup; `SummaryCard` branches on it.

**Tech Stack:** Rust — axum/sqlx server side, Leptos UI.

**Spec:** `docs/superpowers/specs/2026-07-10-in-library-marks-design.md`

Branch `feature/in-library-marks` off develop AFTER the catalog-cache PR merges (both touch api/sources.rs). Standard commit trailer.

---

### Task 1: Domain field + db lookup

**Files:**
- Modify: `crates/yomu-domain/src/source.rs` (`MangaSummary`), `crates/yomu-server/src/db.rs`

- [ ] **Step 1: Field**

```rust
    /// Set by the server when this result is already tracked: the
    /// library manga id. Sources never fill it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<Uuid>,
```

(`use uuid::Uuid;` — check the file's imports; add `uuid.workspace = true` to yomu-domain if absent — it already depends on uuid for Manga, verify.) Fix construction sites workspace-wide with `in_library: None` (`cargo check --workspace --exclude yomu-shell` lists them: sources, tests, catalog read path).

- [ ] **Step 2: Failing db test**

```rust
#[tokio::test]
async fn library_keys_maps_source_key_to_id() {
    let db = Db::in_memory().await.unwrap();
    let manga = db
        .insert_manga("fixture", &details("m1", &[("c1", Some(1.0))]), false)
        .await
        .unwrap();
    let map = db.library_keys("fixture").await.unwrap();
    assert_eq!(map.get("m1"), Some(&manga.id));
    assert!(db.library_keys("other-source").await.unwrap().is_empty());
}
```

- [ ] **Step 3: Implement**

```rust
    /// source_key → manga id for one source; the browse/search
    /// annotation ("already in library").
    pub async fn library_keys(
        &self,
        source_id: &str,
    ) -> Result<std::collections::HashMap<String, Uuid>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT source_key, id FROM manga WHERE source_id = ?",
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(key, id)| Ok((key, parse_uuid(id)?)))
            .collect()
    }
```

- [ ] **Step 4: Run + commit** — `cargo test -p yomu-server library_keys` → PASS; commit `feat(domain,server): in_library annotation groundwork`.

---

### Task 2: Annotate in the API layer

**Files:**
- Modify: `crates/yomu-server/src/api/sources.rs`

- [ ] **Step 1: Helper + wiring**

```rust
/// Mark results that are already tracked (matched on the exact
/// source_key the add flow stores).
async fn annotate_in_library(
    state: &AppState,
    source_id: &str,
    items: &mut [MangaSummary],
) {
    match state.db.library_keys(source_id).await {
        Ok(keys) => {
            for item in items {
                item.in_library = keys.get(&item.key).copied();
            }
        }
        // Annotation is decoration; never fail the listing for it.
        Err(err) => tracing::warn!(%err, "in-library annotation failed"),
    }
}
```

Call it in `search`, `search_all` (per source group) and in ALL THREE branches of `browse` (cached fresh, cached revalidate, live) just before building the `Json` response.

- [ ] **Step 2: Test** — extend the db-level test if no HTTP harness exists (same decision as the catalog plan): insert a manga, build `vec![MangaSummary { key: <its key>, .. }, MangaSummary { key: "other", .. }]`, run the annotation logic's core (the `keys.get` mapping) via `library_keys` — already covered by Task 1's test; the handler wiring is verified live in Task 4.

- [ ] **Step 3: Run + commit** — full `cargo test -p yomu-server` → PASS; commit `feat(server): annotate browse/search results already in the library`.

---

### Task 3: UI

**Files:**
- Modify: `crates/yomu-ui/src/pages/search.rs` (`SummaryCard`), `crates/yomu-web/styles.css`

- [ ] **Step 1: Card branch**

In `SummaryCard`, read `hit.in_library` before `hit` is moved. When `Some(id)`:
- render `<span class="in-library-badge">"✓"</span>` inside the card's `.cover-wrap` (match the card's actual DOM — read the component first),
- replace the add/track buttons with `<a class="button" href=format!("/manga/{id}")>"Open"</a>`.

CSS (next to `.unread-badge`):

```css
.in-library-badge {
  position: absolute;
  top: 0.3rem;
  left: 0.3rem;
  background: var(--accent);
  color: var(--accent-contrast);
  font-size: 0.7rem;
  font-weight: 700;
  padding: 0.05rem 0.35rem;
  border-radius: 0.3rem;
}
```

(Top-LEFT: the unread badge convention owns the top-right corner.)

- [ ] **Step 2: Run + commit** — `cargo check -p yomu-ui --target wasm32-unknown-unknown` + workspace tests → PASS; commit `feat(ui): mark and open already-tracked titles in browse/search`.

---

### Task 4: Live verification + PR

- [ ] Scratch server + headless: add a manga, search its title → card shows ✓ and "Open" navigates to the manga page; browse the source's popular page → same when listed. Screenshot to the user. PR into develop, auto-merge, standard body/footer.
