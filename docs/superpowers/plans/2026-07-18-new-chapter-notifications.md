# New-Chapter Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Spec: docs/superpowers/specs/2026-07-18-new-chapter-notifications-design.md

**Goal:** OS notifications for updater-found chapters from the shells (in-app polling everywhere, WorkManager while the Android app is off), backed by a server updates feed; plus the category-select flicker fix.

**Architecture:** The updater persists "new chapters for manga X" events to an `updates` table (same trigger point as the existing ntfy push). `GET /api/v1/updates?since=` serves them. Shells poll with a watermark and raise notifications via `tauri-plugin-notification`; on Android a Kotlin `UpdatesWorker` does the same poll from WorkManager so it runs with the app killed, sharing the watermark through the `YomuAndroid` bridge.

**Tech Stack:** axum/sqlx (server), Leptos/wasm (UI), Tauri v2 + tauri-plugin-notification (shells), Kotlin + WorkManager (Android).

---

### Task 1: updates feed — domain types, migration, DB layer

**Files:**
- Modify: `crates/yomu-domain/src/api.rs` (append near DownloadsResponse)
- Create: `crates/yomu-server/migrations/0010_updates.sql`
- Create: `crates/yomu-server/src/db/updates.rs`
- Modify: `crates/yomu-server/src/db/mod.rs` (add `mod updates;`)

- [ ] **Step 1: domain types**

```rust
/// One updater round's find for one manga (`GET /updates`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateEvent {
    pub manga_id: Uuid,
    pub manga_title: String,
    pub chapter_count: u32,
    pub first_title: String,
    pub last_title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdatesResponse {
    pub updates: Vec<UpdateEvent>,
}
```

- [ ] **Step 2: migration 0010_updates.sql**

```sql
-- Updater-found new chapters, one row per manga per round; feeds shell
-- notifications. Pruned after 30 days.
CREATE TABLE updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    manga_id TEXT NOT NULL,
    chapter_count INTEGER NOT NULL,
    first_title TEXT NOT NULL,
    last_title TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX updates_created_at ON updates (created_at);
```

- [ ] **Step 3: db/updates.rs** — `add_update(&self, manga_id, chapters: &[Chapter])` (no-op on empty), `updates_since(&self, since: DateTime<Utc>, limit: i64) -> Vec<UpdateEvent>` (JOIN manga for the title, newest first), `prune_updates(&self, before: DateTime<Utc>)`. Unit tests in the file (in-memory Db like siblings): write two events, filter by `since`, cap, prune.

- [ ] **Step 4:** `cargo test -p yomu-server updates` → PASS; commit `feat(server): updates event log`.

### Task 2: updates feed — API route + updater writes

**Files:**
- Create: `crates/yomu-server/src/api/updates.rs`
- Modify: `crates/yomu-server/src/api/mod.rs` (`.route("/updates", get(updates::list))`)
- Modify: `crates/yomu-server/src/updater.rs`

- [ ] **Step 1: api/updates.rs**

```rust
//! `/api/v1/updates`: updater-found new chapters since a watermark,
//! for shell notifications. Read-only, so `OptionalUser`.
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use yomu_domain::UpdatesResponse;

use super::ApiError;
use crate::auth::OptionalUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct UpdatesQuery {
    since: chrono::DateTime<chrono::Utc>,
}

pub async fn list(
    State(state): State<AppState>,
    OptionalUser(_user): OptionalUser,
    Query(q): Query<UpdatesQuery>,
) -> Result<Json<UpdatesResponse>, ApiError> {
    let updates = state.db.updates_since(q.since, 100).await?;
    Ok(Json(UpdatesResponse { updates }))
}
```

- [ ] **Step 2: updater.rs** — at the top of each round: `let _ = state.db.prune_updates(Utc::now() - chrono::Duration::days(30)).await;` (log on Err). In the `Ok(new) if !new.is_empty()` arm, before the ntfy call: `if let Err(err) = state.db.add_update(entry.id, &new).await { tracing::warn!(%err, "recording update event"); }`.

- [ ] **Step 3:** route test (axum router unit test like existing api tests if present; else covered by db tests + manual curl). `cargo test -p yomu-server` → PASS. Commit `feat(server): GET /updates feed`.

### Task 3: client method

**Files:** Modify `crates/yomu-client/src/lib.rs`

- [ ] **Step 1:**

```rust
pub async fn updates(&self, since: chrono::DateTime<chrono::Utc>) -> Result<UpdatesResponse> {
    let mut url = self.base.join("api/v1/updates")?;
    url.query_pairs_mut().append_pair("since", &since.to_rfc3339());
    let resp = self.check_status(self.http.get(url)).await?;
    Ok(resp.json().await?)
}
```

(match the crate's existing helper style; add chrono dep if not present — it is, via yomu-domain re-exports? verify.)

- [ ] **Step 2:** `cargo check -p yomu-client` (native + wasm via `just check`). Commit `feat(client): updates()`.

### Task 4: shell notification plugin

**Files:**
- Modify: `crates/yomu-shell/Cargo.toml` (`tauri-plugin-notification = "2"`)
- Modify: `crates/yomu-shell/src/lib.rs` (`.plugin(tauri_plugin_notification::init())` on the Builder)
- Create: `crates/yomu-shell/capabilities/default.json`

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["core:default", "notification:default"]
}
```

- [ ] Verify the desktop shell still builds: `nix develop /projects/rust/yomu#tauri --command cargo build -p yomu-shell --release`. Commit `feat(shell): notification plugin`.

