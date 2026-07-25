# Delivery size — design

**Goal:** cut what yomu ships — the first-load web payload, the Nix closures
its packages drag along, and the artifacts a release publishes — without
changing what the app does.

Two sources feed this: `docs/frontend-delivery-size.md` (the playbook written
from chaos, §1–§8), and closure measurements taken on 2026-07-25 that turned
up a problem the playbook doesn't cover.

---

## Baseline, measured at v2.0.0

Web bundle (`crates/yomu-web/dist`, release build):

| asset | raw | brotli -q 11 |
| --- | --- | --- |
| `yomu-web-<hash>_bg.wasm` | 3 698 836 | 684 728 (5.4×) |
| `yomu-web-<hash>.js` | 65 151 | 8 990 |
| `styles-<hash>.css` | 30 197 | 6 715 |
| `index.html` | 1 947 | 807 |
| **cold load** | **~3.80 MB** | **~0.70 MB** |

The wasm carries a 565 089 B `name` section (15%) and a `producers` section:
nothing strips. Nothing is compressed on the wire and nothing carries
`Cache-Control`.

Nix closures:

| package | closure |
| --- | --- |
| `yomu-server` | 2.5 GiB |
| `yomu-desktop` | 3.4 GiB |

Of which the Rust toolchain is ~2.4 GiB in both: rust-docs 636.5 MiB,
rust-analyzer 483.3 MiB, clippy 461.3 MiB, rustfmt 448.6 MiB, rustc
439.9 MiB, gcc-wrapper 369.5 MiB.

Artifacts published per release:

| artifact | size |
| --- | --- |
| `yomu-desktop-2.0.0-x86_64.AppImage` | 1 068 182 048 |
| `yomu-2.0.0-aarch64.apk` | 12 875 747 |
| `yomu-web-2.0.0.tar.gz` | 1 156 760 |

The AppImage has been ~1.068 GB in every release since at least 1.12.0, so it
is structural, not a regression.

---

## Findings

### F1. The Rust toolchain is a runtime dependency of both packages

`nix why-depends` puts `rust-default-1.96.1` one hop from each output, and the
reference lives in `.rodata`: panic-location strings that name the toolchain's
bundled std sources.

```
/nix/store/01qwry…-rust-default-1.96.1/lib/rustlib/src/rust/library/alloc/src/borrow.rs
```

Nix scans outputs for store paths, finds those, and retains the whole
toolchain — which then pulls rustc, docs, rust-analyzer, clippy, rustfmt and
gcc into the *runtime* closure. This is the dominant term in both closures,
and on zeus it is what every `nixos-rebuild` carries for a 13 MB server
binary.

### F2. `yomu-desktop` installs a 170 MB static library

`crates/yomu-shell` declares `crate-type = ["staticlib", "cdylib", "rlib"]`
because the Android build needs the first two. `buildRustPackage` installs
everything it finds, so the desktop output contains
`lib/libyomu_shell_lib.a` at 170 870 786 B — five sixths of the output — plus
a 1.4 MB `.so`. Neither is used by `bin/yomu-shell`; both are packed into the
AppImage.

### F3. The Android `.so` ships its symbol table

`libyomu_shell_lib.so` is 10 195 528 B in the APK, but its allocated sections
total 7 864 008 B. The remainder is `.symtab`/`.strtab` — no strip step
anywhere in the Android path.

### F4. `[profile.wasm-release]` has never been used

`Cargo.toml:71` defines it (`opt-level = "z"`, `panic = "abort"`, inheriting
`lto`/`codegen-units` from release) and nothing passes it to trunk, because
`crates/yomu-web/index.html` has no `rel="rust"` link. Release wasm builds run
at `opt-level = 3`, unstripped. This is playbook §2.

---

## What we will do

Ordered by payoff. Each item states how it is verified; sizes are recorded
before and after in the commit that makes the change.

### 1. Drop the toolchain from the runtime closures (F1)

Add `--remap-path-prefix` for the toolchain path to the Rust builds, so panic
locations no longer name a store path:

