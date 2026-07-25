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

# Signed release APK. The version comes from Cargo.toml and is injected, so
# tauri.conf.json can't drift from the workspace; tauri.properties is
# gitignored and keeps whatever version last resolved, so it is removed first.
# Signing reads crates/yomu-shell/gen/android/keystore.properties.
# The strip flag is set here rather than in [profile.release] on purpose:
# the same profile builds yomu-server, whose backtraces need symbols. It is
# scoped to the android target rather than passed as plain RUSTFLAGS because
# tauri's beforeBuildCommand runs `trunk build` inside this invocation, and
# stripping the wasm drops its target_features section, after which wasm-opt
# rejects the bulk-memory ops rustc emits.
apk:
    #!/usr/bin/env bash
    set -euo pipefail
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    rm -f crates/yomu-shell/gen/android/app/tauri.properties
    nix develop .#android --command bash -c \
      "cd crates/yomu-shell && CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS='-C strip=symbols' cargo tauri android build --apk --target aarch64 --config '{\"version\":\"$version\"}'"

fmt:
    cargo fmt --all

test:
    cargo nextest run --workspace --exclude yomu-shell
