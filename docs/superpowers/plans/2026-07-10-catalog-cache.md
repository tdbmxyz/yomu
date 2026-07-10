# Catalog Cache + Cover Proxy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve source catalog pages from a persistent server-side cache (stale-while-revalidate) and proxy result covers so clients never hit the scan sites directly.

**Architecture:** Two SQLite tables (`catalog_entries`, `catalog_pages`) written by every browse/search response; the browse endpoint serves cached pages and revalidates stale ones in the background (single-flight); a `/covers?src=` endpoint disk-caches images fetched through the owning source's rate-limited client; the API layer rewrites `cover_url` to the proxy.

**Tech Stack:** Rust — axum, sqlx/SQLite, tokio; pure-function staleness logic.

**Spec:** `docs/superpowers/specs/2026-07-10-catalog-cache-design.md`

Branch `feature/catalog-cache` (specs already on it). Standard commit trailer.

---

### Task 1: Migration + catalog storage in db.rs

**Files:**
- Create: `crates/yomu-server/migrations/0008_catalog.sql`
- Modify: `crates/yomu-server/src/db.rs` (new section + tests)

- [ ] **Step 1: Migration**

```sql
-- Source catalog cache: every summary the server has seen, plus the
-- composition of each browse page (stale-while-revalidate reads).
CREATE TABLE catalog_entries (
    source_id    TEXT NOT NULL,
    key          TEXT NOT NULL,
    title        TEXT NOT NULL,
    cover_url    TEXT,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (source_id, key)
);
CREATE INDEX catalog_entries_cover ON catalog_entries (cover_url);
CREATE TABLE catalog_pages (
    source_id  TEXT NOT NULL,
    sort       TEXT NOT NULL,
    page       INTEGER NOT NULL,
    keys       TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (source_id, sort, page)
);
```

- [ ] **Step 2: Failing tests in db.rs's test module**

```rust
#[tokio::test]
async fn catalog_upsert_and_page_roundtrip() {
    let db = Db::in_memory().await.unwrap();
    let sum = |k: &str, t: &str| MangaSummary {
        key: k.into(),
        title: t.into(),
        cover_url: Some(format!("https://c.example/{k}.jpg").parse().unwrap()),
        in_library: None, // field arrives in the in-library feature; omit this line until then
    };
    let now = Utc::now();
    db.upsert_catalog_entries("src", &[sum("a", "A"), sum("b", "B")], now)
        .await
        .unwrap();
    // Unchanged rows keep last_seen_at fresh; changed titles win.
    db.upsert_catalog_entries("src", &[sum("a", "A2")], now).await.unwrap();
    db.write_catalog_page("src", "popular", 1, &["a".into(), "b".into()], now)
        .await
        .unwrap();
    let (items, fetched_at) = db
        .read_catalog_page("src", "popular", 1)
        .await
        .unwrap()
        .expect("cached page");
    assert_eq!(fetched_at, now);
    assert_eq!(
        items.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["A2", "B"],
    );
    // Unknown page → None.
    assert!(db.read_catalog_page("src", "latest", 1).await.unwrap().is_none());
    // Cover ownership lookup for the proxy.
    assert_eq!(
        db.catalog_source_for_cover("https://c.example/a.jpg").await.unwrap(),
        Some("src".to_string()),
    );
    assert_eq!(db.catalog_source_for_cover("https://evil.example/x").await.unwrap(), None);
}
```

