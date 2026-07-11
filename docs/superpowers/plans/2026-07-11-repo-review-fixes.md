# Yomu Repo-Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the security and correctness findings from the full-repo review, in descending order of severity.

**Architecture:** Each task is a self-contained fix with its own test where a test is meaningful (server/source/db logic). Browser-only reactive fixes (pager, credentials) are verified by build + code inspection since they have no unit-test surface. Commit after each task.

**Tech Stack:** Rust 2024 workspace, axum 0.8 + sqlx/SQLite (server), Leptos WASM (ui), scraper (source), reqwest (client, native+wasm).

**Verification baseline:** `cargo clippy -p yomu-domain -p yomu-source -p yomu-server -p yomu-client -p yomu-ui --all-targets` (clean today) and `cargo test -p yomu-domain -p yomu-source -p yomu-server -p yomu-client` (50 tests pass today). `yomu-shell` cannot build here (needs system GTK/cairo) — verify it by `cargo check -p yomu-shell` only if the toolchain is present, otherwise inspection.

---

### Task 1: Require auth on mutating/library/source routes (CRITICAL)

**Problem:** In OIDC mode only progress endpoints use `CurrentUser`; every library/source/chapter mutation is unauthenticated.

**Approach:** Add a `RequireUser` extractor that (a) is the shared user in single-account mode and (b) rejects with 401 in OIDC mode — identical to `CurrentUser`. Apply it to every non-auth, non-health route as an argument so an unauthenticated request in OIDC mode is rejected. `CurrentUser` already does exactly this, so reuse it: add `CurrentUser` as the first extractor argument to the handlers that currently take none (or `OptionalUser` where per-user enrichment is needed but the route must still require a session in OIDC mode).

**Files:**
- Modify: `crates/yomu-server/src/api/library.rs`, `chapters.rs`, `categories.rs`, `sources.rs`
- Test: `crates/yomu-server/tests/auth_routes.rs` (create)

- [ ] **Step 1: Write a failing integration test** that boots the router in OIDC mode and asserts an unauthenticated `POST /api/v1/library` returns 401.

```rust
// crates/yomu-server/tests/auth_routes.rs
// Boot AppState with auth.issuer = Some(dummy) so oidc_enabled() is true,
// build router(state), and drive it with tower::ServiceExt::oneshot.
// Assert POST /api/v1/library (no cookie/bearer) => 401 UNAUTHORIZED.
```

Run: `cargo test -p yomu-server --test auth_routes`
Expected: FAIL (currently returns 200/4xx-non-401).

Note: if constructing `AppState` in a test is impractical (OIDC runtime requires a live issuer), instead write a unit test in `auth.rs` asserting `CurrentUser::from_request_parts` rejects when `oidc_enabled()` and no token — and verify route wiring by inspection + `me`-style compile check. Prefer the integration test; fall back only if `OidcRuntime` cannot be stubbed.

- [ ] **Step 2: Add `CurrentUser` to each mutating handler.** For every handler under the routes at `api/mod.rs:29-63` that currently lacks an auth extractor, add `_user: CurrentUser` (or `OptionalUser` only for GETs that must stay usable while genuinely public — none here in OIDC mode). Specifically: `library::add/detail/update/delete/refresh/list/cover`, `categories::list/update`, `sources::list/search/browse/cover/search_all`, `chapters::download/download_many/remove_downloads/pages/page_image`. Keep `health` and `auth::*` open.

- [ ] **Step 3: Build + run the test.**

