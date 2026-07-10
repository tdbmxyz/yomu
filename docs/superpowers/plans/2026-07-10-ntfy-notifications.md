# New-Chapter ntfy Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The periodic updater POSTs a push notification to a configured ntfy topic when a sync finds new chapters.

**Architecture:** A `Notifier` in `yomu-server` (own `reqwest::Client`, built from an optional `[notify]` config section) is called only from `updater.rs`; `sync::refresh_manga` returns the new chapters themselves instead of a count so the updater has titles. Fire-and-forget: failures warn, never break a sync.

**Tech Stack:** Rust — reqwest, figment config, axum (as the mock server in tests).

**Spec:** `docs/superpowers/specs/2026-07-10-ntfy-notifications-design.md`

Branch: `feature/ntfy-notifications` off `origin/develop` (independent of the immersive branch). Commit messages end with the standard Co-Authored-By + Claude-Session trailer used across this repo.

---

### Task 1: `[notify]` config section

**Files:**
- Modify: `crates/yomu-server/src/config.rs`

- [ ] **Step 1: Add the config type**

After `LocalConfig`'s `impl Default` (~line 54):

```rust
/// Push notifications for updater-found chapters, POSTed to an ntfy
/// topic. Absent section = feature off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// ntfy topic URL, e.g. `https://ntfy.example.net/yomu`.
    pub url: url::Url,
    /// Optional ntfy access token (sent as `Authorization: Bearer`).
    #[serde(default)]
    pub token: Option<String>,
}
```

In `Config`: add `pub notify: Option<NotifyConfig>,` after `pub auth: AuthConfig,` and `notify: None,` in `impl Default for Config`.

- [ ] **Step 2: Verify compile**

Run: `cargo check -p yomu-server 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Document in the example config**

Append to `crates/yomu-server/yomu.example.toml`:

```toml
# Push notifications for new chapters found by the updater, sent to an
# ntfy topic (https://ntfy.sh or self-hosted). Subscribe to the topic in
# the ntfy app. Omit the section to disable.
#[notify]
#url = "https://ntfy.example.net/yomu"
#token = "tk_..."          # optional access token
```

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-server/src/config.rs crates/yomu-server/yomu.example.toml
git commit -m "feat(server): optional [notify] config section"
```

---

### Task 2: Notifier module

**Files:**
- Create: `crates/yomu-server/src/notifier.rs`
- Modify: `crates/yomu-server/src/main.rs` (add `mod notifier;` to the module list)

- [ ] **Step 1: Write the module with failing tests**

```rust
//! Push notifications for new chapters, POSTed to an ntfy topic
//! (fire-and-forget: an unreachable ntfy must never fail a sync).

use yomu_domain::Chapter;

use crate::config::NotifyConfig;

pub struct Notifier {
    config: Option<NotifyConfig>,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(config: Option<NotifyConfig>) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// One push per manga per sync round. Failures are logged, never
    /// returned — a broken notifier must not stop the update sweep.
    pub async fn notify_new_chapters(&self, manga_title: &str, chapters: &[Chapter]) {
        let Some(config) = &self.config else {
            return;
        };
        if chapters.is_empty() {
            return;
        }
        let mut request = self
            .http
            .post(config.url.as_str())
            .header("X-Tags", "books");
        // HTTP header values are latin-1; a manga title beyond that moves
        // into the body instead of the X-Title header.
        match reqwest::header::HeaderValue::from_str(manga_title) {
            Ok(title) => {
                request = request.header("X-Title", title).body(message(chapters));
            }
            Err(_) => {
                request = request.body(format!("{manga_title}\n{}", message(chapters)));
            }
        }
        if let Some(token) = &config.token {
            request = request.bearer_auth(token);
        }
        match request.send().await {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(status = %resp.status(), "ntfy push rejected");
            }
            Err(err) => tracing::warn!(%err, "ntfy push failed"),
            Ok(_) => {}
        }
    }
}

