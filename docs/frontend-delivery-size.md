# Shrinking a Leptos CSR delivery — playbook

Notes from cutting chaos's dashboard cold load from 5 899 053 B to 580 959 B
(−90%) on 2026-07-25. Same stack as yomu (Leptos 0.8 CSR + trunk + axum
`ServeDir` + Tauri + a nix flake), so nearly everything transfers. Each item
records what to check, the fix, the trap, and **yomu's status as of
2026-07-25** — measured, not assumed.

Then applied to yomu the same day, cold load 3 796 131 → 467 561 B on the
wire (−87.7%). Everything below marked "yomu now: done" has been through a
second implementation, and the traps in §2, §3 and §8 are all things that
went wrong on one repo or the other rather than things we anticipated.

Ordered by payoff per unit of effort. The first three are the whole story;
the rest is polish.

Companion: `app-auth-playbook.md` — signing the Tauri/Android app into an
SSO'd server, from the same pair of repos.

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

**yomu now:** done, and the numbers are at the bottom of this file. Where it
started: wasm 3 604 377 B, brotli ceiling 670 382 B (5.4×), a 548 315 B `name`
section, `producers` present, nothing compressed on the wire, no
`Cache-Control` anywhere. Where it is: wasm 1 446 401 B raw / 450 227 B
brotli, no `name` and no `producers` section, brotli on the wire, and
`Cache-Control` on every static response.

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

**yomu now:** both exist. `flake.nix` has `yomu-web-compressed` (a
`runCommand` over `yomu-web`), `nix/module.nix` defaults `webPackage` to it,
and the static service in `crates/yomu-server/src/api/mod.rs` is
`.precompressed_br().precompressed_gzip()` on both the `ServeDir` and the
`ServeFile` fallback. `nix derivation show .#yomu-desktop | grep -c
yomu-web-compressed` is 0, so the shell still embeds the plain dist. Measured
on the wire: `content-encoding: br`, `content-length: 450227` for a 1 446 401 B
wasm. The release tarball is the compressed package, so a self-hoster who
untars it gets the siblings too — the download grew (1 156 760 → 1 777 212 B)
so that every visitor's shrank.

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

**yomu now:** done. `crates/yomu-web/index.html` carries the `rel="rust"` link
above verbatim, `[profile.wasm-release]` keeps `opt-level = "z"`, and
`wasm-objdump -h` finds no `name` and no `producers` section. The
extrapolation from chaos's ratios ("near 1.4 MB raw / ~450 KB brotli") landed
almost exactly: 1 446 401 / 450 227.

**And the reader was then actually exercised under `"z"`, because size alone
does not answer the question the setting raises.** Driven through geckodriver
(§7) against a 24-page fixture in the continuous strip, 399 `window.scrollBy`
steps at one per `requestAnimationFrame`, all 399 dispatched to the strip's
Rust `scroll` listener: mean frame delta 16.62 ms, p95 17.08, max 17.38, zero
frames over 33 ms — every frame on the 60 Hz boundary, so the per-frame Rust
never became the limiting term. Do this rather than shipping `"z"` on a size
table and an impression; it is ten lines on top of a harness §7 already
needs.

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
/// A `u64` in hex with leading zeros dropped: 16 nibbles *at most*, fewer
/// whenever the top ones are zero. See the trap below.
const FINGERPRINT_LEN: std::ops::RangeInclusive<usize> = 8..=16;