Run: `cargo test -p yomu-server --test auth_routes && cargo clippy -p yomu-server --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit.**

```bash
git add -A && git commit -m "fix(server): require a session on all routes in OIDC mode"
```

---

### Task 2: Restrict CORS to the configured origin (HIGH, pairs with Task 1)

**Problem:** `CorsLayer::permissive()` + cookie auth lets any web origin drive state changes.

**Files:**
- Modify: `crates/yomu-server/src/api/mod.rs:70-81`
- Modify: `crates/yomu-server/src/config.rs` (add `cors_allow_origin` under a suitable section, default none)

- [ ] **Step 1:** Add `allowed_origins: Vec<url::Url>` to `AuthConfig` (default empty). When empty, keep same-origin only (no CORS layer needed for the served-frontend case). When set, build `CorsLayer::new().allow_credentials(true).allow_origin(<parsed HeaderValues>).allow_methods(Any).allow_headers(Any)`.

- [ ] **Step 2:** Replace `.layer(CorsLayer::permissive())` at `api/mod.rs:79` with the conditional layer. Note `allow_credentials(true)` is incompatible with `allow_origin(Any)` — that is the whole point.

- [ ] **Step 3:** Build.

Run: `cargo clippy -p yomu-server --all-targets`
Expected: no warnings.

- [ ] **Step 4: Commit.**

```bash
git add -A && git commit -m "fix(server): scope CORS to configured origins with credentials"
```

---

### Task 3: Downloader must not destroy a good copy on failed publish (MEDIUM)

**Problem:** `download_chapter` runs `remove_dir_all(&dir)` before `rename(&partial, &dir)`; a rename failure loses the previously-published chapter.

**Files:**
- Modify: `crates/yomu-server/src/downloader.rs:92-96`

- [ ] **Step 1: Rewrite the publish to move-aside-then-swap.** Rename the existing `dir` to a `dir.with_extension("old")` first (ignore error if absent), rename `partial`→`dir`, and only on success remove the `.old`; on rename failure, restore `.old`→`dir`.

```rust
    // Atomic-ish publish that never destroys a good copy: stage the old
    // directory aside, promote the new one, then drop the old. If the
    // promotion fails, put the old copy back.
    let backup = dir.with_extension("old");
    let _ = tokio::fs::remove_dir_all(&backup).await;
    let had_old = tokio::fs::rename(&dir, &backup).await.is_ok();
    if let Err(e) = tokio::fs::rename(&partial, &dir).await {
        if had_old {
            let _ = tokio::fs::rename(&backup, &dir).await;
        }
        return Err(format!("publishing {}: {e}", dir.display()));
    }
    let _ = tokio::fs::remove_dir_all(&backup).await;
```

- [ ] **Step 2:** Build + existing tests.

Run: `cargo clippy -p yomu-server --all-targets && cargo test -p yomu-server`
Expected: no warnings, tests pass.

- [ ] **Step 3: Commit.**

```bash
git add -A && git commit -m "fix(server): failed download publish no longer destroys the existing copy"
```

---

### Task 4: SSRF guard — block IPv4-mapped IPv6 and CGNAT (HIGH)

**Problem:** `is_private_target` IPv6 arm never unwraps `::ffff:x.x.x.x`, so `http://[::ffff:169.254.169.254]/` passes; IPv4 arm misses `100.64.0.0/10`.

**Files:**
- Modify: `crates/yomu-source/src/selector.rs:814-832`
- Test: extend `private_targets_are_blocked` at `selector.rs:1005`

- [ ] **Step 1: Add failing test cases** to the `blocked` array:

```rust
            "http://[::ffff:169.254.169.254]/x", // v4-mapped metadata
            "http://[::ffff:10.0.0.5]/x",        // v4-mapped private
            "http://100.64.0.1/x",               // CGNAT
```

Run: `cargo test -p yomu-source private_targets_are_blocked`
Expected: FAIL on the mapped/CGNAT entries.

- [ ] **Step 2: Fix the guard.** Extract the IPv4 predicate and reuse it for mapped addresses; add CGNAT.

```rust
fn is_private_v4(ip: std::net::Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.octets()[0] == 0
        || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])) // CGNAT 100.64/10
}

fn is_private_target(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => is_private_v4(ip),
        Some(url::Host::Ipv6(ip)) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (ip.segments()[0] & 0xffc0) == 0xfe80 // link-local  fe80::/10
        }
        _ => false,
    }
}
```