/// Body: single chapter title, or "N new chapters — first … last" in the
/// order the listing produced them.
fn message(chapters: &[Chapter]) -> String {
    match chapters {
        [one] => one.title.clone(),
        many => format!(
            "{} new chapters — {} … {}",
            many.len(),
            many.first().expect("non-empty").title,
            many.last().expect("non-empty").title,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;
    use yomu_domain::DownloadState;

    fn chapter(title: &str) -> Chapter {
        Chapter {
            id: Uuid::now_v7(),
            manga_id: Uuid::now_v7(),
            source_key: format!("k-{title}"),
            title: title.into(),
            number: None,
            source_order: 0,
            scanlator: None,
            fetched_at: Utc::now(),
            published_at: None,
            download: DownloadState::None,
            page_count: None,
            read: false,
        }
    }

    #[test]
    fn message_single_chapter_is_its_title() {
        assert_eq!(message(&[chapter("Chapter 171")]), "Chapter 171");
    }

    #[test]
    fn message_many_chapters_shows_count_and_range() {
        let list = [
            chapter("Chapter 171"),
            chapter("Chapter 172"),
            chapter("Chapter 173"),
        ];
        assert_eq!(message(&list), "3 new chapters — Chapter 171 … Chapter 173");
    }

    /// End-to-end against a local mock: method, headers, body, auth.
    #[tokio::test]
    async fn posts_to_the_topic_with_title_and_token() {
        use axum::extract::State;
        use std::sync::{Arc, Mutex};

        type Seen = Arc<Mutex<Option<(String, String, String, String)>>>;
        let seen: Seen = Arc::new(Mutex::new(None));

        async fn capture(
            State(seen): State<Seen>,
            headers: axum::http::HeaderMap,
            body: String,
        ) -> &'static str {
            let get = |k: &str| {
                headers
                    .get(k)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            *seen.lock().unwrap() =
                Some((get("x-title"), get("x-tags"), get("authorization"), body));
            "ok"
        }

        let app = axum::Router::new()
            .route("/yomu", axum::routing::post(capture))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let notifier = Notifier::new(Some(NotifyConfig {
            url: format!("http://{addr}/yomu").parse().unwrap(),
            token: Some("tk_test".into()),
        }));
        notifier
            .notify_new_chapters("Some Manga", &[chapter("Chapter 5")])
            .await;

        let (title, tags, auth, body) = seen.lock().unwrap().clone().expect("request received");
        assert_eq!(title, "Some Manga");
        assert_eq!(tags, "books");
        assert_eq!(auth, "Bearer tk_test");
        assert_eq!(body, "Chapter 5");
    }

    #[tokio::test]
    async fn unconfigured_notifier_is_a_noop() {
        // Must return without any network activity or panic.
        Notifier::new(None)
            .notify_new_chapters("Some Manga", &[chapter("Chapter 5")])
            .await;
    }
}
```

Add `mod notifier;` to `crates/yomu-server/src/main.rs` next to the other `mod` lines.

- [ ] **Step 2: Run the tests (first fail-check the format test by breaking it mentally is not enough — run them)**

Run: `cargo test -p yomu-server notifier 2>&1 | tail -4`
Expected: PASS (module and tests land together; the mock-server test is the behavioral gate — if it hangs or fails, fix before proceeding). If axum's `serve` signature differs, mirror how `main.rs` starts the real server.

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-server/src/notifier.rs crates/yomu-server/src/main.rs
git commit -m "feat(server): ntfy notifier for new chapters"
```

---

### Task 3: refresh_manga returns the chapters; updater notifies

**Files:**
- Modify: `crates/yomu-server/src/sync.rs` (return type, ~line 22 and ~line 64)
- Modify: `crates/yomu-server/src/api/library.rs` (~line 135)
- Modify: `crates/yomu-server/src/updater.rs` (~line 34)

- [ ] **Step 1: Change the return type**

`sync.rs`: doc comment and signature become

```rust
/// Returns the newly discovered chapters (twins merged into re-uploads
/// excluded — see `ChapterSync::new_chapters`).
pub async fn refresh_manga(state: &AppState, manga: &Manga) -> Result<Vec<Chapter>, SyncError> {
```

and the final line `Ok(new_chapters.len() as u32)` becomes `Ok(new_chapters)`. Import `Chapter` from `yomu_domain` (the file already imports `Manga` from there).

`api/library.rs` refresh handler:

```rust
    let new_chapters = sync::refresh_manga(&state, &manga).await?.len() as u32;
```

- [ ] **Step 2: Updater builds a notifier and pushes**

`updater.rs` — in `run`, before the loop:

```rust
    let notifier = crate::notifier::Notifier::new(state.config.notify.clone());
```

and the per-manga body becomes:

```rust
        for entry in manga {
            match sync::refresh_manga(&state, &entry).await {
                Ok(new) if !new.is_empty() => {
                    notifier.notify_new_chapters(&entry.title, &new).await;
                }
                Ok(_) => {}
                Err(err) => {
                    // One broken source must not stop the sweep.
                    tracing::warn!(manga = %entry.title, %err, "update check failed");
                }
            }
        }
```

- [ ] **Step 3: Verify compile + full server tests**

Run: `cargo test -p yomu-server 2>&1 | tail -3`
Expected: PASS. (The updater-only scoping the spec requires is now true by construction: `Notifier` is instantiated solely in `updater.rs` — confirm with `grep -rn "Notifier::new" crates/yomu-server/src`.)

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-server/src/sync.rs crates/yomu-server/src/api/library.rs crates/yomu-server/src/updater.rs
git commit -m "feat(server): notify ntfy when the updater finds new chapters"
```

---

### Task 4: Verification and PR

- [ ] **Step 1: Full checks**

Run: `just check 2>&1 | tail -3` and `cargo test --workspace --exclude yomu-shell 2>&1 | grep -cE "test result: ok"`
Expected: clean / all ok.

- [ ] **Step 2: Live smoke test**

Run a scratch server (scratch db/data in the session scratchpad, port 4791, sources_dir at `~/.config/yomu/sources.d/` — real definitions, never committed) with:

```toml
[notify]
url = "https://ntfy.sh/<random-scratch-topic>"
[updater]
interval_secs = 60
```

Add a manga, then delete one chapter row's worth of knowledge the cheap way: instead of DB surgery, simply verify the negative and positive paths as observable — (a) manual refresh produces NO push (updater-only), (b) wait for one updater tick; if the source has nothing new, temporarily prove the push path by curling the topic subscription (`curl -s https://ntfy.sh/<topic>/json?poll=1`) after step (a) to confirm silence, and rely on the Task 2 mock-server test for the push shape. If a real new chapter does land during the tick, confirm it via the same poll URL. Log what was and wasn't observable — do not claim an unobserved push fired.

- [ ] **Step 3: Push and open the PR into develop**

```bash
git push -u origin feature/ntfy-notifications
gh pr create --base develop --title "feat: new-chapter push notifications via ntfy" --body "..."
```

Body: config example, updater-only semantics, fire-and-forget failure handling, twin-merge non-notification note, zeus enablement hint (`services.yomu.settings.notify.url` — the NixOS module is freeform TOML). Standard footer. Enable auto-merge.

---

## Self-review notes

- Spec coverage: config (T1), notifier + formatting + mock-server test (T2), return-type change + updater wiring + updater-only scoping (T3), live verification honesty + PR (T4).
- Non-ASCII manga titles: handled by the header-or-body fallback in T2 (HTTP headers are latin-1).
- Type consistency: `Notifier::new(Option<NotifyConfig>)`, `notify_new_chapters(&str, &[Chapter])`, `refresh_manga -> Result<Vec<Chapter>, SyncError>` used consistently across tasks.