pub(crate) fn fingerprinted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split(['-', '_', '.']).any(|seg| {
        FINGERPRINT_LEN.contains(&seg.len()) && seg.chars().all(|c| c.is_ascii_hexdigit())
    })
}
```

Splitting on `-`, `_` **and** `.` is what makes `…-1243ba43bf8faa7b_bg.wasm`
work. An `immutable` stamped on the wrong file pins it for a year, which is
the one failure mode here that users can't clear.

Apply it as an `axum::middleware::from_fn` over the static service only (via
`ServiceBuilder`, then `Router::new().fallback_service(...)` and `merge` it into
the app) — not as a layer on the whole app, or API responses get it too.

**Also set `Vary: accept-encoding` yourself.** `ServeDir` sets
`Content-Encoding` but not `Vary`, and `immutable` + no `Vary` is exactly the
combination that lets a shared cache hand a brotli body to a client that never
asked for one. **`append`, not `insert`** — a CORS layer sets its own `Vary`
(origin plus the two `access-control-request-*` headers), and replacing it
makes a shared cache answer one origin's preflight from another's. Which
layer runs first must not decide whether that bug exists.

### Trap 1: trunk's fingerprint length varies, so an exact-length check silently disables the feature

Trunk renders the hash as a `u64` in hex with leading zeros dropped. A real
build of yomu produced `yomu-web-ae4beb7cab1d74_bg.wasm` — **14** characters.
`seg.len() == 16` rejected it, so the 1.45 MB wasm shipped with
`must-revalidate` and every unit test still passed, because every fixture in
the file happened to be a full-length hash. One build in 16 loses a nibble,
one in 4096 loses three or more. Accept a range, and put a short hash in the
tests.

The failure mode is the nastiest kind: no error, no warning, the feature is
simply off for that deploy and back on for the next one.

### Trap 2: the SPA fallback means a URL classifier is answering the wrong question

These two cost two review rounds, and they are general to the pattern — any
`ServeDir(...).fallback(index.html)` behind a URL-shaped cache classifier has
both.

**(i) The classifier stamps the shell.** `ServeDir` answers a miss with
`index.html`. So a request for a hashed asset that no longer exists returns
HTML — and the classifier, which sees only the URL, says "16 hex characters,
`immutable`". The browser now holds HTML under an asset URL for a year. Deploy
v2; a stale tab or the service worker asks for a v1 asset; if v1 is ever served
again (a rollback, or a reverted frontend change reproducing the same trunk
hash) the app fetches wasm, receives HTML, and never boots. Nothing short of a
manual cache clear escapes.

**(ii) The obvious fix — check the response — does not work, and its 304
re-stamps what you just protected.** Keying on `content-type: text/html` looks
like it closes (i). It cannot: **a 304 carries no `content-type` at all**, and
the `must-revalidate` you set is precisely what makes the browser send the
conditional request. So the shell-under-an-asset-URL response comes back as a
304 on its *second* use, the content-type check sees nothing to object to and
writes `immutable` — and RFC 9111 §4.3.4 has the client merge a 304's headers
into its stored response. One round trip later the stored HTML is immutable
again.

**The structural fix: let the service that answered set its own header, and
have the outer layer fill in only what is absent.**

```rust
/// Wraps the SPA fallback service, so *everything it answers* is
/// revalidate-always — 200, 304, 206, any content type, compressed or not.
async fn shell_cache_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE));
    response
}

async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    let value = if fingerprinted(&path) && may_be_pinned(&response) {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    // Only if absent. A value already on the response was put there by the
    // inner service, which knows what it served; this middleware knows only
    // the URL, and the URL is the thing that lies.
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(value));
    response
}
```

Now the question "is this the shell?" is answered by *which service ran*, which
no response header can hide and no 304 can erase. Two consequences worth
naming:

- It makes the URL guess affordable. Once the shell defends itself, a route
  misread as an asset — an id-bearing SPA path, a uuid, a hex source id — is
  answered by the shell and keeps the shell's `must-revalidate`. That is what
  lets trap 1's fix (a *wider* length range) be safe. The two changes are one
  change.
- Still gate on status: a 404 or 405 describes today's dist, not tomorrow's,
  and pinning it makes the miss permanent. `304` must stay pinnable, though —
  it is the successful answer for a body the client already holds, and a real
  hashed asset revalidating has to stay `immutable` or every reload pays a
  round trip.

**yomu now:** done, in `crates/yomu-server/src/api/static_cache.rs`, with both
traps above encoded as tests. Measured on the wire: `cache-control: public,
max-age=31536000, immutable` on the hashed wasm, `vary: accept-encoding`
alongside the CORS `vary`. Note the service worker (§6) already absorbs much of
the repeat-load cost here, so this is a smaller win for yomu than it was for
chaos — the first load and the SW's own fetches still benefit.

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

**yomu now:** done. `crates/yomu-web/index.html` carries a `#yomu-boot`
spinner and its inline `<style>` (background `#14171c`, accent `#4fd1c5`,
hardcoded from `styles.css`, with a `prefers-reduced-motion` branch);
`crates/yomu-web/src/main.rs` removes the node right after `mount_to_body`.
Cost 2 329 B raw / 808 B brotli on `index.html` — the whole of the file's
growth from 1 947 to 4 276 B. Confirmed in a headless browser after a real
boot: `document.getElementById('yomu-boot') === null`, and the app's `<nav>`
is `document.body.firstElementChild`.

---

## 6. yomu-specific: the service worker changes two things

`crates/yomu-web/sw.js` precaches the shell plus the hashed assets it finds by
regexing the shell HTML for `href`/`src`/`from` ending in `.js`/`.css`/`.wasm`.

