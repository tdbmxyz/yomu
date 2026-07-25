# Development commands. Run inside `nix develop` (or with direnv active).

default:
    @just --list

# Run the backend with the example config (http://127.0.0.1:4700)
server:
    YOMU_CONFIG=crates/yomu-server/yomu.example.toml cargo run -p yomu-server

# Serve the frontend with hot reload on http://127.0.0.1:8081 (run `just server` in another terminal)
web:
    cd crates/yomu-web && trunk serve

# Build the production frontend bundle
build-web:
    cd crates/yomu-web && trunk build --release

# Full check: formatting, lints, native + wasm compilation. yomu-shell
# needs the webview stack — check it with `just check-shell` in `.#tauri`.
check:
    cargo fmt --all --check
    cargo clippy --workspace --exclude yomu-shell --all-targets -- -D warnings
    cargo check -p yomu-web -p yomu-ui --target wasm32-unknown-unknown

# Lint the Tauri shell (run inside `nix develop .#tauri`)
check-shell:
    cargo clippy -p yomu-shell --all-targets -- -D warnings

# Run the desktop shell against a server (run inside `nix develop .#tauri`)
shell server="http://127.0.0.1:4700":
    cd crates/yomu-web && trunk build --release
    YOMU_SERVER={{server}} cargo run -p yomu-shell

# The version comes from Cargo.toml and is injected, so tauri.conf.json can't
# drift from the workspace; tauri.properties is gitignored and keeps whatever
# version last resolved, so it is removed first. Signing reads
# crates/yomu-shell/gen/android/keystore.properties.
#
# The strip flag is set here rather than in [profile.release] on purpose: the
# same profile builds yomu-server, whose backtraces need symbols. It is scoped
# to the android target rather than passed as plain RUSTFLAGS because tauri's
# beforeBuildCommand runs `trunk build` inside this invocation, and stripping
# the wasm drops its target_features section, after which wasm-opt rejects the
# bulk-memory ops rustc emits.
#
# That scoping is also why the build is verified afterwards instead of trusted:
# CARGO_TARGET_<TRIPLE>_RUSTFLAGS is the env form of target.<triple>.rustflags,
# and cargo takes rustflags from the first source that applies rather than
# merging them — so a RUSTFLAGS in the caller's environment would outrank it
# and silently ship the symbol table again. The check reads the .so out of the
# finished APK and fails if .symtab survived.
#
# Signed release APK, version injected from Cargo.toml, library verified stripped
apk:
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)*$ ]]; then
      echo "just apk: no semver version found in Cargo.toml (read '$version')" >&2
      exit 1
    fi
    rm -f crates/yomu-shell/gen/android/app/tauri.properties
    nix develop .#android --command bash -c \
      "cd crates/yomu-shell && CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS='-C strip=symbols' cargo tauri android build --apk --target aarch64 --config '{\"version\":\"$version\"}'"
    apk="$(ls -t crates/yomu-shell/gen/android/app/build/outputs/apk/*/release/*.apk | head -1)"
    nix develop .#android --command bash -c '
      set -euo pipefail
      so="$(mktemp)"
      unzip -p "$1" lib/arm64-v8a/libyomu_shell_lib.so > "$so"
      if readelf -S "$so" | grep -qE "\.symtab($|[^A-Za-z0-9_])"; then
        rm -f "$so"
        echo "just apk: $1 ships an unstripped libyomu_shell_lib.so (.symtab present)." >&2
        echo "just apk: a RUSTFLAGS set in your environment overrides the per-target one; unset it." >&2
        exit 1
      fi
      rm -f "$so"
      echo "just apk: $1 — libyomu_shell_lib.so is stripped."
    ' _ "$apk"

fmt:
    cargo fmt --all

test:
    cargo nextest run --workspace --exclude yomu-shell