```nix
env.RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rust-toolchain";
```

Applied to `yomu-server` and `yomu-desktop`. `yomu-web` builds wasm through
trunk and has no runtime closure to protect, but takes the same flag so the
three builds stay consistent (and so the wasm stops embedding store paths in
panic strings, which is a small size win of its own).

**Verify:** `nix path-info -rSh .#yomu-server | grep -c rust-` is 0, and the
closure is reported in the commit message.

**Measured, 2026-07-25** (prototype on `yomu-server`, since discarded): closure
**2.5 GiB → 58.8 MiB**, references down to `glibc` and `gcc-lib` alone. The
binary itself grew 138 KB (13 255 344 → 13 393 392). `cargoTestFlags` ran in
the same build and passed, so remapped panic paths break nothing.

**Risk:** a panic backtrace shows `/rust-toolchain/...` instead of a real
path. That path was never resolvable on a user's machine anyway.

**Watch for:** setting `env.RUSTFLAGS` *replaces* rather than appends. Confirm
`buildRustPackage` isn't relying on a RUSTFLAGS value of its own for these
packages before taking this as done — the 138 KB delta is unexplained and may
be exactly that.

### 2. Stop installing the static library (F2)

`postInstall` removes `lib/libyomu_shell_lib.a`. The `.so` stays: it costs
1.4 MB and dropping build artifacts we might later want is a separate
decision.

**Verify:** the desktop output is ~13 MB rather than 58 MB, and
`bin/yomu-shell` still starts (`--help`, or a launch that reaches the connect
form).

### 3. Stop publishing the AppImage

Remove the bundle step and its asset upload from `.github/workflows/release.yml`,
and point desktop users at the flake in `README.md` and `docs/ARCHITECTURE.md`:

```
nix run github:tdbmxyz/yomu#yomu-desktop
```

Decided with the numbers in hand: even after items 1 and 2 the bundle would be
several hundred MB, because it packs webkitgtk (825 MiB) and gst-plugins-base
(310 MiB). The server is already consumed through the flake, so this is the
same distribution story rather than a new one.

**Verify:** a release dry run publishes exactly the web bundle, its checksum,
and (attached by hand) the APK.

### 4. Precompress the web bundle and serve it (playbook §1)

A second package wraps the trunk dist with brotli `-q 11` and gzip `-9`
siblings; `ServeDir`/`ServeFile` get `.precompressed_br().precompressed_gzip()`.

The plain dist stays a separate package, and it is the one `yomu-desktop`
embeds — compressed siblings in the Tauri dist are ~700 KB of files the asset
protocol never serves, and they would inflate the desktop binary and the APK.
The NixOS module's `webPackage` defaults to the precompressed one.

**Verify:** service-level tests (`tower::ServiceExt::oneshot`, temp dist):
`Accept-Encoding: br` yields `content-encoding: br` and the sibling's bytes;
no `Accept-Encoding` yields identity with no header; **a file with no sibling
still serves identity 200**, which is what keeps a plain local `trunk build`
working. `nix derivation show .#yomu-desktop` must show only the plain dist as
an input.

### 5. Wire the wasm size profile (playbook §2, F4)

One line in `crates/yomu-web/index.html`:

```html
<link data-trunk rel="rust" data-cargo-profile-release="wasm-release"
      data-wasm-opt="z" data-wasm-opt-params="--strip-debug --strip-producers" />
```

It must go through trunk, never a post-build hook: trunk computes the SRI hash
after wasm-opt and embeds it in `index.html`, so rewriting the wasm afterwards
yields a page that refuses to boot.

`opt-level` is decided by measurement, not assumption: build `"z"` and `"s"`,
record raw and brotli for each, and pick from the numbers. `"z"` is what the
profile already says and what won on chaos; `"s"` is the fallback if the
reader feels worse.