1. **Precompression is transparent to it — but for a narrower reason than
   "the Cache API stores decoded responses".** The URLs don't change, only the
   representation the server picks, so no `sw.js` change is needed for §1 and
   no `CACHE` bump. What is actually stored, though, is a **decoded body under
   the compressed response's headers**. Measured on yomu against the
   precompressed dist (Firefox 152, `cache.put(url, await fetch(url))`):

   | cached entry | `stored_bytes` | `content-encoding` | `content-length` |
   | --- | --- | --- | --- |
   | `…_bg.wasm` | 1 446 401 | `br` | `450227` |
   | `…​.js` | 65 150 | `br` | `9004` |
   | `styles-…​.css` | 30 197 | `br` | `6715` |
   | `/` (shell) | 4 276 | `br` | `1615` |

   The body is genuinely decoded — the cached wasm starts `00 61 73 6d` and
   `WebAssembly.compile` accepts it — while both headers still describe the
   compressed representation the server sent. So the stored `Response` is
   internally inconsistent, and a worker that replays it with
   `respondWith(cached)` is handing the browser a body that contradicts its
   own `Content-Encoding`. Gecko ignores the header on that path and the app
   boots; **this was not verified on Chromium.** If you ever read
   `content-length` off a cached response, or serve one to something stricter,
   that is where it bites.

   Verified end to end rather than reasoned about: install the worker against
   the precompressed dist, kill the server (`curl` must fail), reload a deep
   route. yomu's shell booted from cache with the boot skeleton removed, the
   nav rendered, the offline banner up and the wasm at `transferSize: 0`.
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

The same harness answers "did `opt-level = "z"` cost us anything?", which is
otherwise pure hand-waving. Drive one `window.scrollBy` per
`requestAnimationFrame` for a few hundred frames, record the `performance.now()`
deltas, and report mean/p95/max plus a count of frames over 33 ms. Two things
make the number honest: assert that the scroll *listener* actually fired
(count `scroll` events — 399 of 399 on yomu), and remember headless rAF is
capped at the display rate, so "mean 16.6 ms, max 17.4 ms" means *never worse
than vsync*, not "16.6 ms of work". Firefox has no `PerformanceObserver`
`longtask`, so frame deltas are the available instrument.

Worth knowing: an empty database can make a UI assertion vacuous. Verifying
chaos's chart loading needed a stub upstream so a chart actually mounted;
without it the tab rendered "no data" and mounted nothing, and the test would
have passed while proving nothing. yomu's version of that: point the server's
watched-folder scanner at a generated fixture (a directory of 24 PNGs) so the
library is non-empty, and check `document.images.length` and `scrollHeight`
before trusting a scroll measurement — the first attempt scrolled a page whose
`scrollHeight` equalled `innerHeight` and dutifully reported perfect frame
times for doing nothing.

And check which *mode* you are measuring. yomu's reader defaults to the swipe
pager, where the window does not scroll at all; the continuous strip is opt-in
per publication and stored in `localStorage`. Set it (navigate to the origin,
`localStorage.setItem('yomu-reader-mode:<id>', 'vertical')`, then navigate to
the reader) or you measure an empty document.

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
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || { echo "no version" >&2; exit 1; }
    rm -f crates/<app>-desktop/gen/android/app/tauri.properties
    (cd crates/<app>-web && trunk build --release)
    nix develop .#android --command bash -c \
      "cd crates/<app>-desktop && cargo tauri android build --apk --target aarch64 --config '{\"version\":\"$version\"}'"
```

The assertion matters as much as the injection: a `sed` that matches nothing
yields an empty string, the command substitution still *succeeds*, `set -e` does
not fire, and the build proceeds with `--config '{"version":""}'` — which is
exactly the failure the recipe exists to prevent.

The `rm -f` matters: `tauri.properties` is gitignored and keeps whatever version
last resolved, so a stale one silently mislabels a build. Always verify the
artifact rather than the config — `aapt2 dump badging <apk>` prints
`package: name=… versionCode=… versionName=…`, and `apksigner verify
--print-certs` should show your release key, not `CN=Android Debug`.

### The trap next door: don't strip the Android library with a bare `RUSTFLAGS`

Anyone applying §2 and §8 together hits this, and it cost us a build. The APK's
`libapp_lib.so` ships its symbol table — on yomu, 10 195 528 B on disk against
7 864 008 B of allocated sections — and Android **stores native libraries
uncompressed** in the zip (`unzip -v` says `Stored`, 0%), so every one of those
bytes is a byte of download. `-C strip=symbols` on the Android build alone is a
~2.3 MB win; do not put it in `[profile.release]`, which also builds the server
and would cost readable backtraces.

But do not export it as a plain `RUSTFLAGS` in the recipe either. Tauri runs
`beforeBuildCommand` (`trunk build --release`) *inside* the same invocation, so
the variable also reaches the wasm build; stripping removes the wasm's
`target_features` section and wasm-opt — which §2 just turned on — aborts with:

```
[wasm-validator error] memory.copy operations require bulk memory operations
```

Scope it to the Android target instead:

```bash
CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C strip=symbols" \
  cargo tauri android build --apk --target aarch64 …
