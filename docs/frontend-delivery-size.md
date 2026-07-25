# Shrinking a Leptos CSR delivery — playbook

Notes from cutting chaos's dashboard cold load from 5 899 053 B to 580 959 B
(−90%) on 2026-07-25. Same stack as yomu (Leptos 0.8 CSR + trunk + axum
`ServeDir` + Tauri + a nix flake), so nearly everything transfers. Each item
records what to check, the fix, the trap, and **yomu's status as of
2026-07-25** — measured, not assumed.

Ordered by payoff per unit of effort. The first three are the whole story;
the rest is polish.

---

## 0. Measure the wire, not the repo

The repo tells you nothing about what the browser pays. Three commands do:

```bash
# what is actually served, per asset
curl -sI -H 'Accept-Encoding: br, gzip' http://host/asset.wasm \
  | grep -iE 'content-length|content-encoding|cache-control|vary'

# the compression ceiling for a file you already have
brotli -q 11 -c dist/*_bg.wasm | wc -c

# where the wasm's bytes actually are
nix-shell -p wabt --run 'wasm-objdump -h dist/*_bg.wasm'
```

`wasm-objdump -h` is the highest-signal one. A `Custom … "name"` section means
symbol names are shipping to users; its size is pure waste. On chaos it was
724 430 B of a 4 769 951 B file — 15% — and its presence is *proof* that the
release pipeline isn't stripping, which is faster than reading build config.

Do this before touching anything, and write the numbers down. Every claim below
came from a measurement, and two intuitions turned out wrong (§4, §8).

**yomu now:** wasm 3 604 377 B, brotli ceiling 670 382 B (5.4×), `name`
section 548 315 B present, `producers` present. Nothing is compressed on the
wire and nothing carries `Cache-Control`.

---

## 1. Compression: the one big lever

A Leptos wasm bundle compresses 5–6×. If nothing in the chain compresses, you
are shipping five sixths air, and no amount of code-level work competes with
turning it on.

**Do it at build time, not per request.** Three options, in the order we
rejected them:

- `tower_http::CompressionLayer` — re-brotlis megabytes on *every* cold
  request. At `-q 11` that is seconds of CPU per visitor; at a cheap quality
  you give up most of the win.
- The reverse proxy's `compress` middleware — only covers requests that
  traverse it. LAN clients and the Tauri/Android shells talk to the server
  directly and stay uncompressed. (For chaos it also lived in `/etc/nixos`,
  outside the repo.)
- **Precompressed siblings generated once in the nix build**, served by
  `ServeDir`. Zero per-request cost, so you can afford `-q 11`. This is the
  one to use.

Build side — a `runCommand` wrapping the trunk dist:

```nix
yomu-web-static = pkgs.runCommand "yomu-web-static-${version}" {
  nativeBuildInputs = [pkgs.brotli pkgs.gzip];
} ''
  cp -r ${yomu-web} $out
  chmod -R u+w $out
  find $out -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' \
    -o -name '*.html' -o -name '*.json' -o -name '*.svg' -o -name '*.map' \) \
    -print0 | while IFS= read -r -d "" f; do
    brotli -q 11 -f -o "$f.br" "$f"
    gzip -9 -c "$f" > "$f.gz"
  done
'';
```

Serve side:

```rust
let index = ServeFile::new(dir.join("index.html"))
    .precompressed_br()
    .precompressed_gzip();
let files = ServeDir::new(dir)
    .precompressed_br()
    .precompressed_gzip()
    .fallback(index);
```

`ServeDir` picks the encoding from `Accept-Encoding` and **falls back to the
identity file when a sibling is missing**, so a plain local `trunk build` dist
keeps working with no conditional logic. That fallback deserves a test (§7).

### The trap: keep the compressed variant out of the Tauri dist

`yomu-desktop` bakes the dist into its binary via `generate_context!`
(`flake.nix:156-157` copies `${yomu-web}`). Compressed siblings there are ~1 MB
of files the Tauri asset protocol never serves — they inflate the desktop binary
and the APK for nothing. So keep **two** packages: the plain dist for the shell,
the precompressed one for the server, with `services.<app>.webPackage`
defaulting to the latter. Verify with `nix derivation show .#<app>-desktop`
that only the plain one is an input.

**yomu now:** neither exists. `crates/yomu-server/src/api/mod.rs:96` is
`ServeDir::new(dir).fallback(ServeFile::new(index))`, bare.

