# Android Immersive Reading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hide the Android system bars while reading pages, synced to the reader's chrome overlay (center tap shows/hides both).

**Architecture:** A `@JavascriptInterface` bridge (`window.YomuAndroid`) installed by the user-owned `MainActivity.kt` via the `onWebViewCreate` hook; the Leptos reader mirrors its `chrome` signal into `offline::set_immersive`, a no-op wherever the bridge is absent (desktop, browser, old APKs). No Rust shell or Tauri plugin changes.

**Tech Stack:** Kotlin (AndroidX `WindowInsetsControllerCompat`), Leptos/wasm (`js_sys::Reflect`).

**Spec:** `docs/superpowers/specs/2026-07-10-android-immersive-reading-design.md`

Branch `feature/immersive-reading`. Commit messages end with the standard Co-Authored-By + Claude-Session trailer used across this repo.

---

### Task 1: JS-side bridge helper

**Files:**
- Modify: `crates/yomu-ui/src/offline.rs` (Tauri shell bridge section, after `shell_page_url`, ~line 281)

No unit test — this is wasm-only glue around `js_sys::Reflect`; the compiler and the wasm check are the gate (spec's testing section).

- [ ] **Step 1: Add the helper**

```rust
/// Android shell: hide/show the system bars while reading. The bridge is
/// installed by the Android activity as `window.YomuAndroid`; anywhere it
/// is absent (desktop shell, plain browser, an APK older than the bridge)
/// this is a no-op.
pub fn set_immersive(on: bool) {
    use leptos::wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(bridge) = js_sys::Reflect::get(&window, &"YomuAndroid".into()) else {
        return;
    };
    let Ok(method) = js_sys::Reflect::get(&bridge, &"setImmersive".into()) else {
        return;
    };
    let Ok(method) = method.dyn_into::<js_sys::Function>() else {
        return;
    };
    let _ = method.call1(&bridge, &on.into());
}
```

- [ ] **Step 2: Verify it compiles for wasm**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown 2>&1 | tail -3`
Expected: `Finished` with no errors. (A `dead_code` warning is fine until Task 2 wires the caller — native target only; wasm builds the pages that will call it.)

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-ui/src/offline.rs
git commit -m "feat(ui): shell bridge helper for Android immersive mode"
```

---

### Task 2: Reader mirrors chrome into the bridge

**Files:**
- Modify: `crates/yomu-ui/src/pages/reader.rs` (after the `toggle_fullscreen` closure, ~line 279; the `chrome` signal is declared at ~line 88)

- [ ] **Step 1: Add the effect and cleanup**

Insert after the `toggle_fullscreen` closure:

```rust
    // Android shell: system bars follow the reader chrome (no-op
    // elsewhere — see offline::set_immersive). Cleanup restores the bars
    // however the reader is left, back gesture included.
    Effect::new(move |_| {
        offline::set_immersive(!chrome.get());
    });
    on_cleanup(|| offline::set_immersive(false));
```

Check the file's existing imports: it already uses `offline::` (reader fit/direction persistence) and Leptos prelude items (`Effect`, `on_cleanup` come from `leptos::prelude::*`). Add nothing unless the compiler asks.

- [ ] **Step 2: Verify compile + tests**

Run: `cargo check -p yomu-ui --target wasm32-unknown-unknown 2>&1 | tail -3 && cargo test -p yomu-ui 2>&1 | tail -3`
Expected: both clean.

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-ui/src/pages/reader.rs
git commit -m "feat(ui): sync reader chrome to Android immersive mode"
```

---

### Task 3: Kotlin bridge in MainActivity

**Files:**
- Modify: `crates/yomu-shell/gen/android/app/src/main/java/xyz/tdbm/yomu/MainActivity.kt` (whole file; it is 25 lines today)

- [ ] **Step 1: Replace the file**

```kotlin
package xyz.tdbm.yomu

import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

// Android 15+ enforces edge-to-edge for apps targeting SDK 35+, and the
// manifest opt-out is gone when targeting 36 — so not calling
// enableEdgeToEdge() is not enough: the webview still draws under the
// status/navigation bars. Pad the content view by the system-bar (and
// display-cutout) insets instead, keeping the whole UI reachable —
// except while the reader asks for immersion, where pages go truly
// edge-to-edge.
class MainActivity : TauriActivity() {
    private var immersive = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            if (immersive) {
                view.setPadding(0, 0, 0, 0)
            } else {
                val bars = insets.getInsets(
                    WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
                )
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        webView.addJavascriptInterface(ImmersiveBridge(), "YomuAndroid")
    }

    // Reader immersion: the UI mirrors its chrome overlay into this —
    // bars hidden while reading pages, restored with the overlay.
    // JS-interface calls arrive on a worker thread, hence runOnUiThread.
    inner class ImmersiveBridge {
        @JavascriptInterface
        fun setImmersive(on: Boolean) {
            runOnUiThread {
                immersive = on
                val content = findViewById<View>(android.R.id.content)
                val controller = WindowCompat.getInsetsController(window, content)
                if (on) {
                    controller.systemBarsBehavior =
                        WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                    controller.hide(WindowInsetsCompat.Type.systemBars())
                } else {
                    controller.show(WindowInsetsCompat.Type.systemBars())
                }
                ViewCompat.requestApplyInsets(content)
            }
        }
    }
}
```

- [ ] **Step 2: Compile the Kotlin if a local Android toolchain exists**

Check how the repo builds the APK: `grep -in "apk\|android" justfile .github/workflows/*.yml | head`. If a local gradle/SDK path exists (e.g. inside `nix develop .#tauri`), run the Kotlin compile task (`./gradlew :app:compileArm64ReleaseKotlin` or the closest listed by `./gradlew tasks`) from `crates/yomu-shell/gen/android`. If the toolchain is CI-only, state that explicitly in the task notes and rely on the release workflow build — do NOT claim the Kotlin compiled.

- [ ] **Step 3: Commit**

```bash
git add crates/yomu-shell/gen/android/app/src/main/java/xyz/tdbm/yomu/MainActivity.kt
git commit -m "feat(android): immersive system bars while reading"
```

---

### Task 4: Verification and PR

- [ ] **Step 1: Full checks**

Run: `just check 2>&1 | tail -3` and `cargo test --workspace --exclude yomu-shell 2>&1 | grep -cE "test result: ok"`
Expected: check clean; all suites ok.

- [ ] **Step 2: Desktop no-op sanity**

The bridge object doesn't exist outside Android, so the only behavior to confirm off-device is "nothing breaks": build the web bundle (`just build-web`), serve it with a scratch server config (`static_dir` at `crates/yomu-web/dist`, scratch db, port 4791), open a chapter in the reader via headless firefox (bun + puppeteer-core, executablePath `/etc/profiles/per-user/tibo/bin/firefox`), tap center (click mid-viewport), screenshot — reader works exactly as before, no console errors about `YomuAndroid`.

- [ ] **Step 3: Push and open the PR into develop**

```bash
git push -u origin feature/immersive-reading
gh pr create --base develop --title "feat: immersive reading on Android" --body "..."
```

Body: UX summary (bars follow reader chrome, transient-swipe peek), the mechanism (JS bridge in MainActivity, no Rust change), the wry-rejects-web-fullscreen rationale, and an explicit note that on-device verification happens with the release APK. Standard footer. Enable auto-merge (`gh pr merge <n> --merge --auto --delete-branch`).

---

## Self-review notes

- Spec coverage: bridge helper (T1), reader wiring incl. cleanup-on-leave (T2), Kotlin immersive + insets flag (T3), compile/no-op verification and honest on-device deferral (T4).
- `WindowCompat.getInsetsController` never returns null (unlike `ViewCompat.getWindowInsetsController`), so no null-handling needed in Kotlin.
- Type consistency: `offline::set_immersive(bool)` used in T2 matches T1's signature.