(If `MangaSummary` doesn't yet have `in_library`, construct without it — this plan is independent of the marks feature.)

- [ ] **Step 3: Run to verify failure** — `cargo test -p yomu-server catalog 2>&1 | tail -3` → compile error (methods missing).

- [ ] **Step 4: Implement in db.rs (new `// ---- catalog ----` section on `impl Db`)**

```rust
    /// Record summaries seen in a listing/search; the diff update keeps
    /// writes cheap for unchanged rows.
    pub async fn upsert_catalog_entries(
        &self,
        source_id: &str,
        items: &[MangaSummary],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for item in items {
            sqlx::query(
                "INSERT INTO catalog_entries (source_id, key, title, cover_url, last_seen_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (source_id, key) DO UPDATE SET
                     title = excluded.title,
                     cover_url = excluded.cover_url,
                     last_seen_at = excluded.last_seen_at",
            )
            .bind(source_id)
            .bind(&item.key)
            .bind(&item.title)
            .bind(item.cover_url.as_ref().map(url::Url::as_str))
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn write_catalog_page(
        &self,
        source_id: &str,
        sort: &str,
        page: u32,
        keys: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO catalog_pages (source_id, sort, page, keys, fetched_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (source_id, sort, page) DO UPDATE SET
                 keys = excluded.keys, fetched_at = excluded.fetched_at",
        )
        .bind(source_id)
        .bind(sort)
        .bind(page)
        .bind(serde_json::to_string(keys).expect("string list serializes"))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A cached browse page, in listing order, with its fetch time.
    pub async fn read_catalog_page(
        &self,
        source_id: &str,
        sort: &str,
        page: u32,
    ) -> Result<Option<(Vec<MangaSummary>, DateTime<Utc>)>> {
        let Some((keys, fetched_at)) = sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT keys, fetched_at FROM catalog_pages
             WHERE source_id = ? AND sort = ? AND page = ?",
        )
        .bind(source_id)
        .bind(sort)
        .bind(page)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let keys: Vec<String> =
            serde_json::from_str(&keys).map_err(|e| DbError::Corrupt(e.to_string()))?;
        let mut items = Vec::with_capacity(keys.len());
        for key in &keys {
            let row = sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT title, cover_url FROM catalog_entries
                 WHERE source_id = ? AND key = ?",
            )
            .bind(source_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
            if let Some((title, cover_url)) = row {
                items.push(MangaSummary {
                    key: key.clone(),
                    title,
                    cover_url: cover_url.and_then(|c| c.parse().ok()),
                });
            }
        }
        Ok(Some((items, fetched_at)))
    }

    /// Which source a cover URL belongs to — gate for the cover proxy
    /// (the server must not fetch arbitrary URLs).
    pub async fn catalog_source_for_cover(&self, cover_url: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT source_id FROM catalog_entries WHERE cover_url = ? LIMIT 1",
        )
        .bind(cover_url)
        .fetch_optional(&self.pool)
        .await?)
    }
```

(Adjust the `MangaSummary` literal if the in-library field already exists on this branch.)

- [ ] **Step 5: Run** — `cargo test -p yomu-server 2>&1 | tail -3` → PASS.

- [ ] **Step 6: Commit** — `feat(server): catalog storage for source listings`

---

### Task 2: Cache plan + config

**Files:**
- Create: `crates/yomu-server/src/catalog.rs`
- Modify: `crates/yomu-server/src/main.rs` (`mod catalog;`), `crates/yomu-server/src/config.rs`, `crates/yomu-server/yomu.example.toml`

- [ ] **Step 1: Config** — add to config.rs (with `catalog: CatalogConfig::default()` in `Default for Config` and the field on `Config`):

```rust
/// Source catalog cache (Sources tab listings).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    /// Cached browse pages older than this revalidate in the background
    /// on access; 0 disables cached reads (listings always live).
    pub ttl_secs: u64,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self { ttl_secs: 6 * 60 * 60 }
    }
}
```

Example config addition:

```toml
# Sources-tab catalog cache: cached listing pages are served instantly
# and refreshed in the background once older than ttl_secs (0 = always
# fetch live).
#[catalog]
#ttl_secs = 21600
```

- [ ] **Step 2: catalog.rs with failing CachePlan tests**

```rust
//! Stale-while-revalidate policy for cached browse pages, plus the
//! single-flight guard for background revalidations.

use std::collections::HashSet;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

/// What the browse endpoint should do for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePlan {
    /// Serve the cached page, done.
    Fresh,
    /// Serve the cached page and refresh it in the background.
    Revalidate,
    /// Nothing usable cached: fetch live before answering.
    Live,
}

impl CachePlan {
    pub fn decide(cached_at: Option<DateTime<Utc>>, ttl_secs: u64, now: DateTime<Utc>) -> Self {
        let Some(at) = cached_at else { return CachePlan::Live };
        if ttl_secs == 0 {
            return CachePlan::Live;
        }
        if (now - at).num_seconds() as u64 <= ttl_secs {
            CachePlan::Fresh
        } else {
            CachePlan::Revalidate
        }
    }
}

/// Guards against a stampede of identical background revalidations.
#[derive(Default)]
pub struct Inflight(Mutex<HashSet<String>>);

impl Inflight {
    /// True when the caller acquired the slot (must call `finish`).
    pub fn start(&self, key: &str) -> bool {
        self.0.lock().expect("inflight lock").insert(key.to_string())
    }
    pub fn finish(&self, key: &str) {
        self.0.lock().expect("inflight lock").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cache_plan_covers_all_states() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let fresh = now - chrono::Duration::minutes(5);
        let stale = now - chrono::Duration::hours(7);
        assert_eq!(CachePlan::decide(None, 3600, now), CachePlan::Live);
        assert_eq!(CachePlan::decide(Some(fresh), 21600, now), CachePlan::Fresh);
        assert_eq!(CachePlan::decide(Some(stale), 21600, now), CachePlan::Revalidate);
        // ttl 0 = caching off, even with a cached page
        assert_eq!(CachePlan::decide(Some(fresh), 0, now), CachePlan::Live);
    }

    #[test]
    fn inflight_is_single_entry() {
        let guard = Inflight::default();
        assert!(guard.start("k"));
        assert!(!guard.start("k"));
        guard.finish("k");
        assert!(guard.start("k"));
    }
}
```

- [ ] **Step 3: Run** — `cargo test -p yomu-server catalog:: 2>&1 | tail -3` (add `mod catalog;` to main.rs) → PASS (logic and tests land together; both tests are pure).

- [ ] **Step 4: Commit** — `feat(server): catalog cache policy and config`

---

### Task 3: Browse endpoint serves the cache; search feeds it

**Files:**
- Modify: `crates/yomu-server/src/api/sources.rs`, `crates/yomu-server/src/state.rs` (add `pub catalog_inflight: Arc<crate::catalog::Inflight>` initialized in `AppState::new`)

- [ ] **Step 1: Rewrite `browse`**

```rust
pub async fn browse(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<Vec<MangaSummary>>, ApiError> {
    let source = state.sources.get(&id).ok_or(ApiError::NotFound)?;
    let sort_key = query.sort.key();
    let now = chrono::Utc::now();
    let cached = state.db.read_catalog_page(&id, sort_key, query.page).await?;
    let plan = crate::catalog::CachePlan::decide(
        cached.as_ref().map(|(_, at)| *at),
        state.config.catalog.ttl_secs,
        now,
    );
    match plan {
        crate::catalog::CachePlan::Fresh => {
            let (items, _) = cached.expect("Fresh implies cached");
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Revalidate => {
            let (items, _) = cached.expect("Revalidate implies cached");
            let flight_key = format!("{id}/{sort_key}/{}", query.page);
            if state.catalog_inflight.start(&flight_key) {
                let state = state.clone();
                let source = source.clone();
                let page = query.page;
                let sort = query.sort;
                tokio::spawn(async move {
                    match source.browse(sort, page).await {
                        Ok(fresh) => {
                            let _ = store_page(&state, source.id(), sort.key(), page, &fresh).await;
                        }
                        // Stale page stays; it self-heals when the source answers.
                        Err(err) => tracing::warn!(source = source.id(), %err, "catalog revalidation failed"),
                    }
                    state.catalog_inflight.finish(&flight_key);
                });
            }
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Live => {
            let items = source.browse(query.sort, query.page).await?;
            store_page(&state, &id, sort_key, query.page, &items).await?;
            Ok(Json(items))
        }
    }
}

/// Upsert entries then record the page composition.
async fn store_page(
    state: &AppState,
    source_id: &str,
    sort: &str,
    page: u32,
    items: &[MangaSummary],
) -> Result<(), crate::db::DbError> {
    let now = chrono::Utc::now();
    state.db.upsert_catalog_entries(source_id, items, now).await?;
    let keys: Vec<String> = items.iter().map(|s| s.key.clone()).collect();
    state.db.write_catalog_page(source_id, sort, page, &keys, now).await
}
```

`search` and `search_all`: after obtaining `results`, call
`state.db.upsert_catalog_entries(&id, &results, Utc::now()).await` (log-and-continue on error — caching must not fail a search: `if let Err(err) = … { tracing::warn!(%err, "catalog upsert failed") }`).

- [ ] **Step 2: Endpoint test with a stub source**

Look at how existing server tests construct an `AppState`/router (`grep -rn "fn test_state\|Router" crates/yomu-server/src --include=*.rs | grep -i test`). If no HTTP-level test harness exists, test at the db+plan level instead and verify the endpoint live in Task 5 — do NOT build a new test harness for this plan.

- [ ] **Step 3: Run** — `cargo test -p yomu-server 2>&1 | tail -3` → PASS.

- [ ] **Step 4: Commit** — `feat(server): browse serves the catalog cache, stale-while-revalidate`

---

### Task 4: Cover proxy + response rewrite

**Files:**
- Modify: `crates/yomu-server/src/api/sources.rs` (new `covers` handler + rewrite helper), `crates/yomu-server/src/api/mod.rs` (route `/covers`), `crates/yomu-server/src/api/library.rs` (share `cover_response` — make it `pub(crate)`)

- [ ] **Step 1: Handler**

```rust
#[derive(Deserialize)]
pub struct CoverQuery {
    src: url::Url,
}

/// Proxied, disk-cached catalog cover. Only covers the catalog (or the
/// library) knows about are fetched — this is not an open proxy.
pub async fn cover(
    State(state): State<AppState>,
    Query(query): Query<CoverQuery>,
) -> Result<Response, ApiError> {
    use sha2::{Digest, Sha256};
    let url_str = query.src.as_str();
    let hash = format!("{:x}", Sha256::digest(url_str.as_bytes()));
    let dir = state.config.data_dir.join("covers/by-url");

    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let path = dir.join(format!("{hash}.{ext}"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(crate::api::library::cover_response(
                bytes,
                crate::downloader::content_type_for(&path),
            ));
        }
    }

    let source_id = state
        .db
        .catalog_source_for_cover(url_str)
        .await?
        .ok_or(ApiError::NotFound)?;
    let source = state
        .sources
        .get(&source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
    let image = source.image(&query.src).await?;
    let ext = crate::downloader::extension_for(&image.content_type, url_str);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(format!("{hash}.{ext}"));
    let _ = tokio::fs::write(&path, &image.bytes).await;
    Ok(crate::api::library::cover_response(
        image.bytes.to_vec(),
        crate::downloader::content_type_for(&path),
    ))
}
```

Check Cargo.toml for `sha2` (`grep sha2 Cargo.toml crates/yomu-server/Cargo.toml`); add `sha2.workspace = true` (and to the workspace list if absent) if missing. Check the exact signature of `downloader::extension_for` before use and adapt.

Route in api/mod.rs next to the sources routes: `.route("/covers", get(sources::cover))`.

- [ ] **Step 2: Rewrite covers in responses**

In api/sources.rs, applied to search/search_all/browse results (cached and live paths), after annotation/upserts:

```rust
/// Point result covers at the proxy so clients never touch the site CDN.
fn proxy_covers(items: &mut [MangaSummary]) {
    for item in items {
        if let Some(cover) = item.cover_url.take() {
            let proxied = format!(
                "/api/v1/covers?src={}",
                percent_encoding::utf8_percent_encode(
                    cover.as_str(),
                    percent_encoding::NON_ALPHANUMERIC,
                ),
            );
            // Relative URL: parse against a dummy base only if MangaSummary
            // requires an absolute Url — check the type. If cover_url is
            // url::Url (absolute-only), keep the field absolute by joining
            // onto a placeholder is WRONG for clients; instead change the
            // API-facing type: introduce `cover: Option<String>` at the API
            // layer OR relax MangaSummary.cover_url to Option<String>.
            // DECISION: relax MangaSummary.cover_url to Option<String>
            // (domain change, sources adapt their construction) — one type,
            // no parallel API struct.
            item.cover_url = Some(proxied);
        }
    }
}
```

NOTE (design decision locked here): `MangaSummary.cover_url` becomes `Option<String>` in yomu-domain. Sources construct it from their parsed `Url` via `.to_string()`; the UI uses it as an `<img src>` string already. Compile errors from the type change point at every construction/usage site — fix them all (`cargo check --workspace --exclude yomu-shell`). The upsert/read code from Task 1 simplifies (no re-parse).

- [ ] **Step 3: Run everything** — `cargo test --workspace --exclude yomu-shell 2>&1 | tail -3`, `just check 2>&1 | tail -1` → PASS.

- [ ] **Step 4: Commit** — `feat(server): proxied, disk-cached catalog covers`

---

### Task 5: Live verification

- [ ] Scratch server (dist static_dir, real sources.d, scratch db). Browse a source page via the API twice; assert the second request logs no source fetch and returns instantly; check `cover_url` values point at `/api/v1/covers?src=…`; fetch one and confirm an image content-type and a file under `covers/by-url/`; fetch a cover URL not in the catalog → 404. Load the Sources tab in headless firefox and screenshot the grid (covers render through the proxy). Send the screenshot to the user.

---

### Task 6: PR

- [ ] `gh pr create --base develop --title "feat: source catalog cache and cover proxy"` — body: spec summary, TTL config, non-open-proxy note, verification evidence. Standard footer. Auto-merge.

---

## Self-review notes

- Spec coverage: storage (T1), policy+config (T2), endpoint wiring incl. search upsert (T3), proxy + rewrite + type decision (T4), E2E (T5).
- The `cover_url: Option<String>` relaxation is the one cross-cutting decision; it is stated explicitly in Task 4 and back-propagates to Task 1's literals (compiler-guided).
- Names consistent: `upsert_catalog_entries`, `write_catalog_page`, `read_catalog_page`, `catalog_source_for_cover`, `CachePlan::decide`, `Inflight::start/finish`.
