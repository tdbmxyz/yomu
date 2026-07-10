# Swipe Pager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tachidesk-style interactive page turning in paged mode — the page follows the finger, the neighbor is revealed, the turn commits past a distance/flick threshold — plus chapter-boundary transition panels and an accent edge bar replacing the current-chapter border.

**Architecture:** Paged mode renders a three-panel sliding track (prev/current/next) translated by drag offset; a pure `pager.rs` module decides commit/cancel, drag→step mapping, and what each panel shows over a virtual position `[-1 .. count]` whose ends are transition panels. Spec: `docs/superpowers/specs/2026-07-10-swipe-pager-design.md`.

**Tech Stack:** Leptos (yomu-ui, wasm), plain CSS in `crates/yomu-web/styles.css`, touch events via web-sys.

---

### Task 1: Current-chapter cue fix

**Files:**
- Modify: `crates/yomu-web/styles.css:753` (`.chapter-item.current`)

- [ ] **Step 1: Replace the border override with an inset edge bar**

```css
/* accent edge bar, not a border: the border is the download-state cue */
.chapter-item.current {
  box-shadow: inset 3px 0 0 var(--accent);
}
```

(Also reword the comment above the `dl-*` block, which says `.current` wins via border ordering: only `.selected` overrides the border now.)

- [ ] **Step 2: Commit**

```bash
git add crates/yomu-web/styles.css
git commit -m "fix(web): current-chapter cue no longer hides the download outline"
```

### Task 2: Pure pager module

**Files:**
- Create: `crates/yomu-ui/src/pager.rs` (module + `#[cfg(test)]` tests)
- Modify: `crates/yomu-ui/src/lib.rs` (add `pub mod pager;` next to `chapter_actions`)

- [ ] **Step 1: Write the module with failing-first tests**

```rust
//! Pure decisions for the paged reader's sliding track: does a released
//! drag commit or cancel, which way a drag turns, and what each of the
//! three panels shows over the virtual position range [-1 .. count]
//! (the ends are chapter-transition panels). Wasm-free so the matrix is
//! unit-testable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Commit,
    Cancel,
}

/// A released drag commits past 30% of the viewport width, or as a
/// flick: fast recent movement (>0.5 px/ms) in the drag's own direction
/// with at least 40px travelled.
pub fn verdict(dx: f64, width: f64, velocity: f64) -> Verdict {
    let flick = dx.abs() > 40.0 && velocity.abs() > 0.5 && velocity * dx > 0.0;
    if dx.abs() > width * 0.30 || flick {
        Verdict::Commit
    } else {
        Verdict::Cancel
    }
}

/// Position delta for a drag: pulling the page left (dx < 0) reveals
/// the panel on the right — the next page in LTR, the previous in RTL.
pub fn step(dx: f64, rtl: bool) -> i64 {
    let forward = if dx < 0.0 { 1 } else { -1 };
    if rtl { -forward } else { forward }
}

/// What a slot at `position` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Page(u32),
    /// "Finished previous — this chapter starts" panel at -1.
    TransitionPrev,
    /// "Finished this chapter — next up" panel at count.
    TransitionNext,
    /// Chapter edge with no neighbor: shown, but never landed on.
    DeadEnd,
    /// Beyond the virtual range: nothing.
    Empty,
}

pub fn panel(position: i64, count: u32, has_prev: bool, has_next: bool) -> Panel {
    if count == 0 {
        return Panel::Empty;
    }
    if (0..count as i64).contains(&position) {
        return Panel::Page(position as u32);
    }
    match position {
        -1 if has_prev => Panel::TransitionPrev,
        -1 => Panel::DeadEnd,
        p if p == count as i64 && has_next => Panel::TransitionNext,
        p if p == count as i64 => Panel::DeadEnd,
        _ => Panel::Empty,
    }
}

/// Lowest / highest position a turn may land on (dead ends rubber-band).
pub fn bounds(count: u32, has_prev: bool, has_next: bool) -> (i64, i64) {
    let lo = if has_prev { -1 } else { 0 };
    let hi = if has_next { count as i64 } else { count as i64 - 1 };
    (lo, hi)
}

/// Damped offset for dragging against a dead end.
pub fn damp(dx: f64) -> f64 {
    dx / 3.0
}

/// Flick velocity over the last ~100ms of touch samples.
#[derive(Default)]
pub struct Flick {
    samples: Vec<(f64, f64)>, // (x px, t ms)
}

impl Flick {
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn push(&mut self, x: f64, t: f64) {
        self.samples.push((x, t));
        self.samples.retain(|(_, st)| t - st <= 100.0);
    }

    /// px/ms over the retained window; 0 without two spaced samples.
    pub fn velocity(&self) -> f64 {
        let (Some((x0, t0)), Some((x1, t1))) = (self.samples.first(), self.samples.last())
        else {
            return 0.0;
        };
        if t1 - t0 <= 0.0 {
            return 0.0;
        }
        (x1 - x0) / (t1 - t0)
    }
}
```

