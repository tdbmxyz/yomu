# Swipe pager for paged reading — design

## Problem

Paged mode swaps a single `<img>` on finger-up. There is no drag
feedback, the neighbor page is invisible until the turn, and adjacent
pages only start loading after the turn. The reader should page like
Tachidesk: the page follows the finger, the next/previous page is
revealed underneath, and the turn commits once the slide passes a
threshold.

A small independent fix rides along: `.chapter-item.current` paints an
accent border that hides the row's download-state outline.

## Three-panel sliding track

Paged mode renders a track of three full-viewport panels — previous,
current, next — positioned so the middle panel fills the screen
(`translateX(-100%)` plus the live drag offset). Each panel is its own
stage: the fit classes apply per panel, an overflowing fit scrolls
inside its panel, and the scroll resets when the position changes
(today's reset effect, per panel). The neighbor `<img>`s are mounted DOM
elements, so adjacent pages preload for free.

## Virtual position

The pager position indexes `[-1, 0 .. count-1, count]`:

- `0..count` are the chapter's pages; landing on one reports progress
  exactly as today.
- `-1` and `count` are transition panels ("Finished: <chapter> — Next:
  <chapter>" / the reverse), rendered in the neighbor slot when the
  current position sits at the chapter edge. Transition panels never
  report progress.
- A forward turn while on `count` navigates (SPA) to the next chapter at
  page 0. A backward turn while on `-1` navigates to the previous
  chapter with a new `?page=end` query, resolved to the last page once
  that chapter's page count loads.
- With no neighbor chapter, the panel says so ("No next chapter" plus a
  back-to-the-list link) and dragging past it rubber-bands: the offset
  is damped (÷3) and always snaps back.

## Drag mechanics

- Touch only, and only at zoom 1 (a zoomed drag keeps panning; pinch
  and click behavior are unchanged).
- A one-finger gesture captures the drag once it shows horizontal
  intent: |dx| > |dy| past ~10px. From then on touchmove calls
  `prevent_default()` (no vertical scroll) and the track follows the
  finger 1:1. Vertical-intent gestures are never captured, so fit-width
  vertical scrolling is untouched.
- On release: **commit** if |dx| > 30% of the viewport width, or the
  gesture was a flick (velocity over the last ~100ms above 0.5 px/ms
  with at least 40px total travel); otherwise **cancel**.
- Commit/cancel animate the track with a ~200ms `transform` transition;
  on `transitionend` the position signal updates and the track
  re-centers with the transition disabled (no visible jump — the new
  center shows the same image the animation ended on).
- RTL reverses which physical side holds the next page — the same
  mapping the tap zones already use.
- Tap zones, arrow keys, and the ‹ › pill buttons run the same animated
  transition instead of hard-swapping the image.

## Pure logic: `crates/yomu-ui/src/pager.rs`

Wasm-free module the component wires DOM events to:

- `Release { dx, width, velocity } → Verdict` (`Commit` / `Cancel`).
- Drag sign × direction (LTR/RTL) → position delta (+1/−1).
- `panel(position, count, prev_exists, next_exists) → PanelContent`
  (`Page(n)` / `TransitionPrev` / `TransitionNext` / `DeadEnd`), used
  for all three slots.
- Rubber-band damping for dead-end drags.

All of it unit-tested (threshold table, flick cases, RTL mapping,
edge/virtual-index panels).

## Chapter-list fix

`.chapter-item.current` drops its `border-color` override and gains an
inset accent bar on the left edge (`box-shadow: inset 3px 0 0
var(--accent)`). No layout shift; all four border sides stay free for
the download-state outline, including the split-gradient `dl-both`
background.

## Testing

- Unit: `pager.rs` decision/mapping/panel tables.
- Headless-firefox E2E on a scratch server: long drag commits (counter
  advances), short drag cancels (same page), RTL reverses the mapping,
  edge drag reveals the transition panel and a second turn lands in the
  next chapter (URL + counter), `?page=end` opens the last page,
  zoomed drag still pans instead of turning, current-chapter row shows
  both the accent edge bar and its download outline.

## Rollout

Client-only (web + shells; APK-relevant). No server or config changes.
