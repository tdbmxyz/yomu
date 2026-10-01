# yomu (読む)

Self-hosted manga/webtoon and book library: download chapters from scan sites,
import local EPUB/PDF books, and read from any browser on the LAN — with your
section progression or page position following you. Sibling project of
[chaos](../chaos), built on the same stack and conventions.

## What it does

- **Library**: search a source, track a manga, browse chapters.
- **Sources without extensions**: a scan site is a TOML file with CSS
  selectors (`sources.d/*.toml`) — no code, no extension ecosystem.
- **Local series**: drop `<Series>/<Chapter>/*.png` (or `<Chapter>.cbz`)
  under the configured `books.dir`; the streamer adds them directly to the library.
- **Books and versions**: drop standalone `.epub` or `.pdf` files directly under
  `books.dir`. All books share the Books shelf; matching filename stems (for
  example `My Book.epub` and `My Book.pdf`) appear as selectable versions of one
  work. Each version keeps its own progress and reading order. Categories apply
  to the work; removing a version leaves its siblings intact. Different filenames
  are not automatically linked merely because metadata titles match.
  `.mobi`/`.azw3` versions are catalogued and downloadable, not readable yet.
  PDF covers are generated lazily from page one and cached. Nix packages include
  Poppler; non-Nix servers need `pdftoppm` (Poppler utilities) on PATH.
  EPUB follows the spine, with section navigation and text-size
  controls, adjustable width and readable night/paper colors; PDF has selectable text and fit/zoom
  controls. Touch swipes navigate, with text pinch for EPUB and pinch/pan for PDF.
  Both share the comic reader's back control and tap-to-hide chrome, and resume
  where you stopped. See [reader architecture and limits](docs/adr/0004-publication-navigators.md).
- **Server-side downloads**: chapters are fetched to the server's disk by a
  queue worker; or read **live** (proxied page by page, nothing stored).
- **Progress tracking**: current unit + page or text progression, stored as an append-only
  journal; offline-capable clients merge queued events on reconnect.
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
| `yomu-source` | `Source` trait + selector scan-site impl |
| `yomu-server` | Axum backend: library, downloader, updater, page serving, streamer (local books dir) |
| `yomu-client` | Typed API client (native & wasm) |
| `yomu-store` | SQLite journal, sync cursor, device metadata, and native client state |
| `yomu-ui` | Leptos pages: library, search, manga, reader |
| `yomu-web` | Trunk entrypoint |
| `yomu-shell` | Tauri v2 desktop/Android shell around the same UI |

## Development

```console
$ nix develop
$ npm ci                 # pinned PDF.js assets and Playwright tooling
$ just server            # backend on http://127.0.0.1:4700
$ just web               # frontend with hot reload on http://127.0.0.1:8081
$ just check && just test
$ just e2e               # real Chromium + fixture source/IdP
```

Playwright runs the same deterministic journeys in CI. For AI-assisted test
planning/generation/healing in pi, install `pi-mcp-adapter`; the checked-in
`.mcp.json` and `.pi/skills/playwright-e2e/` connect pi to Playwright MCP while
CI remains model- and credential-free.

Desktop shell: `nix develop .#tauri`, then `just shell http://<server>:4700`
(or set `~/.config/yomu/server`). Android: `just apk` — it enters
`nix develop .#android` itself and injects the workspace version from
`Cargo.toml`, so the APK can never be stamped with a stale one. On first
launch the app shows a connect screen asking for the server URL. Release
signing reads `crates/yomu-shell/gen/android/keystore.properties` (see the
sample; the keystore lives outside the repo).

Add a scan site: copy `crates/yomu-server/sources.d/example.toml.sample` to
`<sources_dir>/<site>.toml`, adjust the selectors (browser devtools on the
site), restart the server. Production readiness, metrics, SQLite backup, and
maintenance guidance is in [`docs/OPERATIONS.md`](docs/OPERATIONS.md).
Release safety, test drills, size budgets, and documentation policy are in
[`docs/MAINTENANCE.md`](docs/MAINTENANCE.md).

First checkout: enter the shell, `cargo generate-lockfile`,
`git add Cargo.lock`, re-enter (wasm-bindgen-cli pinning, as in chaos).

## Clients

The browser is the primary client: point it at the server. There is no
prebuilt desktop download — the desktop shell requires nix, and is run
from the flake:

```console
$ nix run github:tdbmxyz/yomu#yomu-desktop
```

That needs nix with flakes enabled (`nix-command` and `flakes` experimental
features); on a non-NixOS host a WebKitGTK app run from the store may also
need a GL wrapper such as `nixGL`.

It asks for the server address on first launch (or read it from
`YOMU_SERVER` / `~/.config/yomu/server`). Android is an APK, signed
locally and attached to a release by hand.

## Branching & releases

Git flow, enforced by CI on the long-lived branches:

- `main` — protected; only receives merges from `develop`, every commit
  on it is releasable.
- `develop` — integration branch; feature branches (`feat/…`, `fix/…`)
  target it through pull requests.
- Releases: bump the workspace version in `Cargo.toml` (the only place it
  lives — `tauri.conf.json` carries no version; `just apk` injects it),
  merge `develop` into `main`, tag `vX.Y.Z`. The release workflow checks
  the tag against `Cargo.toml` and creates release metadata; deployments
  consume the tagged Nix flake, so no web bundle is attached. The Android
  APK is signed locally and attached by hand. The desktop shell is not
  published as an artifact — it is run from the flake (see Clients above).

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE). yomu is a self-hosted reader;
it ships no content and no site-specific source definitions.