**Measured, 2026-07-25:** baseline 3 698 968 / 685 893 brotli; `"z"` +
wasm-opt **1 452 288 / 450 159**; `"s"` + wasm-opt 1 818 965 / 507 605. `"z"`
kept — `"s"` costs +25% raw, +12.7% brotli. wasm-opt alone would have given
2 774 889 / 613 145, so the profile contributes more than wasm-opt, as the
playbook predicted.

`panic = "abort"` is set explicitly in the profile, but it is a **no-op on
`wasm32-unknown-unknown`** — that target's spec already hardcodes
`"panic-strategy": "abort"`. No unwinding behaviour changes, and a sweep of
the wasm-compiled crates found nothing depending on unwinding (no
`catch_unwind`, no `resume_unwind`, no `#[should_panic]`; the only panic
plumbing is `console_error_panic_hook::set_once()`, and hooks still run under
abort).

**Verify:** raw and brotli sizes for both levels, in the commit message; the
app boots and a chapter scrolls in the reader.

### 6. `Cache-Control` and `Vary` on static assets (playbook §3)

A pure `cache_control_for(path) -> &'static str` classifier: a filename
segment of exactly 16 hex characters (splitting on `-`, `_` and `.`) means
fingerprinted, so `immutable` for a year; everything else revalidates. Applied
as `axum::middleware::from_fn` over the static service only, never the whole
app, and paired with `Vary: accept-encoding` — `ServeDir` sets
`Content-Encoding` but not `Vary`, and `immutable` without `Vary` lets a
shared cache hand a brotli body to a client that never asked for one.

**Verify:** unit tests for the positives and, more importantly, the negatives
— `index.html`, `/vendor/*`, `/assets/*`, and a uuid-bearing API path (uuid
groups are 8/4/4/4/12 hex, never 16). An `immutable` on the wrong file is the
one failure here users cannot clear.

Smaller win for yomu than for chaos: the service worker already absorbs most
repeat-load cost. First load and the worker's own fetches still benefit.

### 7. Paint before the wasm boots (playbook §5)

A skeleton in `<body>` with an **inline** `<style>` in `<head>` (the hashed
stylesheet may not have arrived yet, so two or three colors are hardcoded from
`styles.css`), removed after `mount_to_body` — which *appends*, so without the
removal the skeleton stays behind the app covering it. Includes a
`prefers-reduced-motion` branch.

**Verify:** assert on the built `dist/index.html`, not the source, that the
skeleton and inline style survived trunk; after boot,
`document.getElementById(...)` is null.

### 8. Strip the Android library (F3)

`-C strip=symbols` for the Android build only, through the new `just apk`
recipe below rather than `[profile.release]`, so server backtraces keep their
symbols.

**Verify:** `.so` size before/after, and the APK size.

### 9. `just apk` with an injected version (playbook §8)

A recipe that reads the version from `Cargo.toml`, removes the gitignored
`gen/android/app/tauri.properties` (it keeps whatever version last resolved
and would silently mislabel a build), and passes `--config '{"version":"…"}'`.
The `version` field comes out of `tauri.conf.json` so there is one source of
truth.

`.github/workflows/release.yml` currently checks the tag against *both*
`Cargo.toml` and `tauri.conf.json`; with the field gone, that second check is
dropped — the recipe is what guarantees it now.

**Verify:** `aapt2 dump badging <apk>` prints the workspace version as
`versionName`, and `versionCode` is not 1.

---

## Not doing

- **§4 defer vendor JS** — yomu has no vendor JS.
- **`tower_http::CompressionLayer`** — re-compresses megabytes per cold
  request; build-time siblings cost nothing per request.
- **Bumping the service worker's `CACHE`** — compression and cache headers are
  neither a caching-logic change nor a fixed-name asset change. URLs don't
  change and the Cache API stores decoded responses, so `sw.js` needs no edit.
- **Trimming the webkit closure** — moot once the AppImage is gone.

---

## Outcome — measured 2026-07-25, all nine items landed

The "predicted" column is what this document said before the work; it is kept
so the estimate can be judged rather than quietly replaced. "Measured" is from
`nix build` outputs and a release `trunk build` on the same machine as the
baseline.

