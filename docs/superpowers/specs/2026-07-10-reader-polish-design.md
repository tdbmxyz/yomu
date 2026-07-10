# Reader polish — design

Four user-reported issues in the 1.6.0 reader, one spec.

## 1. Seamless chrome toggle (no viewport resize)

The web chrome already overlays the pages (`position: absolute`), so
the shift on toggle is not a web reflow — it's the Android shell: the
activity pads the content view by the system-bar insets, so hiding the
bars (immersive) *resizes* the webview and the page jumps.

Fix: while the reader is open the webview is permanently edge-to-edge —
the system bars overlay the page when shown and slide away when hidden,
never resizing anything:

- `MainActivity` gains a `setReading(Boolean)` bridge call (mirroring
  `setImmersive`): while reading, content padding stays 0 and the
  inset heights are pushed to the page as CSS variables
  (`--shell-inset-top/-bottom`, in CSS px); outside the reader the
  padded behavior is unchanged.
- The reader mounts with `set_reading(true)` and cleans up with
  `set_reading(false)` (no-op outside the Android shell, guarded like
  `set_immersive`). `setImmersive` keeps toggling only bar visibility.
- CSS offsets the reader's own overlays by the variables:
  `.reader-progress`/`.reader-top` by `--shell-inset-top`,
  `.reader-bottom`/`.reader-menu` by `--shell-inset-bottom`.
- Transition / dead-end panels center their content, so nothing they
  say hides under the bars; page edges running under the translucent
  bars until a tap hides them is the normal immersive-reader look.
- Entering/leaving the reader still resizes once (padding 0 ↔ bars) —
  that's a navigation, not the reading toggle.

## 2. Remove the fullscreen pill button

One tap hides the UI, which is the real fullscreen use; browsers have
F11, desktop has the OS, Android is fullscreen by default. The `⛶`
button and its `toggle_fullscreen` handler go away.

## 3. Back returns to the chapter list

Chapter-to-chapter navigation inside the reader must not push history
entries: crossing through a transition panel and the `|‹ ›|` pill
buttons navigate with `replace: true` (the vertical strip already uses
`history.replaceState`). System back therefore always leaves the reader
to wherever it was opened from — normally the chapter list. Opening the
reader from the list keeps its one pushed entry.

## 4. Vertical scroll glue (leap + transition jump-back)

Two symptoms, both suspected in the image-load scroll compensation:

- several small phone slides sometimes become one big quick leap;
- crossing into the next chapter sometimes still jumps back several
  pages (second attempt works).

This part is empirical: build a headless repro that scrolls at reading
pace with monkeypatched `scrollBy`/`scrollTo` logging (same harness as
the 1.4.0 investigation), identify the actual mis-compensation, then
fix with a regression repro before/after. Candidate hypotheses to test,
not assume: compensation firing for images that straddle the midline
during momentum scrolling; double-compensation when several images load
in one frame; the `?page=…` opening scroll racing `load_next`.

## Testing

- Chrome overlay: headless — reader-page bounding box identical with
  chrome shown and hidden; transition panel content fully visible with
  chrome shown.
- History: headless — open list → chapter → cross two chapters forward
  → `history.back()` lands on the manga page.
- Vertical: scripted repro above, before/after.

## Rollout

Client-only. No server or config changes.