### Task 5: UI polling loop

**Files:**
- Create: `crates/yomu-ui/src/notify.rs`
- Modify: `crates/yomu-ui/src/lib.rs` (`mod notify;` + start from `App` when `offline::shell_available()`)

- [ ] **Step 1: notify.rs** — `pub fn start(conn: RwSignal<Connectivity>, client: YomuClient)`:
  - watermark accessors: if `window.YomuAndroid.updatesWatermark` exists (js_sys::Reflect), call it / `setUpdatesWatermark(ts)`; else localStorage `yomu-updates-seen`.
  - first run (no watermark): store `Utc::now().to_rfc3339()`, skip fetch.
  - `poll()`: if `conn.get_untracked() != Online` return; `client.updates(watermark).await`; for each event raise a notification (title = manga_title, body = ntfy-style message from counts/titles, tag = manga_id) via `invoke("plugin:notification|notify", …)` after a one-time `request_permission`; advance watermark to max `created_at` if non-empty.
  - schedule: run once on start + `set_interval_with_handle` every 15 min.
  - also: on Android, call `window.YomuAndroid.configureUpdates(api_base)` once at start (Reflect; absent = no-op) so the background worker knows the server.

- [ ] **Step 2:** `just check` clean. Commit `feat(ui): shell update notifications`.

### Task 6: Android app-off worker

**Files:**
- Modify: `crates/yomu-shell/gen/android/app/build.gradle.kts` (add `implementation("androidx.work:work-runtime-ktx:2.9.1")`)
- Create: `crates/yomu-shell/gen/android/app/src/main/java/xyz/tdbm/yomu/UpdatesWorker.kt`
- Modify: `crates/yomu-shell/gen/android/app/src/main/java/xyz/tdbm/yomu/MainActivity.kt`

- [ ] **Step 1: UpdatesWorker.kt** — CoroutineWorker (or plain Worker): read `SharedPreferences("yomu-updates")` keys `base`, `seen`; GET `<base>api/v1/updates?since=<seen>` (URL-encode; HttpURLConnection, 10 s timeouts); parse with org.json; post one notification per event on channel `new_chapters` (create channel, tag = manga_id, PendingIntent opening MainActivity); on success set `seen` = newest `created_at`; missing permission → skip posting but still advance the watermark (the fetch succeeded; spec ties advancement to fetch success only). Network failure → `Result.retry()`.
- [ ] **Step 2: MainActivity bridge** — in `ImmersiveBridge` (or a second interface on the same object): `@JavascriptInterface fun configureUpdates(base: String)` → store `base` in prefs, enqueue `PeriodicWorkRequestBuilder<UpdatesWorker>(30, MINUTES)` with network constraint, `ExistingPeriodicWorkPolicy.UPDATE`, unique name `yomu-updates`; `fun updatesWatermark(): String` / `fun setUpdatesWatermark(ts: String)` → prefs `seen`.
- [ ] **Step 3:** build the APK (`nix develop /projects/rust/yomu#android` per justfile recipe) and confirm gradle compiles; manifest gains POST_NOTIFICATIONS from the plugin. Commit `feat(android): app-off update notifications via WorkManager`.

### Task 7: category-select flicker

**Files:** Modify `crates/yomu-ui/src/pages/manga.rs`; E2E in scratchpad.

- [ ] **Step 1 (root cause):** headless-Blink repro — open a manga page with a stubbed slow `/categories` (300 ms) and a downloads poll that bumps `refresh`; sample `document.querySelector('.category-select')` presence every 50 ms across a bump. Expected (hypothesis): select disappears while the remounted `categories` LocalResource refetches.
- [ ] **Step 2 (fix):** hoist the categories fetch to `MangaPage` (created once per page visit, passed down), so a `refresh` bump can't recreate it; keep rendering from the last loaded list.
- [ ] **Step 3:** E2E again → select present at every sample. Commit `fix(ui): keep category select mounted across refreshes`.

### Task 8: E2E for shell notifications

- [ ] Shell-sim script in scratchpad (pattern: ring-e2e.js): fake `__TAURI__.core.invoke` capturing `plugin:notification|notify` calls; static dist on 4799; stub `/api/v1/updates` sequence (first run → no fetch; second load with old watermark → 2 events). Assert: no notifications on first run; 2 notifications with manga titles + tags after; watermark advanced to newest created_at.

### Task 9: verify + ship

- [ ] `just check`, `cargo test --workspace --exclude yomu-shell`.
- [ ] PR into develop; then release 1.12.0 (Cargo.toml line 14 + tauri.conf.json version + `cargo update -w`, develop→main PR, unsigned tag, back-sync). **APK + server ship together** (endpoint is additive, old clients fine; new APK against old server 404s → treated as fetch failure, quiet).
