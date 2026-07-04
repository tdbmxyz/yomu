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

# Full check: formatting, lints, native + wasm compilation.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo check -p yomu-web -p yomu-ui --target wasm32-unknown-unknown

fmt:
    cargo fmt --all

test:
    cargo test --workspace