```

**And then verify the artifact, because that scoping is fragile.** That variable
is the env form of `target.<triple>.rustflags`, and cargo takes rustflags from
the *first applicable source* rather than merging them — so a `RUSTFLAGS` in the
caller's environment outranks it entirely, silently, with no error and a
2.3 MB-larger APK. (Same mechanism as the nixpkgs `cargoSetupHook` hazard: a
bare `RUSTFLAGS` there silently drops `-Cforce-frame-pointers=yes`.) Assert on
the shipped library rather than on the environment:

```bash
unzip -p "$apk" lib/arm64-v8a/libapp_lib.so > "$so"
readelf -S "$so" | grep -qE '\.symtab($|[^A-Za-z0-9_])' && exit 1
```

**yomu now:** done. `tauri.conf.json` carries no `"version"`; `just apk` reads
it from `Cargo.toml`, aborts unless it parses as semver, removes the stale
`tauri.properties`, builds inside `nix develop .#android` with the per-target
strip flag, and then fails the build if `.symtab` survived into the APK. The
measured APK saving was 2 622 704 B, of which the strip is ~89%.

---

## Outcome for yomu — measured 2026-07-25, after §1–§3, §5 and §8 all landed

The predicted column is what this file said before the work, extrapolated from
chaos's ratios. It is kept so the extrapolation can be judged.

| | before (measured) | predicted | **after (measured)** |
| --- | --- | --- | --- |
| wasm raw / brotli | 3 698 836 / 684 728 | ~450 000 br | **1 446 401 / 450 227** |
| bindgen glue raw / brotli | 65 151 / 8 990 | ~9 000 br | **65 150 / 9 004** |
| css raw / brotli | 30 197 / 6 715 | ~6 000 br | **30 197 / 6 715** |
| `index.html` raw / brotli | 1 947 / 807 | — | **4 276 / 1 615** |
| **cold load** | **3 796 131 raw** | ~0.47 MB | **467 561 B on the wire** |
| first paint | after wasm boots | immediate skeleton | **immediate skeleton** |

A factor of 8.1 on the first visit, and the extrapolation was accurate to
within a percent on every line — cross-project ratios from one Leptos CSR
bundle to another turned out to be a usable estimator.

No vendor JS to defer, and the service worker already covers repeat loads — so
for yomu the win is concentrated entirely in the first visit, which was also the
only one that hurt.

Off the frontend, from the same pass (yomu-specific, but the mechanisms are
not — see §8 and the two nix items below):

| | before | after |
| --- | --- | --- |
| `yomu-server` nix closure | 2.5 GiB | **58.7 MiB** |
| `yomu-desktop` nix closure | 3.4 GiB | **854.5 MiB** |
| `yomu-desktop` output | 58 MB | **12 521 976 B** |
| APK | 12 875 747 | **10 253 043 B** |
| published per release | ~1 082 MB | **~12.0 MB** |

The two nix items, both worth checking on any rust-in-nix project:

- **Panic-location strings pin the whole toolchain into the *runtime*
  closure.** `rustc` embeds `${toolchain}/lib/rustlib/src/rust/library/...`
  in `.rodata`; nix scans outputs for store paths, finds them, and retains
  rustc, rust-docs, rust-analyzer, clippy, rustfmt and gcc behind a 13 MB
  binary. `env.RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rust-toolchain"`
  removes 2.4 GiB. Check with
  `nix path-info -rSh .#pkg | grep -c 'rust-default\|rustc-\|clippy'` — 0 is
  the answer. Append to any existing `RUSTFLAGS` rather than assigning over
  it.
- **`crate-type = ["staticlib", ...]` for a mobile build gets installed on
  desktop too.** `buildRustPackage` installs whatever it finds;
  yomu's `libyomu_shell_lib.a` was 170 870 786 B of a 58 MB output. `rm` it in
  `postInstall`.

## Order of work

1. §1 compression — biggest win, no code risk, and independent of everything else
2. §2 profile + wasm-opt — biggest win *inside* the bundle; slow builds, so do it once and after any other change that alters wasm contents
3. §3 cache headers — small for yomu given the SW, cheap anyway. Budget more
   than you expect: the classifier is five lines and both of its traps are
   invisible until a specific deploy sequence hits them.
4. §5 skeleton — cosmetic but the most visible to a user
5. §8 before the next release, whether or not the rest happens

And once, at the end: re-measure everything against the numbers you wrote down
in §0, keeping the predictions beside the measurements. Two of yomu's
predictions were right to within a percent and one ("the strip inside the APK
is deflated, so the strip saving mostly vanishes") was wrong by the whole
amount. You only learn which was which by leaving both columns in the file.