- [ ] **Step 3: Run test.**

Run: `cargo test -p yomu-source private_targets_are_blocked && cargo clippy -p yomu-source --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit.**

```bash
git add -A && git commit -m "fix(source): SSRF guard blocks v4-mapped IPv6 and CGNAT ranges"
```

---

### Task 5: Cross-origin credentials in the wasm client (HIGH)

**Problem:** wasm reqwest defaults to same-origin credentials; when the frontend targets a remote server, session cookies are never sent → 401.

**Files:**
- Modify: `crates/yomu-client/src/lib.rs` (`check_status`, ~line 280)

- [ ] **Step 1: Add a wasm-only credentials helper and apply it to every request.**

```rust
/// On wasm, `fetch` defaults to same-origin credentials; a cross-origin
/// server deployment (remote `yomu-api-base`) then never receives the
/// session cookie. Force include. No-op on native.
fn with_credentials(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req.fetch_credentials_include()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        req
    }
}
```

Then in `check_status`, wrap: `let resp = with_credentials(req).send().await...`.

- [ ] **Step 2: Build both targets.**

Run: `cargo clippy -p yomu-client --all-targets` and, if the wasm target is installed, `cargo clippy -p yomu-client --target wasm32-unknown-unknown`
Expected: no warnings on native; wasm compiles (`fetch_credentials_include` exists on the wasm RequestBuilder).

- [ ] **Step 3: Commit.**

```bash
git add -A && git commit -m "fix(client): include credentials on wasm so cross-origin auth works"
```

---

### Task 6: Reader — pager `snap` deadlock fallback (MEDIUM)

**Problem:** A committed turn sets `snap = Some(delta)` and relies on `transitionend`; if the transition never fires (reduced-motion, `transition: none`), `snap` stays `Some` forever and all turns die.

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs` (where `snap.set(Some(delta))` is called in `request_turn`, ~line 285, and the `on_transitionend` handler ~812)

**Approach:** When arming a snap, also schedule a fallback timer (`set_timeout`) slightly longer than the CSS transition (e.g. 400ms) that, if `snap` is still `Some(delta)`, performs the same commit the `transitionend` would. Extract the commit-from-snap logic so both paths share it. This is a WASM/browser fix with no unit-test surface — verify by `cargo check` and code inspection; behaviorally confirm with reduced-motion in a browser if available.

- [ ] **Step 1: Extract the snap-commit closure** used by `on_transitionend` (the `snap.set(None); drag.set(0.0); commit(pos+delta)` block) into a reusable closure `finish_snap` capturing `snap`, `drag`, `pos`, `commit`.

- [ ] **Step 2: On `snap.set(Some(delta))` in `request_turn`, schedule a fallback:**

```rust
            snap.set(Some(delta));
            // Fallback: if the transform transition never fires transitionend
            // (prefers-reduced-motion, transition:none), land the turn anyway
            // so the pager can't wedge.
            let finish = finish_snap.clone();
            set_timeout(move || finish(), std::time::Duration::from_millis(400));
```

Use `leptos::leptos_dom::helpers::set_timeout` (or `leptos::prelude::set_timeout`, whichever this Leptos version exposes — grep the crate for existing `set_timeout` usage first). `finish_snap` must be idempotent: guard on `snap.get_untracked() == Some(delta)` (or just `is_some()`) so a real `transitionend` that already cleared it makes the timer a no-op.

- [ ] **Step 3: Make `on_transitionend` call the same `finish_snap`.**

- [ ] **Step 4: Build.**

Run: `cargo clippy -p yomu-ui --all-targets`
Expected: no warnings.

- [ ] **Step 5: Commit.**

```bash
git add -A && git commit -m "fix(reader): fallback timer prevents pager deadlock without transitionend"
```

---

### Task 7: Reader — arrow keys in vertical mode scroll instead of phantom-journaling (MEDIUM)

