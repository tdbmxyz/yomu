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

fmt:
    cargo fmt --all

test:
    cargo nextest run --workspace --exclude yomu-shell
