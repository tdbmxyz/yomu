# Home & More Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Spec: docs/superpowers/specs/2026-07-19-home-more-cleanup-design.md

**Goal:** Remove the redundant Downloads link from the More page and drop fully-read titles from the Home "Continue reading" row.

**Architecture:** Two view-only edits over data already loaded — a deleted markup line and an added filter predicate.

---

### Task 1: Remove Downloads from More

**Files:** Modify `crates/yomu-ui/src/pages/more.rs`.

- [ ] **Step 1:** delete the Downloads link line:

```rust
                <a href="/downloads">"Downloads →"</a>
```

- [ ] **Step 2:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles. Commit `feat(ui): drop redundant Downloads link from More`.

### Task 2: Filter finished titles from Continue reading

**Files:** Modify `crates/yomu-ui/src/pages/home.rs`.

- [ ] **Step 1:** in the resume shelf builder, replace:

```rust
                    let mut resume: Vec<MangaWithPosition> =
                        list.iter().filter(|e| e.position.is_some()).cloned().collect();
```

with the unread guard:

```rust
                    let mut resume: Vec<MangaWithPosition> = list
                        .iter()
                        .filter(|e| e.position.is_some() && e.unread_count > 0)
                        .cloned()
                        .collect();
```

- [ ] **Step 2:** `cargo check -p yomu-ui --target wasm32-unknown-unknown` → compiles. Commit `feat(ui): drop finished titles from Continue reading`.

### Task 3: Verify + E2E

**Files:** Create `<scratchpad>/vscroll/home-more-e2e.js`.

- [ ] **Step 1:** `just check`. Passes (fmt + clippy + wasm).

- [ ] **Step 2:** headless check (bun + chromium against the dev server on 4791):
  - Load `/more`; assert `document.querySelector('a[href="/downloads"]')` is null.
  - Ensure library state has a finished title: pick a manga, `POST /api/v1/chapters/mark` all its chapters read (so `unread_count == 0`) after setting a position via `POST /api/v1/manga/{id}/position` (or read an existing position); pick another manga left in-progress (unread > 0, has position). Load `/`, wait for the "Continue reading" shelf, and assert its cards include the in-progress manga's cover link and exclude the finished one. Match cards by the manga id in each card's `href` (`/manga/<id>` or `/read/<id>/...`).

Run: `cd <scratchpad> && bun vscroll/home-more-e2e.js` → all PASS.

- [ ] **Step 3:** open the PR into develop.