---

## 2. Wire up the size profile — it beat wasm-opt

Both repos define `[profile.wasm-release]` (`opt-level = "z"`, `panic =
"abort"`) and **neither ever used it**: nothing passes the profile to trunk, so
release builds run at `opt-level = 3`. Dead config that looks like a solved
problem is worse than no config, because it stops you looking.

Fix, plus wasm-opt, in one line of `index.html`:

```html
<link data-trunk rel="rust" data-cargo-profile-release="wasm-release"
      data-wasm-opt="z" data-wasm-opt-params="--strip-debug --strip-producers" />
```

Adding an explicit `rel="rust"` link where there was none is fine — trunk
defaults to the crate in the same directory. `data-cargo-profile-release`
applies to release only, so `trunk serve` keeps its fast dev profile.

**It must go through trunk, never a post-build hook.** Trunk computes the
subresource-integrity hash *after* wasm-opt and embeds it in the generated
`index.html`. A hook that rewrites the wasm afterwards yields an SRI mismatch
and a page that refuses to boot — the browser reports "Failed to find a valid
digest in the integrity attribute".

**The surprise:** we expected wasm-opt stripping to dominate and it didn't.
Measured on chaos, 4 771 483 B baseline:

| step | bytes |
| --- | --- |
| baseline (`opt-level = 3`) | 4 771 483 |
| manual `wasm-opt -Oz --strip-debug` alone | 3 594 251 |
| both, i.e. `+ [profile.wasm-release]` | **1 878 053** |
| the above, brotli `-q 11` | **562 892** |

Re-measured on yomu (2026-07-25), and the ordering holds — the profile
contributes more than wasm-opt does, on a different codebase:

| step | raw | brotli -q 11 |
| --- | --- | --- |
| baseline (`opt-level = 3`) | 3 698 968 | 685 893 |
| `wasm-opt -Oz --strip-debug --strip-producers` alone | 2 774 889 | 613 145 |
| both | **1 452 288** | **450 159** |
| `opt-level = "s"` instead of `"z"`, both | 1 818 965 | 507 605 |

wasm-opt bought 924 KB, the profile another 1 323 KB. `"s"` cost +25% raw and
+12.7% brotli over `"z"` — far outside the band where the extra inlining would
be worth it, so yomu ships `"z"`.

`opt-level = "z"` did more than twice what wasm-opt did. It is also the change
most likely to cost runtime speed — `"s"` is the middle setting if the UI feels
sluggish — but on chaos nothing was perceptibly slower.

`panic = "abort"` is **not** a behavior change on `wasm32-unknown-unknown`:
that target's spec already hardcodes `"panic-strategy": "abort"`, so setting it
in the profile changes nothing. (Measured on yomu 2026-07-25 via `rustc --print
target-spec-json`.) The line is harmless and worth keeping for a future
non-wasm target, but do not spend a verification pass on unwinding — and if a
profile sets it explicitly, say so rather than claiming it "rides along via
`inherits`".

What *is* worth checking once: that `console_error_panic_hook` still installs.
A panic hook is invoked under abort, so panic reporting survives.

**yomu now:** no `rel="rust"` link, so the profile is unused and the wasm is
unstripped. Extrapolating from chaos's ratios, 3 604 377 B should land near
1.4 MB raw / ~450 KB brotli.

---

## 3. `Cache-Control` on fingerprinted filenames

Trunk emits content-hashed names (`yomu-web-<16 hex>_bg.wasm`,
`styles-<16 hex>.css`). Those can be pinned for a year; a change always
arrives under a new URL. Everything else — `index.html`, SPA routes,
hand-vendored files — must revalidate.

Without this you get `last-modified: Thu, 01 Jan 1970 00:00:01 GMT` (the nix
store timestamp) and no validator beyond it: conditional requests do return
304, but every navigation costs a round trip and every force-reload refetches
the whole bundle.

Keep the decision a pure function so it is trivially testable:

```rust
pub(crate) fn cache_control_for(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let hashed = name
        .split(['-', '_', '.'])
        .any(|seg| seg.len() == 16 && seg.chars().all(|c| c.is_ascii_hexdigit()));
    if hashed { IMMUTABLE } else { REVALIDATE }
}
```

