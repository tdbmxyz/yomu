# New-chapter notifications via ntfy — design

## Problem

New chapters are discovered server-side (the periodic updater), but
nothing tells the reader. The Android app is a webview shell with no
service worker and no background execution guarantee, so app-side push
is unreliable by construction. The self-hosted-friendly answer is a
server-side push to an [ntfy](https://ntfy.sh) topic: the ntfy app on
the phone delivers real notifications even when yomu is closed.

## Config

```toml
[notify]
url = "https://ntfy.example.net/yomu"   # ntfy topic URL
# token = "tk_..."                      # optional access token
```

Absent `[notify]` section → feature off, zero behavior change. The
token, when set, is sent as `Authorization: Bearer <token>`.

## Behavior

- **Only the periodic updater notifies.** Manual refreshes (the user is
  in the app) and adding a manga (initial import would announce the
  whole backlog) do not.
- One POST per manga per sync round, not per chapter:
  - HTTP `X-Title` header: the manga title
  - Body: the chapter title for one new chapter ("Chapter 171"); for
    several, `"3 new chapters — Chapter 171 … Chapter 173"` (count,
    first and last by listing order)
  - `X-Tags: books`
- Fire-and-forget with a `tracing::warn!` on failure: an unreachable
  ntfy must never fail or delay a sync. No retry queue — the next round
  notifies only genuinely-new chapters, so a lost push stays lost
  (acceptable).
- Re-upload twin merges never notify: `sync_chapters` already excludes
  merged twins from `new_chapters`.

## Components

- `crates/yomu-server/src/notifier.rs` — `Notifier` built from the
  config section, holding the shared `reqwest::Client`; one method
  `notify_new_chapters(manga_title: &str, chapters: &[Chapter])` (no-op
  when unconfigured). Message formatting is a pure function, unit-tested.
- Config: `NotifyConfig { url, token }` added to the server's config
  struct as `Option<NotifyConfig>`.
- `sync.rs::refresh_manga` returns the new chapters themselves
  (`Vec<Chapter>`) instead of a count — the updater needs titles;
  existing callers take `.len()`.
- `updater.rs` calls the notifier after each manga refresh that returned
  a non-empty list.

## Testing

- Unit tests for the message formatting (one chapter, several, title
  passthrough).
- Integration-style test for `Notifier` against a local mock HTTP server
  (bind a `tokio` listener): asserts method, `X-Title`, body, and the
  `Authorization` header when a token is set.
- Updater-only scoping is enforced by construction (only `updater.rs`
  holds a notifier call), asserted by review rather than a test.
- Live verification during E2E: point `[notify]` at a scratch ntfy.sh
  topic and confirm a push arrives on a real new-chapter sync.

## Rollout

Server-only feature: ships with the next server release; enable by
adding `[notify]` to the production config on zeus and subscribing to
the topic in the ntfy Android app. No client change.
