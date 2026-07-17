# New-Chapter Notifications Design

Date: 2026-07-18

## Problem

When the periodic updater finds new chapters for tracked titles, nothing
reaches the user unless they open the app and look. Wanted: native OS
notifications from the shells — including on Android while the app is
closed — plus the existing server-side ntfy path documented and kept as
the zero-infra push option. Scoping stays what the updater already does:
only categories with auto-update enabled (e.g. Reading) are announced.

Also a small UI fix: while a chapter download animates on the manga
page, the category select briefly disappears and reappears.

## Decisions (user)

- Shells poll while the app is alive and raise OS notifications via the
  Tauri notification plugin; ntfy remains the push channel for anyone
  who wants phone push without the app running.
- Shells only — the plain browser tab does not request Web Notification
  permission.
- End state must include Android notifications while the app is off:
  delivered natively via a WorkManager periodic worker in the Android
  shell (no FCM, no foreground service).

## Design

### 1. Server updates feed

The updater already computes "new chapters found for manga X this
round" (it feeds the ntfy notifier). Persist that as an event:

- New table `updates`:
  `id INTEGER PK, manga_id TEXT, chapter_count INTEGER,
  first_title TEXT, last_title TEXT, created_at TEXT` — one row per
  manga per updater round that found chapters. Written in
  `updater::run` at the same point `notify_new_chapters` is called;
  manual refreshes and library adds never write it (same semantics as
  ntfy: only the updater announces).
- `GET /api/v1/updates?since=<RFC3339>` → `{ updates: [ { manga_id,
  manga_title, chapter_count, first_title, last_title, created_at } ] }`,
  newest first, joined with the manga title, capped at 100 rows. Same
  auth policy as the other read routes.
- Rows older than 30 days are pruned at the start of each updater round.

### 2. Shell in-app polling

- `tauri-plugin-notification` added to yomu-shell (Android + desktop,
  one API); capability granted; the plugin drives the Android 13+
  `POST_NOTIFICATIONS` runtime prompt.
- yomu-ui: a shell-only loop started from `App` when
  `shell_available()`: on boot and then every 15 minutes, if
  connectivity is Online, fetch `/updates?since=<watermark>`; for each
  entry raise one OS notification titled with the manga title, body in
  the ntfy message format ("Chapter 171" / "3 new chapters — … … …");
  then advance the watermark to the newest returned `created_at` (an
  empty response leaves it untouched).
- Watermark storage: localStorage key `yomu-updates-seen` — except on
  Android, where the Kotlin side owns it (SharedPreferences) so the
  in-app loop and the background worker share one cursor (see 3).
  First run ever: set the watermark to now and notify nothing.
- Notification tag/id = manga id, so a duplicate for the same manga
  replaces rather than stacks.

### 3. Android app-off notifications (WorkManager)

- `UpdatesWorker` (Kotlin, in the shell's Android project next to
  `MainActivity`): reads base URL + watermark from SharedPreferences,
  GETs `/api/v1/updates?since=`, parses the JSON, posts one
  notification per entry on a "New chapters" channel
  (NotificationManagerCompat, tag = manga id, tap opens the app),
  advances the watermark. Missing permission or unreachable server =
  silent no-op (next run retries; watermark only advances after a
  successful fetch).
- Scheduled as a `PeriodicWorkRequest` (30 min, network-connected
  constraint, `ExistingPeriodicWorkPolicy.UPDATE`) — WorkManager
  persists it across app kills and reboots.
- Config handoff: the existing `YomuAndroid` JS bridge gains
  `configureUpdates(baseUrl)`; the UI calls it on boot with the
  resolved API base. The bridge stores the base URL and (re)schedules
  the worker. The bridge also exposes `updatesWatermark()` /
  `setUpdatesWatermark(ts)` so the in-app loop uses the shared cursor
  on Android.
- On Android the in-app loop still runs (instant on-open check); the
  shared watermark plus per-manga notification tags keep the two
  paths from double-notifying.
- OIDC-mode servers: the worker has no session cookie, so its fetch
  fails and it stays quiet — app-off notifications are a
  single-account-mode feature for now (matches the deployed setup).
- Desktop shell: no app-off path (out of scope — closing the desktop
  app means no notifications, ntfy covers that if wanted).

### 4. ntfy (server side)

Already shipped in v1.4.0: a `[notify]` config section
(`url = "https://ntfy.sh/<topic>"`, optional `token`) makes the updater
push one message per manga. No code change; deploying it on the server
host is a config edit. This spec only documents it as the
push-when-closed companion.

### 5. Category select flicker fix

Symptom: during download animations the category select vanishes and
pops back. Suspected cause (verify before fixing): every `refresh` bump
(download poll, local saves) refetches the manga detail and remounts
the actions row; the select renders from a `LocalResource` that yields
`None` mid-refetch, so it unmounts for a beat. Fix direction: keep the
last loaded category list rendered while a refetch is in flight so the
select never unmounts. Root cause is confirmed with the headless-Blink
harness first; if the cause turns out different, fix that instead.

### 6. Error handling

- Server: feed write failures are logged, never fail the sweep (same
  policy as ntfy).
- Shell loop: fetch errors skip the round without advancing the
  watermark; the loop never touches connectivity state.
- Worker: any failure ends the run quietly; `Result.retry()` only on
  network errors.

### 7. Testing

- Server: unit tests for event write on updater-found chapters (and
  not on manual refresh), `since` filtering, cap, pruning.
- UI: shell-sim E2E — stub `__TAURI__` notification invoke and
  `/updates` responses; assert notifications raised, watermark
  advanced, nothing notified on first run, tag = manga id.
- Flicker: E2E asserting the select stays present across a refresh
  bump during a stubbed download.
- Android worker: real-device smoke (install APK, force-stop, insert a
  fake updates row server-side, wait a cycle) — documented as a manual
  step; the Kotlin JSON parsing gets a plain unit test if the Gradle
  test setup is already wired, otherwise it stays manual.
