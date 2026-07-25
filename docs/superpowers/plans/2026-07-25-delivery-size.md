# Delivery Size Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cut what yomu ships — first-load web payload, Nix closures, and published release artifacts — without changing what the app does.

**Architecture:** Four independent fronts. Nix packaging (closure leak, stale static lib, a precompressed dist package), the axum static service (precompressed siblings, cache headers), the trunk build (size profile, boot skeleton), and the Android/release path (strip, version injection, no more AppImage). Nothing here changes app behaviour or the HTTP wire; every task is verified by a measurement or a test, and every size claim is re-measured rather than predicted.

**Tech Stack:** Nix flake (rust-overlay, `buildRustPackage`, `runCommand`), axum + tower-http `fs`, trunk + binaryen, Tauri v2 (desktop + Android), just, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-07-25-delivery-size-design.md`
**Playbook:** `docs/frontend-delivery-size.md`

**Baseline to beat** (measured 2026-07-25 at v2.0.0):

| thing | bytes |
| --- | --- |
| wasm raw / brotli | 3 698 836 / 684 728 |
| bindgen glue raw / brotli | 65 151 / 8 990 |
| css raw / brotli | 30 197 / 6 715 |
| web cold load | ~3.80 MB |
| `yomu-server` closure | 2.5 GiB |
| `yomu-desktop` closure / output | 3.4 GiB / 58 MB |
| APK | 12 875 747 |
| `libyomu_shell_lib.so` (android) | 10 195 528 |

---

## File Structure

| file | responsibility | tasks |
| --- | --- | --- |
| `flake.nix` | package definitions: remap flag, drop `.a`, new compressed dist package | 1, 2, 4 |
| `nix/module.nix` | `webPackage` default points at the compressed dist | 4 |
| `crates/yomu-server/src/api/mod.rs` | static service: precompressed siblings, cache-header layer | 4, 6 |
| `crates/yomu-server/src/api/static_cache.rs` *(new)* | `cache_control_for` classifier + its layer, isolated so it is unit-testable without a router | 6 |
| `crates/yomu-web/index.html` | trunk directives: rust profile + wasm-opt, boot skeleton, inline style | 5, 7 |
| `crates/yomu-web/src/main.rs` | remove the skeleton after mount | 7 |
| `Cargo.toml` | `[profile.wasm-release]` opt-level, if measurement says so | 5 |
| `crates/yomu-shell/tauri.conf.json` | drop the `version` field | 9 |
| `justfile` | `apk` recipe: version injection + strip | 8, 9 |
| `.github/workflows/release.yml` | no AppImage; tag check reads only `Cargo.toml` | 3, 9 |
| `README.md`, `docs/ARCHITECTURE.md`, `docs/HANDOFF.md` | desktop install story, build commands | 3, 9 |

Order matters in two places only: task 4 must land before task 6 (both edit the static service; 6 layers onto what 4 builds), and task 5 should be re-measured after any later change that alters wasm contents. Everything else is independent.

---

### Task 1: Drop the Rust toolchain from the runtime closures

**Files:**
- Modify: `flake.nix` (`yomu-server` ~line 87, `yomu-web` ~line 103, `yomu-desktop` ~line 141)

**Why:** panic-location strings in `.rodata` name `${rustToolchain}/lib/rustlib/src/rust/library/...`, so Nix retains the whole toolchain as a runtime dependency. Prototyped already: 2.5 GiB → 58.8 MiB on `yomu-server`.

- [ ] **Step 1: Record the baseline**

```bash
nix path-info -Sh .#yomu-server
nix path-info -Sh .#yomu-desktop
nix-store -q --references $(nix build .#yomu-server --no-link --print-out-paths)
```

Expected: 2.5 GiB, 3.4 GiB, and a reference list containing `rust-default-1.96.1`. Write the numbers down; they go in the commit message.

- [ ] **Step 2: Check whether `buildRustPackage` already sets RUSTFLAGS**

```bash
nix derivation show .#yomu-server | grep -i rustflags
```

If it prints a value, the new flag must be **appended** to it, not assigned over it. This matters: the prototype's binary grew 138 KB unexplained, and clobbering an existing flag is the leading suspect. Record what you find in the commit message either way.

- [ ] **Step 3: Add the remap flag to all three packages**

In `flake.nix`, for `yomu-server` and `yomu-desktop`, next to the existing `env.YOMU_BUILD_COMMIT`:

```nix
      # Panic locations otherwise embed ${rustToolchain}/lib/rustlib/src/...,
      # which Nix reads as a runtime reference and follows into rustc, docs,
      # rust-analyzer, clippy, rustfmt and gcc — 2.4 GiB of build tooling in
      # the closure of a 13 MB binary.
      env.RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rust-toolchain";
```

For `yomu-web` (a `mkDerivation`, so plain attribute style like its neighbouring `YOMU_BUILD_COMMIT`):

```nix
      RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rust-toolchain";
```

If step 2 found an existing value, write `env.RUSTFLAGS = "<existing> --remap-path-prefix=…";` instead.

- [ ] **Step 4: Rebuild and verify the closures**

```bash
nix path-info -Sh .#yomu-server
nix path-info -Sh .#yomu-desktop
nix path-info -rSh .#yomu-server | grep -c 'rust-default\|rustc-\|rust-docs\|clippy\|rustfmt\|rust-analyzer'
```

Expected: server ≈ 58.8 MiB, desktop far below 3.4 GiB (webkitgtk still dominates), grep count `0`.

- [ ] **Step 5: Verify the binary still works**

```bash
nix run .#yomu-server -- --help 2>&1 | head -5   # or boot it against a temp config
```

The build's own `checkPhase` runs `cargoTestFlags`, so the test suite has already passed by this point.

- [ ] **Step 6: Commit**

```bash
git add flake.nix
git commit -m "nix: keep the rust toolchain out of the runtime closures"
```

The message must carry before/after closure sizes and what step 2 found.

---

### Task 2: Stop installing the 170 MB static library

**Files:**
- Modify: `flake.nix` (`yomu-desktop` `postInstall`, ~line 160)

**Why:** `crates/yomu-shell` declares `crate-type = ["staticlib", "cdylib", "rlib"]` for the Android build, so `buildRustPackage` installs `lib/libyomu_shell_lib.a` — 170 870 786 B of the desktop output's 58 MB… which is to say the output is mostly it.

- [ ] **Step 1: Record the baseline**

```bash
OUT=$(nix build .#yomu-desktop --no-link --print-out-paths)
du -sh $OUT && ls -la $OUT/lib/
```

- [ ] **Step 2: Remove it in `postInstall`**

At the top of `yomu-desktop`'s existing `postInstall`:

```nix
      postInstall = ''
        # The staticlib/cdylib exist for the Android build; on the desktop
        # only bin/yomu-shell runs, and the .a alone is 170 MB.
        rm -f $out/lib/libyomu_shell_lib.a
        install -Dm644 crates/yomu-shell/icons/128x128.png \
```

- [ ] **Step 3: Verify size and that the binary still runs**

```bash
OUT=$(nix build .#yomu-desktop --no-link --print-out-paths)
du -sh $OUT
ls $OUT/lib/ $OUT/bin/
```

Expected: output ~13 MB, no `.a`, `bin/yomu-shell` and `.yomu-shell-wrapped` intact.

- [ ] **Step 4: Commit**

```bash
git add flake.nix
git commit -m "nix: don't install the desktop static library"
```

---

### Task 3: Stop publishing the AppImage

**Files:**
- Modify: `.github/workflows/release.yml` (bundle step ~line 59, release body ~line 97)
- Modify: `README.md` (~line 53 area, desktop instructions), `docs/ARCHITECTURE.md`, `docs/HANDOFF.md` if they mention the AppImage

**Why:** 1 068 182 048 B per release, and the same size in every release since 1.12.0. Even with tasks 1–2 it stays several hundred MB, because it packs webkitgtk (825 MiB) and gst-plugins-base (310 MiB). The server is already consumed through the flake.

- [ ] **Step 1: Find every mention**

```bash
grep -rn -i "appimage" --include='*.yml' --include='*.md' --include='*.nix' .
```

- [ ] **Step 2: Delete the build step and its upload**

Remove the `Build desktop AppImage` step from `.github/workflows/release.yml` and the AppImage entries from whatever attaches assets. Leave the web bundle steps and the tag-version check alone.

- [ ] **Step 3: Update the release body text**

The workflow's release notes describe the AppImage ("self-contained x86_64 Linux build of the Tauri shell (WebKitGTK bundled)… `chmod +x`"). Replace with the flake instruction:

```
nix run github:tdbmxyz/yomu#yomu-desktop
```

- [ ] **Step 4: Update the docs**

`README.md` and any doc that tells a user how to get the desktop app must now say `nix run github:tdbmxyz/yomu#yomu-desktop`. Say plainly that desktop use needs nix; don't imply a binary download still exists.

- [ ] **Step 5: Verify the workflow still parses**

```bash
gh workflow view release.yml 2>/dev/null | head -5   # or: python -c 'import yaml,sys;yaml.safe_load(open(".github/workflows/release.yml"))'
```

`python3` is not installed on this machine — if neither check is available, read the file and confirm the YAML nesting by eye rather than skipping the step silently.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml README.md docs/
git commit -m "release: stop publishing the AppImage"
```

---

### Task 4: Precompress the web bundle and serve it

**Files:**
- Modify: `flake.nix` (new `yomu-web-compressed` package after `yomu-web`, and the `packages` set ~line 183)
- Modify: `nix/module.nix` (`webPackage` default ~line 25)
- Modify: `crates/yomu-server/src/api/mod.rs` (~line 94)
- Test: `crates/yomu-server/src/api/mod.rs` (tests module at the bottom)

**Why:** the wasm compresses 5.4× and nothing in the chain compresses. Build-time siblings cost nothing per request, unlike `CompressionLayer`.

- [ ] **Step 1: Write the failing service test**

In the `tests` module of `crates/yomu-server/src/api/mod.rs`. It builds a throwaway dist in `std::env::temp_dir()` — no `tempfile` dependency. The `.br` fixture does **not** need to be valid brotli; `ServeDir` serves the file and labels it.

```rust
    /// A dist with a precompressed sibling: the server must hand the sibling
    /// to a client that accepts brotli, and the plain file to one that does
    /// not. Without this the 3.7 MB wasm ships uncompressed to every visitor.
    #[tokio::test]
    async fn static_files_prefer_a_precompressed_sibling() {
        let dir = std::env::temp_dir().join("yomu-precompressed-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.wasm"), b"plain-wasm-bytes").unwrap();
        std::fs::write(dir.join("app.wasm.br"), b"brotli-wasm-bytes").unwrap();
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let mut config = Config::default();
        config.static_dir = Some(dir.clone());
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let req = Request::builder()
            .uri("/app.wasm")
            .header("accept-encoding", "br")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-encoding").and_then(|v| v.to_str().ok()),
            Some("br")
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"brotli-wasm-bytes");

        let req = Request::builder()
            .uri("/app.wasm")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(resp.headers().get("content-encoding").is_none());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"plain-wasm-bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A local `trunk build` dist has no siblings at all; it must still serve.
    /// This is what keeps `just web` and a hand-built dist working.
    #[tokio::test]
    async fn static_files_fall_back_to_identity_without_a_sibling() {
        let dir = std::env::temp_dir().join("yomu-no-sibling-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.js"), b"console.log(1)").unwrap();
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let mut config = Config::default();
        config.static_dir = Some(dir.clone());
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/app.js")
            .header("accept-encoding", "br, gzip")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-encoding").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run them and watch the first one fail**

```bash
cargo test -p yomu-server static_files_ -- --nocapture
```

Expected: `static_files_prefer_a_precompressed_sibling` fails (no `content-encoding`); the fallback test passes already.

- [ ] **Step 3: Turn on precompressed serving**

`crates/yomu-server/src/api/mod.rs`, replacing the bare `ServeDir`:

```rust
    if let Some(dir) = &state.config.static_dir {
        let index = dir.join("index.html");
        // Siblings are generated once at build time (see yomu-web-compressed
        // in flake.nix); ServeDir picks one by Accept-Encoding and falls back
        // to the identity file when none exists, so a plain local dist works
        // unchanged.
        let index = ServeFile::new(index).precompressed_br().precompressed_gzip();
        let files = ServeDir::new(dir)
            .precompressed_br()
            .precompressed_gzip()
            .fallback(index);
        app = app.fallback_service(files);
    }
```

`tower-http` already has the `fs` feature (`Cargo.toml:53`); no dependency change.

- [ ] **Step 4: Both tests pass**

```bash
cargo test -p yomu-server static_files_
```

- [ ] **Step 5: Add the compressed package to `flake.nix`**

After the `yomu-web` derivation:

```nix
    # What the server serves: the trunk dist plus brotli/gzip siblings built
    # once here, so ServeDir never compresses per request. Kept separate from
    # yomu-web because yomu-desktop bakes that one into the binary, where the
    # siblings would be ~700 KB of files the asset protocol never serves.
    yomu-web-compressed = pkgs.runCommand "yomu-web-compressed-${version}" {
      nativeBuildInputs = [pkgs.brotli pkgs.gzip];
    } ''
      cp -r ${yomu-web} $out
      chmod -R u+w $out
      find $out -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' \
        -o -name '*.html' -o -name '*.json' -o -name '*.svg' \
        -o -name '*.webmanifest' \) -print0 |
        while IFS= read -r -d "" f; do
          brotli -q 11 -f -o "$f.br" "$f"
          gzip -9 -c "$f" > "$f.gz"
        done
    '';
```

and add it to the `packages` set alongside the others.

- [ ] **Step 6: Point the module at it**

`nix/module.nix`, the `webPackage` option default:

```nix
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.yomu-web-compressed;
      defaultText = lib.literalExpression "yomu.packages.\${system}.yomu-web-compressed";
```

- [ ] **Step 7: Verify the sizes and that the desktop package did NOT get siblings**

```bash
WEB=$(nix build .#yomu-web-compressed --no-link --print-out-paths)
ls -la $WEB | head
du -sb $WEB
nix derivation show .#yomu-desktop | grep -c 'yomu-web-compressed'
```

Expected: `.br`/`.gz` next to each asset, the `_bg.wasm.br` near 685 KB, and the last command prints `0` — the desktop binary must embed the plain dist only.

- [ ] **Step 8: Commit**

```bash
git add flake.nix nix/module.nix crates/yomu-server/src/api/mod.rs
git commit -m "nix,server: serve precompressed web assets"
```

Message carries the served sizes: wasm 3 698 836 → ~685 000 on the wire.

---

### Task 5: Wire the wasm size profile, measuring before choosing

**Files:**
- Modify: `crates/yomu-web/index.html`
- Possibly modify: `Cargo.toml` (`[profile.wasm-release]`, line 71)

**Why:** `[profile.wasm-release]` has existed and never been used; release wasm builds run at `opt-level = 3`, with a 565 089 B `name` section.

**Trap:** this must go through trunk, never a post-build hook. Trunk computes the SRI hash *after* wasm-opt and embeds it in `index.html`; rewriting the wasm afterwards yields "Failed to find a valid digest in the integrity attribute" and a page that will not boot.

- [ ] **Step 1: Record the baseline**

```bash
cd crates/yomu-web && trunk build --release
stat -c%s dist/*_bg.wasm
nix shell nixpkgs#brotli --command bash -c 'brotli -q 11 -c dist/*_bg.wasm | wc -c'
```

Expected: 3 698 836 and 684 728.

- [ ] **Step 2: Add the trunk directive**

In `crates/yomu-web/index.html`, alongside the other `data-trunk` links:

```html
    <!-- Release builds go through [profile.wasm-release] (opt-level, panic =
         abort) and wasm-opt; trunk hashes the result for SRI, so this must
         never be done as a post-build step. `trunk serve` keeps the fast dev
         profile — data-cargo-profile-release applies to release only. -->
    <link data-trunk rel="rust" data-cargo-profile-release="wasm-release"
          data-wasm-opt="z" data-wasm-opt-params="--strip-debug --strip-producers" />
```

- [ ] **Step 3: Measure `opt-level = "z"`**

```bash
cd crates/yomu-web && trunk build --release
stat -c%s dist/*_bg.wasm
nix shell nixpkgs#brotli --command bash -c 'brotli -q 11 -c dist/*_bg.wasm | wc -c'
nix shell nixpkgs#wabt --command bash -c 'wasm-objdump -h dist/*_bg.wasm | grep -i custom'
```

The `name` and `producers` sections must be gone.

- [ ] **Step 4: Measure `opt-level = "s"`**

Change `Cargo.toml:73` to `opt-level = "s"`, rebuild, record both numbers, then set it back to `"z"` for now.

- [ ] **Step 5: Report both, then choose**

Put the four numbers (raw and brotli, for `z` and `s`) in the task report. Default to `"z"` — it is what the profile says and what won on the sibling project — and flag the delta so the human can overrule. If `s` is within ~5% of `z`, prefer `s`: it keeps more inlining, and the reader does real per-frame work on a phone and an e-ink tablet.

- [ ] **Step 6: Verify the app actually boots**

`panic = "abort"` is a real behaviour change. Serve the built dist and load it:

```bash
cd /projects/rust/yomu && cargo run -p yomu-server &   # with static_dir at the dist
curl -s localhost:4700/ | grep -c integrity
```

The SRI attributes must match the served files — if the page 200s but the browser would refuse, that is exactly the trap above. State clearly in the report whether a browser actually rendered the app or only that the bytes were served; do not claim a boot that was not observed.

- [ ] **Step 7: Commit**

```bash
git add crates/yomu-web/index.html Cargo.toml
git commit -m "web: build release wasm through the size profile"
```

Message carries baseline → `z` → `s`, raw and brotli.

---

### Task 6: `Cache-Control` and `Vary` on static assets

**Files:**
- Create: `crates/yomu-server/src/api/static_cache.rs`
- Modify: `crates/yomu-server/src/api/mod.rs` (module declaration; wrap the static service from task 4)

**Why:** no cache headers at all today, so every navigation revalidates and every force-reload refetches the bundle. Smaller win for yomu than for the sibling project — the service worker absorbs most repeat loads — but cheap.

**Trap:** `immutable` on a non-fingerprinted file pins it for a year and users cannot clear it. The negative tests matter more than the positive ones.

- [ ] **Step 1: Write the failing unit tests**

Create `crates/yomu-server/src/api/static_cache.rs` with the tests first:

```rust
//! Cache headers for the static frontend. Trunk emits content-hashed
//! filenames, which can be pinned for a year because a change always
//! arrives under a new URL; everything else must revalidate.

#[cfg(test)]
mod tests {
    use super::cache_control_for;

    #[test]
    fn fingerprinted_assets_are_immutable() {
        assert_eq!(
            cache_control_for("/yomu-web-9da5a24d4d3677cc_bg.wasm"),
            super::IMMUTABLE
        );
        assert_eq!(
            cache_control_for("/styles-dcb9e8dca193296c.css"),
            super::IMMUTABLE
        );
    }

    /// The failure that users cannot clear: a year-long pin on something that
    /// changes in place.
    #[test]
    fn everything_else_revalidates() {
        for path in [
            "/index.html",
            "/",
            "/sw.js",
            "/manifest.webmanifest",
            "/favicon.svg",
            "/icon-192.png",
            "/library",
            // uuid groups are 8/4/4/4/12 hex — never 16, so an API path
            // cannot be mistaken for a fingerprint
            "/api/v1/manga/019f4921-3946-7c20-9a67-d84d46072fe6",
        ] {
            assert_eq!(cache_control_for(path), super::REVALIDATE, "{path}");
        }
    }
}
```

- [ ] **Step 2: Run and watch it fail to compile**

```bash
cargo test -p yomu-server cache_control
```

Expected: `cannot find function cache_control_for`.

- [ ] **Step 3: Implement the classifier and the layer**

Above the tests in the same file:

```rust
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const REVALIDATE: &str = "public, max-age=0, must-revalidate";

/// Trunk's fingerprint is a 16-hex-character segment of the filename
/// (`yomu-web-9da5a24d4d3677cc_bg.wasm`, `styles-<hash>.css`). Split on `-`,
/// `_` and `.` so the `_bg` suffix doesn't hide it.
pub(crate) fn cache_control_for(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let hashed = name
        .split(['-', '_', '.'])
        .any(|seg| seg.len() == 16 && seg.chars().all(|c| c.is_ascii_hexdigit()));
    if hashed { IMMUTABLE } else { REVALIDATE }
}

/// Applied to the static service only — never the whole app, or API
/// responses would get cache headers too.
pub(crate) async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(&path)),
    );
    // ServeDir sets Content-Encoding but not Vary; `immutable` without Vary
    // lets a shared cache hand a brotli body to a client that never asked.
    headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    response
}
```

- [ ] **Step 4: Tests pass**

```bash
cargo test -p yomu-server cache_control
```

- [ ] **Step 5: Wire it over the static service only**

In `crates/yomu-server/src/api/mod.rs`, declare `mod static_cache;` and wrap the service built in task 4:

```rust
        let files = ServeDir::new(dir)
            .precompressed_br()
            .precompressed_gzip()
            .fallback(index);
        let files = tower::ServiceBuilder::new()
            .layer(axum::middleware::from_fn(static_cache::cache_headers))
            .service(files);
        app = app.fallback_service(files);
```

- [ ] **Step 6: Add a service-level assertion**

In the `tests` module of `api/mod.rs`, extend the precompressed test (or add one) asserting a fingerprinted asset comes back `immutable` while `/index.html` does not, and that an API response has **no** `cache-control` — that last one is the regression guard for layering it in the wrong place.

- [ ] **Step 7: Full suite + commit**

```bash
just check && cargo test --workspace --exclude yomu-shell
git add crates/yomu-server/src/api/
git commit -m "server: cache headers for fingerprinted static assets"
```

---

### Task 7: Paint before the wasm boots

**Files:**
- Modify: `crates/yomu-web/index.html`
- Modify: `crates/yomu-web/src/main.rs`

**Why:** `<body>` is empty until the wasm instantiates, so the first visit is a white screen even at ~450 KB.

**Trap:** `mount_to_body` **appends**; it does not replace body content. Without removing the skeleton it stays behind the app, `position: fixed`, covering it.

- [ ] **Step 1: Add the skeleton and an inline style**

In `crates/yomu-web/index.html`. The hashed stylesheet may not have arrived yet, so the two colours are hardcoded from `styles.css` (`--bg: #14171c`, `--accent: #4fd1c5`):

```html
    <style>
      /* Shown until the wasm mounts (removed in main.rs). Colours are
         hardcoded because the hashed stylesheet may not have landed yet. */
      #yomu-boot {
        position: fixed;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #14171c;
      }
      #yomu-boot span {
        width: 2rem;
        height: 2rem;
        border: 3px solid #262b33;
        border-top-color: #4fd1c5;
        border-radius: 50%;
        animation: yomu-spin 0.9s linear infinite;
      }
      @keyframes yomu-spin { to { transform: rotate(360deg); } }
      @media (prefers-reduced-motion: reduce) {
        #yomu-boot span { animation: none; border-top-color: #4fd1c5; }
      }
    </style>
```

and in `<body>`:

```html
  <body>
    <div id="yomu-boot"><span></span></div>
  </body>
```

- [ ] **Step 2: Remove it after mount**

In `crates/yomu-web/src/main.rs`, right after the existing `mount_to_body(...)` call:

```rust
    // mount_to_body appends: without this the boot skeleton stays on top of
    // the app it was covering for.
    if let Some(node) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("yomu-boot"))
    {
        node.remove();
    }
```

`Document` and `Element` must be in the entry crate's web-sys features — check `crates/yomu-web/Cargo.toml` and add them if missing.

- [ ] **Step 3: Verify against the built dist, not the source**

```bash
cd crates/yomu-web && trunk build --release
grep -c 'yomu-boot' dist/index.html
grep -c '@keyframes yomu-spin' dist/index.html
```

Both must be non-zero: trunk preserves custom `<body>` content and inline `<style>`, but verify rather than assume.

- [ ] **Step 4: Compile for wasm and commit**

```bash
cargo check -p yomu-web --target wasm32-unknown-unknown
git add crates/yomu-web/
git commit -m "web: paint a skeleton before the wasm boots"
```

---

### Task 8: Strip the Android library

**Files:**
- Modify: `justfile` (the `apk` recipe created in task 9 — do task 9 first if you prefer, they touch the same recipe)

**Why:** `libyomu_shell_lib.so` is 10 195 528 B in the APK but its allocated sections total 7 864 008 B; the rest is `.symtab`/`.strtab`.

- [ ] **Step 1: Record the baseline**

```bash
stat -c%s target/aarch64-linux-android/release/libyomu_shell_lib.so
stat -c%s <the built apk>
```

- [ ] **Step 2: Add the strip flag to the Android build only**

In the `apk` recipe, prefix the tauri invocation:

```bash
    RUSTFLAGS="-C strip=symbols" cargo tauri android build --apk --target aarch64 …
```

Android only, deliberately: `[profile.release]` would also strip the server binary and cost readable backtraces in logs.

- [ ] **Step 3: Rebuild and measure**

```bash
just apk
stat -c%s target/aarch64-linux-android/release/libyomu_shell_lib.so
```

Expected: ~7.9 MB, and an APK smaller by roughly 1 MB (the `.so` is deflated inside the zip, so the APK saving is well under the raw 2.3 MB).

- [ ] **Step 4: Commit**

```bash
git add justfile
git commit -m "android: strip the shared library"
```

---

### Task 9: `just apk` with an injected version

**Files:**
- Modify: `justfile`
- Modify: `crates/yomu-shell/tauri.conf.json` (remove `"version"`)
- Modify: `.github/workflows/release.yml` (tag check reads only `Cargo.toml`)
- Modify: `README.md`, `docs/HANDOFF.md` (the documented APK command)

**Why:** `tauri.conf.json` pins a version that can drift from the workspace. Deleting the field alone is wrong — Tauri's Android generator writes `gen/android/app/tauri.properties` only when the config carries a version, with no Cargo fallback, so the APK silently builds as `versionName 1.0 / versionCode 1`, which future upgrades then reject. Inject it in the recipe instead.

- [ ] **Step 1: Add the recipe**

In `justfile`:

```make
# Signed release APK. The version comes from Cargo.toml and is injected, so
# tauri.conf.json can't drift from the workspace; tauri.properties is
# gitignored and keeps whatever version last resolved, so it is removed first.
# Signing reads crates/yomu-shell/gen/android/keystore.properties.
apk:
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    rm -f crates/yomu-shell/gen/android/app/tauri.properties
    nix develop .#android --command bash -c \
      "cd crates/yomu-shell && RUSTFLAGS='-C strip=symbols' cargo tauri android build --apk --target aarch64 --config '{\"version\":\"$version\"}'"
```

- [ ] **Step 2: Remove the version from the Tauri config**

Delete the `"version": "2.0.0",` line from `crates/yomu-shell/tauri.conf.json`.

- [ ] **Step 3: Drop the now-broken workflow check**

`.github/workflows/release.yml` reads `jq -r .version crates/yomu-shell/tauri.conf.json` and asserts it equals the tag. With the field gone that check fails every release. Remove the `shell_version` lines, keep the `Cargo.toml` one.

- [ ] **Step 4: Build and verify the artifact, not the config**

```bash
just apk
nix shell nixpkgs#androidenv.androidPkgs.build-tools --command \
  aapt2 dump badging crates/yomu-shell/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk | head -2
```

`versionName` must be the workspace version and `versionCode` must not be 1. If `aapt2` isn't reachable, say so rather than declaring the check done.

- [ ] **Step 5: Update the docs**

`README.md:53-56` and `docs/HANDOFF.md:116-121` document the raw `cargo tauri android build` command. Point them at `just apk` and note that the version is injected.

- [ ] **Step 6: Commit**

```bash
git add justfile crates/yomu-shell/tauri.conf.json .github/workflows/release.yml README.md docs/
git commit -m "android: build the apk through just, with the version injected"
```

---

### Task 10: Re-measure everything and close the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-07-25-delivery-size-design.md` (the outcome table)

- [ ] **Step 1: Collect the real numbers**

```bash
cd crates/yomu-web && trunk build --release
stat -c%s dist/*_bg.wasm dist/*.js dist/*.css
nix shell nixpkgs#brotli --command bash -c 'for f in dist/*_bg.wasm dist/*.js dist/*.css; do brotli -q 11 -c $f | wc -c; done'
cd /projects/rust/yomu
nix path-info -Sh .#yomu-server .#yomu-desktop
du -sh $(nix build .#yomu-desktop --no-link --print-out-paths)
stat -c%s <the apk>
```

- [ ] **Step 2: Replace the spec's prediction table with measurements**

Mark the table "measured" and keep the original prediction column beside it, so the estimate can be judged against reality rather than quietly replaced.

- [ ] **Step 3: Full verification**

```bash
just check && cargo test --workspace --exclude yomu-shell
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-07-25-delivery-size-design.md
git commit -m "docs: record measured delivery-size results"
```

---

## Notes for implementers

- **Nix builds are slow.** `yomu-desktop` compiles the Tauri shell with `lto = true` and `codegen-units = 1`; budget minutes, not seconds, and don't interpret a long build as a hang.
- **Don't touch `crates/yomu-web/sw.js`.** Compression and cache headers change neither the caching logic nor a fixed-name asset, URLs don't change, and the Cache API stores decoded responses. Bumping `CACHE` would needlessly evict every user's downloaded chapters.
- **`crates/yomu-web/dist/` is a build output** that exists in the working tree. Don't commit changes to it.
- **Scan-site names must never enter the repo, commit messages, or PR text.** If a measurement or log surfaces one, use "fixture" or "a source" instead.
- **Report measurements honestly.** Every task here is judged by a number; if a step could not be run (a tool missing from the devshell, a build that would not finish), say so in the report instead of substituting an estimate.