Splitting on `-`, `_` **and** `.` is what makes `…-1243ba43bf8faa7b_bg.wasm`
work. Test the negatives explicitly: `index.html`, `/vendor/*`, `/assets/*`,
and a uuid path (uuid groups are 8/4/4/4/12 hex — never 16, so they can't be
mistaken for a fingerprint). An `immutable` stamped on the wrong file pins it
for a year, which is the one failure mode here that users can't clear.

Apply it as an `axum::middleware::from_fn` over the static service only (via
`ServiceBuilder`, then `Router::new().fallback_service(...)` and `merge` it into
the app) — not as a layer on the whole app, or API responses get it too.

**Also set `Vary: accept-encoding` yourself.** `ServeDir` sets
`Content-Encoding` but not `Vary`, and `immutable` + no `Vary` is exactly the
combination that lets a shared cache hand a brotli body to a client that never
asked for one.

**yomu now:** no cache headers at all. Note the service worker (§6) already
absorbs much of the repeat-load cost here, so this is a smaller win for yomu
than it was for chaos — the first load and the SW's own fetches still benefit.

---

## 4. Defer big vendor JS

chaos loaded a 1 MB ECharts bundle as a blocking `<script>` in `<head>` on
every page, for two tabs that actually charted. It became a memoized loader:

```js
window.chaosLoadECharts = function () {
  if (!window.chaosEChartsPromise) {
    window.chaosEChartsPromise = new Promise(function (resolve, reject) {
      var script = document.createElement("script");
      script.src = "/vendor/echarts.min.js";
      script.onload = function () { resolve(true); };
      script.onerror = function () { reject(new Error("failed")); };
      document.head.appendChild(script);
    });
  }
  return window.chaosEChartsPromise;
};
```

awaited from Rust:

```rust
#[wasm_bindgen(js_name = chaosLoadECharts, catch)]
async fn load_echarts() -> Result<JsValue, JsValue>;
```

Then the component gates its mount effect on a `ready` signal set by
`spawn_local(load_echarts())`, flipping an existing `failed` signal on error.

Two reasons to keep the loader in JS rather than injecting the `<script>` from
Rust: the crate also compiles natively for `clippy --all-targets`, and
`extern "C"` imports compile there as stubs while DOM plumbing does not; and
`catch` then also covers the function being *absent* (a shell serving a stale
`index.html`), which surfaces as a normal load failure instead of a panic.
Memoizing in JS means N components mounting together share one request.

**yomu now:** nothing to do — no vendor JS at all. But see §6 before adding
any, because yomu's service worker interacts with runtime-injected scripts.

---

## 5. Paint before the wasm boots

`<body>` is empty until instantiation, so even a 563 KB bundle shows a white
screen. Add a skeleton with an **inline** `<style>` in `<head>` (the hashed
stylesheet may not have arrived yet, so hardcode the two or three colors from
`styles.css`), then remove it after mount:

```rust
mount_to_body(move || view! { <App config=config.clone()/> });
if let Some(node) = web_sys::window()
    .and_then(|w| w.document())
    .and_then(|d| d.get_element_by_id("chaos-boot"))
{
    node.remove();
}
```

`mount_to_body` **appends** — it does not replace body content — so without the
removal the skeleton stays behind the app, `position: fixed`, covering it.
Needs `Document` and `Element` in the web-sys features of the entry crate.
Include a `prefers-reduced-motion` branch for the animation.

Trunk preserves custom `<body>` content and does not strip inline `<style>`;
verify on the *built* `dist/index.html`, not the source.

**yomu now:** `<body></body>`, so a blank first paint. The service worker
softens repeat visits but not the first one.

---

## 6. yomu-specific: the service worker changes two things

`crates/yomu-web/sw.js` precaches the shell plus the hashed assets it finds by
regexing the shell HTML for `href`/`src`/`from` ending in `.js`/`.css`/`.wasm`.

1. **Precompression is transparent to it.** The URLs don't change — only the
   representation the server picks — and the Cache API stores the decoded
   response. No `sw.js` change is needed for §1, and no `CACHE` bump.
2. **A runtime-injected script would never be precached.** If yomu ever adopts
   §4, the lazily-injected URL is not in the shell HTML, so `assetUrls()` won't
   find it and the feature breaks offline. Either add such URLs to the
   precache list explicitly, or accept that the deferred feature is
   online-only — and decide deliberately, because the failure only shows up
   offline.

Also: the `CACHE` constant must be bumped when caching logic or a fixed-name
asset changes. Changing compression or cache headers is neither, so leave it.

---