| | before (measured) | predicted | **after (measured)** | vs. prediction |
| --- | --- | --- | --- | --- |
| web cold load, **on the wire** | 3 796 131 (nothing compressed) | ~0.45 MB | **467 561 B** | as predicted |
| web cold load, raw bytes | 3 796 131 | — | **1 546 024 B** | — |
| web cold load, brotli ceiling | 701 240 | — | **467 561 B** | now actually reached |
| wasm raw / brotli | 3 698 836 / 684 728 | — | **1 446 401 / 450 227** | −61% / −34% |
| bindgen glue raw / brotli | 65 151 / 8 990 | — | **65 150 / 9 004** | unchanged, as expected |
| css raw / brotli | 30 197 / 6 715 | — | **30 197 / 6 715** | unchanged |
| `index.html` raw / brotli | 1 947 / 807 | — | **4 276 / 1 615** | +2.3 KB: the boot skeleton |
| first paint | after wasm boots | immediate skeleton | **immediate skeleton** | as predicted |
| `yomu-server` closure | 2.5 GiB | 58.8 MiB | **58.7 MiB** | as predicted |
| `yomu-desktop` closure | 3.4 GiB | (not predicted) | **854.5 MiB** | webkitgtk, as expected |
| `yomu-desktop` output | 58 MB | ~13 MB | **12 521 976 B** | as predicted |
| APK | 12 875 747 | ~10.5 MB | **10 253 043 B** | slightly better |
| published per release | 1 082 MB | ~14 MB | **~12.0 MB** | slightly better |

Toolchain references in either runtime closure: **0**
(`nix path-info -rSh .#yomu-server | grep -c 'rust-default\|rustc-\|rust-docs\|clippy\|rustfmt\|rust-analyzer'`).

"Published per release" is now the web tarball (1 777 212 B, `nix build
.#yomu-web-compressed` tarred) plus the APK; the AppImage is gone. The
tarball is larger than 1.x's 1 156 760 B on purpose — it now carries the
`.br`/`.gz` siblings the server hands out, so the *download* is bigger and
every visitor's is smaller.

Two numbers worth naming explicitly, because they are the ones a reader will
check: the wasm on the wire went **684 728 → 450 227 B**, and the cold load
**~3.80 MB raw and uncompressed → 467 561 B compressed**, a factor of 8.1.

### Verification the plan left open, closed here

**The reader under `opt-level = "z"`.** Item 5 picked `"z"` on size alone.
Driven through geckodriver against a fixture publication (24 pages, vertical
strip, `scrollHeight` 27 747 px, headless Firefox 152 at 900×1400): 399
`window.scrollBy` steps, one per `requestAnimationFrame`, all 399 handled by
the strip's `scroll` listener. Frame deltas: **mean 16.62 ms, p50 17.06,
p95 17.08, p99 17.20, max 17.38; zero frames over 33 ms.** Every frame landed
on the 60 Hz vsync boundary, i.e. the strip's per-frame Rust never became the
limiting term. `"z"` stands; nothing here argues for reconsidering `"s"`.

**The service worker against `Content-Encoding`.** Smoked against the
precompressed dist (`yomu-web-compressed`), so every shell asset arrived
`content-encoding: br`. After install, the `yomu-v5` cache holds four entries
and each one stores the **decoded** body under **stale compressed headers** —
e.g. the wasm at `stored_bytes` 1 446 401 with `content-encoding: br` and
`content-length: 450227`. The stored bytes start `00 61 73 6d` and
`WebAssembly.compile` accepts them, so the body really is decoded and the
headers really are a lie. Then, with the server killed and `curl` failing:
`/library` reloaded, the shell booted from cache (boot skeleton removed, nav
present, offline banner shown), wasm `transferSize` 0. So the "not doing"
entry above holds — no `sw.js` change, no `CACHE` bump — but for a narrower
reason than it claimed: the *body* is decoded, not the response. Firefox
ignores the header when the worker replays the response; this was not tested
on Chromium.
