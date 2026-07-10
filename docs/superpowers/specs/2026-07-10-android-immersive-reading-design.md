# Android immersive reading — design

## Problem

On Android the system status and navigation bars stay visible while
reading a chapter. The reader's own fullscreen button uses the web
Fullscreen API, which wry's `RustWebChromeClient` rejects on Android
(`onShowCustomView` immediately calls `onCustomViewHidden()`), so it is a
no-op there. Reading should be immersive: pages edge-to-edge, no system
bars.

## UX decision

System bars follow the reader's own chrome overlay (the one toggled by a
center tap): overlay visible → bars visible; overlay hidden → bars
hidden. One mental model — tap shows everything, tap again gives pure
pages. `chrome` starts `true`, so entering the reader shows everything
until the first tap. Leaving the reader always restores the bars.
Edge-swipe peeks the bars transiently while immersive (standard Android
behavior).

## Approach

A JavaScript bridge in the user-owned `MainActivity.kt` — no Rust
changes, no Tauri plugin:

- `MainActivity` overrides the `onWebViewCreate(webView)` hook exposed by
  the generated `WryActivity` and calls
  `webView.addJavascriptInterface(bridge, "YomuAndroid")`.
- The bridge exposes `@JavascriptInterface fun setImmersive(on: Boolean)`.
  JS-interface calls arrive on a worker thread, so the body hops to the
  UI thread (`runOnUiThread`).
- **on**: `WindowInsetsControllerCompat(window, content).hide(systemBars())`
  with `systemBarsBehavior = BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`, and
  an `immersive` flag makes the activity's existing insets listener stop
  padding the content view (pages truly edge-to-edge, including under a
  display cutout). Re-request insets after flipping the flag.
- **off**: `show(systemBars())`, clear the flag, re-request insets so the
  edge-to-edge padding returns.

UI side (`yomu-ui`):

- `offline.rs` (next to the existing Tauri shell bridge): a
  `set_immersive(on: bool)` helper that looks up `window.YomuAndroid` via
  `js_sys::Reflect` and invokes `setImmersive`. Silently a no-op when the
  object is absent — desktop shell, browser, or an APK older than the
  bridge.
- `reader.rs`: an `Effect` mirrors the chrome signal
  (`set_immersive(!chrome.get())`) and `on_cleanup` calls
  `set_immersive(false)` so navigating away — back gesture included —
  always restores the bars.

Untouched: the web Fullscreen API button (still useful on desktop), the
Rust shell, the web bundle behavior in plain browsers.

## Testing

- `cargo check -p yomu-ui --target wasm32-unknown-unknown` and the
  workspace test suite (the helper is wasm-only glue; no logic worth a
  unit test beyond compilation).
- Kotlin compiles via the Android release build (gradle) — the APK is
  produced by the release workflow.
- On-device verification (user): enter a chapter → tap center → bars and
  overlay disappear together; tap again → both return; back out of the
  reader with chrome hidden → bars restored; edge-swipe while immersive →
  bars peek and re-hide.

## Rollout

Ships in the next release's APK; web/desktop bundles are unaffected
(no-op bridge lookup). No server or config change.