Tests (same file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_drag_commits_short_drag_cancels() {
        assert_eq!(verdict(-200.0, 400.0, 0.0), Verdict::Commit); // 50% > 30%
        assert_eq!(verdict(-100.0, 400.0, 0.0), Verdict::Cancel); // 25%
    }

    #[test]
    fn flick_commits_but_only_in_its_own_direction() {
        assert_eq!(verdict(-50.0, 400.0, -0.8), Verdict::Commit);
        assert_eq!(verdict(-50.0, 400.0, 0.8), Verdict::Cancel); // reversed flick
        assert_eq!(verdict(-30.0, 400.0, -0.8), Verdict::Cancel); // too short
    }

    #[test]
    fn drag_left_is_forward_in_ltr_backward_in_rtl() {
        assert_eq!(step(-80.0, false), 1);
        assert_eq!(step(80.0, false), -1);
        assert_eq!(step(-80.0, true), -1);
        assert_eq!(step(80.0, true), 1);
    }

    #[test]
    fn panels_cover_pages_transitions_and_edges() {
        assert_eq!(panel(0, 5, true, true), Panel::Page(0));
        assert_eq!(panel(4, 5, true, true), Panel::Page(4));
        assert_eq!(panel(-1, 5, true, true), Panel::TransitionPrev);
        assert_eq!(panel(5, 5, true, true), Panel::TransitionNext);
        assert_eq!(panel(-1, 5, false, true), Panel::DeadEnd);
        assert_eq!(panel(5, 5, true, false), Panel::DeadEnd);
        assert_eq!(panel(-2, 5, true, true), Panel::Empty);
        assert_eq!(panel(6, 5, true, true), Panel::Empty);
        assert_eq!(panel(0, 0, true, true), Panel::Empty);
    }

    #[test]
    fn bounds_exclude_missing_neighbours() {
        assert_eq!(bounds(5, true, true), (-1, 5));
        assert_eq!(bounds(5, false, false), (0, 4));
    }

    #[test]
    fn flick_velocity_uses_recent_window_only() {
        let mut f = Flick::default();
        f.push(0.0, 0.0);
        f.push(-10.0, 1000.0); // old sample evicted by the next push
        f.push(-110.0, 1100.0);
        assert!((f.velocity() - (-1.0)).abs() < 1e-9);
        f.clear();
        assert_eq!(f.velocity(), 0.0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p yomu-ui pager`
Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-ui/src/pager.rs crates/yomu-ui/src/lib.rs
git commit -m "feat(ui): pure pager decisions for the sliding paged reader"
```

### Task 3: Track / panel / transition CSS

**Files:**
- Modify: `crates/yomu-web/styles.css` (paged-mode section, around `.reader-stage`)

- [ ] **Step 1: Replace the `.reader-stage` block with track styles**

`.reader-stage` (the single-scroller rules) is superseded. New rules —
the fit classes move to the track so the existing `.fit-* .reader-page`
descendant selectors keep working:

```css
/* ---- paged mode: three-panel sliding track ---- */

/* clips the track; owns the tap cursor */
.reader-pager {
  flex: 1;
  min-height: 0;
  overflow: hidden;
  display: flex;
  cursor: pointer;
}

.reader-track {
  display: flex;
  width: 300%;
  transform: translateX(-33.3333%);
}

.reader-track.snap {
  transition: transform 0.2s ease-out;
}

/* each panel is one viewport; overflowing fits scroll inside it */
.reader-panel {
  flex: 0 0 33.3333%;
  min-width: 0;
  display: flex;
  overflow: auto;
  scrollbar-width: thin;
  /* native vertical scroll stays (fit-width); horizontal drags and
     pinches are ours to interpret */
  touch-action: pan-y;
}

/* chapter-boundary transition panel */
.reader-transition {
  margin: auto;
  padding: 2rem;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  align-items: center;
  text-align: center;
  color: var(--muted);
}

.reader-transition strong {
  color: var(--text);
}
```

Keep `.reader-page { margin: auto; }` and the `.fit-* .reader-page`
rules as they are.

- [ ] **Step 2: Commit** (with Task 5, once the markup matches)

### Task 4: Reader — virtual position, animated turns, `?page=end`

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs` (top of `ReaderInner`)

- [ ] **Step 1: Hoist pager signals and the turn requester**

At `ReaderInner` top level (signals must survive the `pages.get()`
re-render), next to `page`:

```rust
// Paged mode's virtual position: -1 and page_count are the chapter
// transition panels; real pages mirror into `page` (which keeps
// driving the counter, the progress bar, and progress reports).
let pos = RwSignal::new(initial_page as i64);
let drag = RwSignal::new(0.0_f64); // live drag offset, px
let snap = RwSignal::new(None::<i64>); // animating toward pos+delta
```

`?page=end` support where `initial_page` is parsed: `"end"` parses to 0
plus a flag:

```rust
let page_query = use_query_map().get_untracked().get("page");
let wants_end = page_query.as_deref() == Some("end");
let initial_page: u32 = page_query.and_then(|p| p.parse().ok()).unwrap_or(0);
```

In the existing `opened` effect, before the report:

```rust
if wants_end {
    let last = page_count().saturating_sub(1);
    page.set(last);
    pos.set(last as i64);
}
```

Neighbour booleans and navigation targets (top level, after
`neighbours`):

```rust
let neighbour_ids = move || neighbours().unwrap_or((None, None));
```

Replace the paged arm of `go_page`/key handling with a single animated
requester used by keys, tap zones, pill ‹ ›, and drag commits:

```rust
let request_turn = {
    let navigate = navigate.clone();
    move |delta: i64| {
        if snap.get_untracked().is_some() {
            return; // mid-animation
        }
        let count = page_count();
        if count == 0 {
            return;
        }
        let (prev, next) = neighbour_ids();
        let (lo, hi) = pager::bounds(count, prev.is_some(), next.is_some());
        let current = pos.get_untracked();
        let target = current + delta;
        if target > hi {
            // one more turn on the transition panel enters the chapter
            if current == count as i64 && let Some(next) = next {
                navigate(&format!("/read/{manga_id}/{next}"), Default::default());
            }
            return;
        }
        if target < lo {
            if current == -1 && let Some(prev) = prev {
                navigate(&format!("/read/{manga_id}/{prev}?page=end"), Default::default());
            }
            return;
        }
        snap.set(Some(delta));
    }
};
```

Committing after the animation (called from `transitionend`):

```rust
let commit_pos = {
    let report = report.clone();
    move |target: i64| {
        pos.set(target);
        if (0..page_count() as i64).contains(&target) {
            let p = target as u32;
            if p != page.get_untracked() {
                page.set(p);
                report(chapter_id, p);
            }
        }
    }
};
```

The window key handler and `go_page`'s `ReaderMode::Paged` arm call
`request_turn(±forward)`; the old `turn` closure stays for vertical
mode only.

- [ ] **Step 2: Check it compiles** — `cargo check -p yomu-ui --target wasm32-unknown-unknown` (markup still uses the old single stage; finish Task 5 before running).

### Task 5: Reader — three-panel markup and gestures

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs` (the `ReaderMode::Paged` view arm)

- [ ] **Step 1: Replace the single stage with the track**

Panel content from the pure module; the DOM is physical (left, center,
right), so slot positions depend on direction:

```rust
let slot = move |physical: i64| -> i64 {
    let rtl = dir.get() == ReaderDirection::Rtl;
    pos.get() + if rtl { -physical } else { physical }
};
let panel_view = {
    let client = client_paged.clone();
    move |position: i64| {
        let (prev, next) = neighbour_ids();
        match pager::panel(position, page_count(), prev.is_some(), next.is_some()) {
            pager::Panel::Page(n) => {
                let src = page_source(&client, chapter_id, n);
                view! { <img class="reader-page" src=src alt=""/> }.into_any()
            }
            pager::Panel::TransitionNext => view! {
                <div class="reader-transition">
                    <span>"Finished:"</span>
                    <strong>{chapter_title}</strong>
                    <span>"Next up — keep turning:"</span>
                    <strong>{next_title}</strong>
                </div>
            }
            .into_any(),
            pager::Panel::TransitionPrev => /* mirrored wording */,
            pager::Panel::DeadEnd => view! {
                <div class="reader-transition">
                    <span>"No more chapters this way"</span>
                    <a class="button" href=format!("/manga/{manga_id}")>
                        "Back to the chapter list"
                    </a>
                </div>
            }
            .into_any(),
            pager::Panel::Empty => ().into_any(),
        }
    }
};
```

(`next_title`/`prev_title` read the neighbour's title from `detail`
the same way `chapter_title` does. The center panel keeps the zoom/pan
`style:transform` on its `<img>`; neighbours don't zoom.)

Markup:

```rust
view! {
    <div class="reader-pager"
        on:click=on_click
        on:touchstart=on_touchstart
        on:touchmove=on_touchmove
        on:touchend=on_touchend
        on:wheel=on_wheel
    >
        <div
            class="reader-track"
            class:snap=move || snap.get().is_some()
            class:fit-screen=move || fit.get() == ReaderFit::Screen
            class:fit-width=move || fit.get() == ReaderFit::Width
            class:fit-original=move || fit.get() == ReaderFit::Original
            style:transform=track_transform
            on:transitionend=on_transitionend
        >
            <div class="reader-panel">{move || panel_view(slot(-1))}</div>
            <div class="reader-panel" node_ref=stage>{move || center_view()}</div>
            <div class="reader-panel">{move || panel_view(slot(1))}</div>
        </div>
    </div>
}
```

Transform: a snap animates one panel width in the *physical* direction
(delta flips under RTL); a drag follows the finger:

```rust
let track_transform = move || {
    let rtl = dir.get() == ReaderDirection::Rtl;
    match snap.get() {
        Some(delta) => {
            let shift = if rtl { -delta } else { delta } as f64;
            format!("translateX({}%)", -33.3333 - shift * 33.3333)
        }
        None => {
            let px = drag.get();
            if px == 0.0 {
                "translateX(-33.3333%)".into()
            } else {
                format!("translateX(calc(-33.3333% + {px}px))")
            }
        }
    }
};
let on_transitionend = {
    let commit_pos = commit_pos.clone();
    move |ev: leptos::ev::TransitionEvent| {
        if ev.property_name() != "transform" {
            return;
        }
        if let Some(delta) = snap.get_untracked() {
            snap.set(None);
            drag.set(0.0);
            commit_pos(pos.get_untracked() + delta);
        }
    }
};
```

The existing scroll-reset / zoom-reset effects key on `pos` instead of
`page` and reset the center panel (`stage` node ref).

- [ ] **Step 2: Gesture capture**

`Gesture` gains `h_capture: bool`. `on_touchmove`'s one-finger arm, when
`zoom == 1`:

```rust
if !g.h_capture && (dx.abs() > 10.0 || dy.abs() > 10.0) {
    gesture.update_value(|g| g.moved = true);
    if dx.abs() > dy.abs() {
        gesture.update_value(|g| g.h_capture = true);
    }
}
if gesture.with_value(|g| g.h_capture) && snap.get_untracked().is_none() {
    ev.prevent_default();
    flick.update_value(|f| f.push(x, ev.time_stamp()));
    let rtl = dir.get_untracked() == ReaderDirection::Rtl;
    let target = pos.get_untracked() + pager::step(dx, rtl);
    let (prev, next) = neighbour_ids();
    let (lo, hi) = pager::bounds(page_count(), prev.is_some(), next.is_some());
    // beyond the last panel there is nothing to reveal: damp
    let beyond = target < lo - 1 || target > hi + 1
        || pager::panel(target, page_count(), prev.is_some(), next.is_some())
            == pager::Panel::Empty;
    drag.set(if beyond { pager::damp(dx) } else { dx });
}
```

(`flick: StoredValue<pager::Flick>`, cleared in `on_touchstart`, which
also pushes the first sample and resets `h_capture`.)

`on_touchend` replaces today's threshold block: when `h_capture` was
set and a drag is live,

```rust
let width = window().inner_width().ok().and_then(|w| w.as_f64()).unwrap_or(1.0);
let velocity = flick.with_value(|f| f.velocity());
let dx = /* changed_touches x - start x, as today */;
let rtl = dir.get_untracked() == ReaderDirection::Rtl;
match pager::verdict(dx, width, velocity) {
    pager::Verdict::Commit => request_turn(pager::step(dx, rtl)),
    pager::Verdict::Cancel => {}
}
// request_turn refused (dead end / navigation) or cancel: spring back
if snap.get_untracked().is_none() {
    if drag.get_untracked() == 0.0 {
        // no transform change → no transitionend; reset directly
    } else {
        snap.set(Some(0));
    }
}
```

Tap zones (`on_click`) call `request_turn(if forward { 1 } else { -1 })`
through the same `rtl` mapping as today.

- [ ] **Step 3: Run checks**

Run: `just check && cargo test -p yomu-ui`
Expected: clean; pager + chapter_actions + format tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/yomu-ui/src/pages/reader.rs crates/yomu-web/styles.css
git commit -m "feat(reader): swipe pager with sliding track and chapter transitions"
```

### Task 6: Full workspace verification

- [ ] **Step 1:** `cargo test --workspace --exclude yomu-shell` — all suites pass.
- [ ] **Step 2:** `just check` — fmt, clippy, wasm check clean. Commit any fmt fallout.

### Task 7: Headless E2E verification

Scratch server on a 479x port (`YOMU_CONFIG` in the scratchpad,
`static_dir` = `crates/yomu-web/dist` after `trunk build`), headless
firefox via bun + puppeteer-core (`executablePath`
`/etc/profiles/per-user/tibo/bin/firefox`), touch via
`page.touchscreen` / dispatched `TouchEvent`s.

- [ ] Long horizontal drag commits: page counter advances by one; mid-drag screenshot shows both pages.
- [ ] Short drag cancels: counter unchanged.
- [ ] RTL flips the mapping (same drag turns the other way).
- [ ] Dragging forward on the last page reveals the transition panel; a second turn navigates to the next chapter (URL + counter).
- [ ] `?page=end` opens the previous chapter's last page (via the transition panel backward).
- [ ] Pinch to zoom, then drag: pans, does not turn.
- [ ] Tap zones and ArrowLeft/ArrowRight still turn (animated).
- [ ] Chapter list: current chapter shows the accent edge bar and its download outline (screenshot).
- [ ] Kill scratch servers: `pkill -x -u tibo yomu-server`.

### Task 8: PR

- [ ] `git push -u origin feature/swipe-pager`; `gh pr create` into `develop` (neutral wording, standard footer); `gh pr merge --merge --auto --delete-branch`.