## 7. What to test, and how, without a browser

**Unit:** the `cache_control_for` classifier — positives, and the negatives
that matter (`index.html`, vendored paths, uuid-bearing API paths).

**Service-level**, with `tower::ServiceExt::oneshot` and a temp dist from
`std::env::temp_dir()` (no `tempfile` dependency needed):

- `Accept-Encoding: br` on a file *with* a `.br` sibling → `content-encoding:
  br` and the sibling's bytes
- no `Accept-Encoding` → identity bytes, no `content-encoding` header
- a file *without* a sibling → identity, 200, not 404 (the local-dist case)
- `Cache-Control` on a fingerprinted asset vs a SPA route

The `.br` fixture does **not** need to be valid brotli — `ServeDir` just serves
the file and labels it. Writing `"brotli-wasm-bytes"` as the sibling makes the
assertion readable.

**Browser behavior, headlessly:** `nix-shell -p geckodriver firefox`, drive the
WebDriver HTTP API with curl, and read the page's own Performance API:

```js
return performance.getEntriesByType('resource').map(e => e.name)
```

That is how we proved the ECharts load counts (zero on the dashboard, exactly
one after opening the tab, still one after navigating away and back) and that
the skeleton was *removed* rather than hidden
(`document.getElementById('chaos-boot') === null` **and**
`document.body.firstElementChild` is the app shell). Devtools network
throttling isn't reachable this way — assert on the served HTML and the
post-boot DOM instead.

Worth knowing: an empty database can make a UI assertion vacuous. Verifying
chaos's chart loading needed a stub upstream so a chart actually mounted;
without it the tab rendered "no data" and mounted nothing, and the test would
have passed while proving nothing.

---

## 8. The Tauri version trap (bit us)

`tauri.conf.json` pins `"version"`. It drifts — chaos's said `1.11.0` while the
workspace was at `1.12.0`, which would have stamped a 1.12.0 release APK as
1.11.0.

The obvious fix is wrong. Deleting the field is documented to fall back to the
Cargo package version, and it does — **for desktop bundling only**. Tauri's
Android generator (`tauri-cli 2.11.4`, `src/mobile/android/mod.rs`) writes
`gen/android/app/tauri.properties` *only* when the config carries a version and
has no Cargo fallback there, so with the field removed the APK silently builds
as `versionName 1.0 / versionCode 1` — which future upgrades then reject.

What works: keep the field out of the config and inject it in the build recipe,
so there is one source of truth that cannot drift.

```make
apk:
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    rm -f crates/<app>-desktop/gen/android/app/tauri.properties
    (cd crates/<app>-web && trunk build --release)
    nix develop .#android --command bash -c \
      "cd crates/<app>-desktop && cargo tauri android build --apk --target aarch64 --config '{\"version\":\"$version\"}'"
```

The `rm -f` matters: `tauri.properties` is gitignored and keeps whatever version
last resolved, so a stale one silently mislabels a build. Always verify the
artifact rather than the config — `aapt2 dump badging <apk>` prints
`package: name=… versionCode=… versionName=…`, and `apksigner verify
--print-certs` should show your release key, not `CN=Android Debug`.

**yomu now:** `tauri.conf.json` pins `"version": "2.0.0"` and the workspace is
also at `2.0.0`, so nothing is wrong *today* — but the drift is unguarded, and
yomu's `justfile` has no `apk` recipe to inject from, so whatever builds its
APK needs the same treatment before the next release.

---

## Expected outcome for yomu

Applying §1–§3 and §5, extrapolating from chaos's measured ratios:

| | now | after |
| --- | --- | --- |
| wasm | 3 604 377 | ~450 000 br |
| bindgen glue | 65 023 | ~9 000 br |
| css | 28 622 | ~6 000 br |
| **cold load** | **~3.70 MB** | **~0.47 MB** |
| first paint | after wasm boots | immediate skeleton |

No vendor JS to defer, and the service worker already covers repeat loads — so
for yomu the win is concentrated entirely in the first visit, which is also the
only one that currently hurts.

## Order of work

1. §1 compression — biggest win, no code risk, and independent of everything else
2. §2 profile + wasm-opt — biggest win *inside* the bundle; slow builds, so do it once and after any other change that alters wasm contents
3. §3 cache headers — small for yomu given the SW, cheap anyway
4. §5 skeleton — cosmetic but the most visible to a user
5. §8 before the next release, whether or not the rest happens