**Problem:** In vertical mode the keydown handler calls `turn(delta)` which mutates `page` + reports without scrolling; `go_page`'s vertical branch already scrolls correctly.

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs:303-320`

- [ ] **Step 1:** Replace the keydown handler's `ReaderMode::Vertical => key_turn(delta)` with a call to the same logic `go_page` uses for vertical (scroll to the adjacent `img[data-chapter][data-page]`). Simplest: clone `go_page` into the keydown handler and call `key_go_page(delta)` for both arms (paged `go_page` already routes to `request_turn`). Remove the now-unused `key_turn`/`turn` clone if nothing else uses it (grep first — `turn` may be used elsewhere).

- [ ] **Step 2: Build.**

Run: `cargo clippy -p yomu-ui --all-targets`
Expected: no warnings (watch for unused-variable warnings from a removed `turn` clone).

- [ ] **Step 3: Commit.**

```bash
git add -A && git commit -m "fix(reader): arrow keys scroll the strip in vertical mode"
```

---

### Task 8: Reader — clamp `?page=` to the page count (MEDIUM)

**Problem:** `initial_page` from `?page=N` is never clamped; an overshoot opens a blank panel and journals a bogus position.

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs:184-197` (the `opened` effect)

- [ ] **Step 1:** In the `opened` effect, before `report(...)`, clamp both `page` and `pos` into `0..count`:

```rust
                let count = page_count();
                if wants_end {
                    let last = count.saturating_sub(1);
                    page.set(last);
                    pos.set(last as i64);
                } else if page.get_untracked() >= count {
                    let last = count.saturating_sub(1);
                    page.set(last);
                    pos.set(last as i64);
                }
                report(chapter_id, page.get_untracked());
```

- [ ] **Step 2: Build.**

Run: `cargo clippy -p yomu-ui --all-targets`
Expected: no warnings.

- [ ] **Step 3: Commit.**

```bash
git add -A && git commit -m "fix(reader): clamp out-of-range ?page= to the last page"
```

---

### Task 9: Validate `chapter_id` in the offline push path (MEDIUM)

**Problem:** `append_events` validates `manga_id` exists but never `chapter_id`; dangling positions result.

**Files:**
- Modify: `crates/yomu-server/src/db.rs:729-765`
- Test: add to the `db.rs` test module

- [ ] **Step 1: Write a failing test** in `db.rs` tests: insert a manga+chapters, then `append_events` with a real `manga_id` but a random `chapter_id`; assert the event is *skipped* (counted in `skipped`), and `latest_position` returns `None`.

Run: `cargo test -p yomu-server append_events`
Expected: FAIL (event currently accepted).

- [ ] **Step 2: Add a chapter-existence check** alongside the manga check in the `append_events` loop:

```rust
            let chapter_known: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM chapters WHERE id = ?)",
            )
            .bind(event.chapter_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
            if !known || !chapter_known {
                skipped += 1;
                continue;
            }
```

(Replace the existing `if !known` block.)

- [ ] **Step 3: Run test.**

Run: `cargo test -p yomu-server && cargo clippy -p yomu-server --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit.**

```bash
git add -A && git commit -m "fix(server): skip offline events with unknown chapter_id"
```

---

### Task 10: `upsert_oidc_user` — handle concurrent same-subject insert (MEDIUM)

**Problem:** Check-then-insert race: two first logins for the same `sub` can double-insert; the unique-violation fallback only handles username collisions.

**Files:**
- Modify: `crates/yomu-server/src/db.rs:602-647`

- [ ] **Step 1:** After any insert path hits a unique violation, re-query by subject and return that user if present (the concurrent winner) before attempting the username-suffix fallback. Restructure:

```rust
        match result {
            Ok(_) => self.user_by_id(id).await,
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                // Someone inserted this subject concurrently, or the username
                // collided. Prefer the existing subject row; else retry with a
                // subject-qualified username.
                if let Some(existing) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM users WHERE subject = ?",
                )
                .bind(subject)
                .fetch_optional(&self.pool)
                .await?
                {
                    return self.user_by_id(parse_uuid(existing)?).await;
                }
                insert(format!("{}-{subject}", username.trim().to_lowercase()))
                    .execute(&self.pool)
                    .await?;
                self.user_by_id(id).await
            }
            Err(e) => Err(e.into()),
        }
