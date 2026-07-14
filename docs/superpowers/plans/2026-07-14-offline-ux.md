# Offline UX Implementation Plan

> Spec: docs/superpowers/specs/2026-07-14-offline-ux-design.md

**Goal:** bounded loading everywhere, cache-first reads while offline, a
retry badge, and offline library covers in the shells.

### Task 1: request timeouts (crates/yomu-client/src/lib.rs)

- [ ] `check_status` becomes a method; it `build()`s the request, sets an
      8 s timeout via `timeout_mut()` when none is set, and executes it
      with `self.http.execute`.
- [ ] `health()` builds its request with `.timeout(3s)`.

### Task 2: connectivity signal + gate rework (crates/yomu-ui/src/lib.rs)

- [ ] `Connectivity { Checking, Online, Offline }` + context provider in
      `App`, `use_connectivity()` accessor.
- [ ] `ServerGate` states: `Checking` / `Ready` / `Unreachable`. Probe
      result maps: Ok ⇒ Ready + Online; Err+seen ⇒ Ready + Offline;
      Err+unseen ⇒ Unreachable. "Continue anyway" ⇒ Ready + Offline.
- [ ] `OfflineBadge` component in `App` (visible when conn != Online):
      retry button, spinner while Checking, "still offline" flash; on
      success flush outbox + marks.
- [ ] `online` event listener: when conn != Online, one probe (replaces
      the gate-scoped listener).

### Task 3: cache-first helper + page migration

- [ ] `offline::cached(conn, key, fetch)` per spec §3 (+ non-flagged
      wrapper), replacing `with_cache`/`with_cache_flagged` call sites in
      home, library (library+categories), manga detail, sources list,
      downloads, reader detail. Each resource closure reads `conn.get()`.
- [ ] Reader `chapter_pages`: when Offline, try device metadata before the
      network.

### Task 4: shell cover storage (crates/yomu-shell/src/lib.rs)

- [ ] `device_save_cover` command (download to `covers/<manga>.<ext>`),
      registered in the invoke handler.
- [ ] `yomudev` protocol route `cover/<manga>`.

### Task 5: Cover component + sweep (crates/yomu-ui)

- [ ] `offline::shell_cover_url`, `offline::shell_save_cover`,
      `yomu-device-covers` mark set.
- [ ] `cover.rs`: `Cover(manga_id, src, class)` per spec §5; used in
      library grid + manga page.
- [ ] Library page: post-load sweep saving missing covers (shell + Online
      only).

### Task 6: verification

- [ ] `just check`, `cargo test --workspace --exclude yomu-shell`, Kotlin
      compile check not needed (no shell/Kotlin change on Android side —
      Tauri command is Rust).
- [ ] Headless E2E per spec §7 (launch bound, zero offline API calls,
      retry flow), desktop shell cover check.
