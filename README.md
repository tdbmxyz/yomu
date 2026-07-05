# yomu (読む)

Self-hosted manga/webtoon library: track what you read, download chapters on
the server from scan sites, read from any browser on the LAN — with your
position (chapter + page) following you. Sibling project of
[chaos](../chaos), built on the same stack and conventions.

## What it does

- **Library**: search a source, track a manga, browse chapters.
- **Sources without extensions**: a scan site is a TOML file with CSS
  selectors (`sources.d/*.toml`) — no code, no extension ecosystem.
- **Local series** (à la Suwayomi): drop `local/<Series>/<Chapter>/*.png`
  (or `<Chapter>.cbz`) on the server and it's a searchable, trackable
  source like any other.
- **Server-side downloads**: chapters are fetched to the server's disk by a
  queue worker; or read **live** (proxied page by page, nothing stored).
- **Progress tracking**: current chapter + page, stored as an append-only
  journal designed for future offline clients that merge on reconnect.
- **Optional sign-in**: point `[auth]` at an OIDC provider (authentik) for
  per-user reading positions; without it everyone shares one account and
  the same position — zero login friction.
- **Categories**: Reading / Paused / Finished; only categories you opt in
  are checked for new chapters.
- **Updates**: the server periodically re-checks tracked manga (in
  update-enabled categories) and auto-downloads new chapters where enabled.

## Stack & layout

Leptos 0.8 (CSR, trunk) + Axum + sqlx/SQLite + Nix flake — see chaos for the
rationale (ADRs there apply; yomu-specific decisions in `docs/adr/`).

| Crate | Role |
| --- | --- |
| `yomu-domain` | Types + API contract + progress journal merge rule |
| `yomu-source` | `Source` trait + selector scan-site impl + local source |
| `yomu-server` | Axum backend: library, downloader, updater, page serving |
| `yomu-client` | Typed API client (native & wasm) |
| `yomu-ui` | Leptos pages: library, search, manga, reader |
| `yomu-web` | Trunk entrypoint |

## Development

```console
$ nix develop
$ just server            # backend on http://127.0.0.1:4700
$ just web               # frontend with hot reload on http://127.0.0.1:8081
$ just check && just test
```

Add a scan site: copy `crates/yomu-server/sources.d/example.toml.sample` to
`<sources_dir>/<site>.toml`, adjust the selectors (browser devtools on the
site), restart the server.

First checkout: enter the shell, `cargo generate-lockfile`,
`git add Cargo.lock`, re-enter (wasm-bindgen-cli pinning, as in chaos).