```

(Remove the trailing `self.user_by_id(id).await` that followed the old match.)

- [ ] **Step 2: Build + tests.**

Run: `cargo clippy -p yomu-server --all-targets && cargo test -p yomu-server`
Expected: no warnings, tests pass.

- [ ] **Step 3: Commit.**

```bash
git add -A && git commit -m "fix(server): resolve concurrent same-subject OIDC user creation"
```

---

### Task 11: Low-severity batch — dates, events cursor, SW navigation, local symlink

These are independent one-liners/small fixes; commit each separately.

**11a — Datetime-without-timezone chapter dates** (`crates/yomu-source/src/dates.rs:22-29`)
- [ ] Add a `NaiveDateTime::parse_from_str` attempt between the `DateTime` and `NaiveDate` attempts:

```rust
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, fmt) {
            return Some(dt.and_utc());
        }
```
- [ ] Add a test: `parse_chapter_date("2026-05-19 14:30", Some("%Y-%m-%d %H:%M"), now)` is `Some`. Run `cargo test -p yomu-source`. Commit `fix(source): parse chapter dates that carry a time but no timezone`.

**11b — `events_since` cursor null on a short page** (`crates/yomu-server/src/db.rs:806`)
- [ ] Change `let next = rows.last().map(|row| row.seq);` to return `None` when fewer than the limit came back, so the client learns it is caught up without an extra round-trip:

```rust
        let next = (rows.len() == 1000).then(|| rows.last().map(|r| r.seq)).flatten();
```
- [ ] Verify no UI code treats `None` as "start over" (grep `events_since` consumers — it should mean "caught up"). Run `cargo test -p yomu-server`. Commit `perf(server): stop returning a sync cursor on the final events page`.

**11c — SW: fall back to cached shell on non-ok navigation** (`crates/yomu-web/sw.js:110-123`)
- [ ] In `navigate`, when `response.ok` is false, return the cached shell if present:

```js
    const response = await fetch(SHELL);
    if (response.ok) {
      event.waitUntil(refreshShell(cache, response.clone()).catch(() => {}));
      return response;
    }
    const cached = await cache.match(SHELL);
    return cached || response;
```
- [ ] Commit `fix(web): serve cached shell when navigation fetch returns non-ok`. (No build step; sw.js is copied to dist by the build — do not hand-edit dist/.)

**11d — Local source: reject symlink escapes** (`crates/yomu-source/src/local.rs:60-74`)
- [ ] In `resolve`, after the lexical check and `path.exists()`, canonicalize both `self.dir` and `path` and assert the canonical path starts with the canonical dir:

```rust
        let canon_dir = self.dir.canonicalize().map_err(|e| {
            SourceError::Parse(format!("local dir not resolvable: {e}"))
        })?;
        let canon = path.canonicalize().map_err(|e| {
            SourceError::Parse(format!("local key {key:?} not resolvable: {e}"))
        })?;
        if !canon.starts_with(&canon_dir) {
            return Err(SourceError::Parse(format!("local key {key:?} escapes the local dir")));
        }
        Ok(canon)
```
- [ ] Extend the `keys_cannot_escape_the_local_dir` test with a symlink case if the test harness can create one (`std::os::unix::fs::symlink`); otherwise inspection. Run `cargo test -p yomu-source`. Commit `fix(source): reject symlinked local keys that escape the confinement dir`.

---

### Final verification

- [ ] `cargo clippy -p yomu-domain -p yomu-source -p yomu-server -p yomu-client -p yomu-ui --all-targets` — clean.
- [ ] `cargo test -p yomu-domain -p yomu-source -p yomu-server -p yomu-client` — all pass.
- [ ] Review the full diff (`git log --oneline` for this branch) against the review findings.
</content>
