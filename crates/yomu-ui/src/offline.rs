//! Client-side offline support.
//!
//! Three pieces, all backed by localStorage (survives restarts, readable
//! synchronously) while page images live in the service worker's cache:
//!
//! - **outbox**: progress events written while the server was unreachable,
//!   as real `ProgressEvent`s (client UUIDv7 + client timestamp). Flushed
//!   with the idempotent batch endpoint whenever we're back online — the
//!   server-side journal merge (same rule as `merge_position`) resolves any
//!   divergence with what was read on other devices meanwhile.
//! - **device marks**: which chapters were prefetched into the browser
//!   cache ("on this device"), so the UI can show it without querying the
//!   Cache API.
//! - **reader prefs**: paged/vertical mode per manga.

use uuid::Uuid;
use yomu_domain::{Position, ProgressEvent, PushEventsRequest, merge_position};

const OUTBOX_KEY: &str = "yomu-outbox";
const DEVICE_KEY: &str = "yomu-device-chapters";
const MODE_KEY_PREFIX: &str = "yomu-reader-mode:";
const FIT_KEY_PREFIX: &str = "yomu-reader-fit:";
const DIR_KEY_PREFIX: &str = "yomu-reader-dir:";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn read_json<T: serde::de::DeserializeOwned + Default>(key: &str) -> T {
    storage()
        .and_then(|s| s.get_item(key).ok().flatten())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_json<T: serde::Serialize>(key: &str, value: &T) {
    if let (Some(storage), Ok(raw)) = (storage(), serde_json::to_string(value)) {
        let _ = storage.set_item(key, &raw);
    }
}

/// UUIDv7 built from the browser clock + Web Crypto, so offline events sort
/// correctly into the journal (no getrandom dependency on wasm).
pub fn uuid_v7_js() -> Uuid {
    let millis = js_sys::Date::now() as u64;
    let mut bytes = [0u8; 16];
    let filled = web_sys::window()
        .and_then(|w| w.crypto().ok())
        .and_then(|crypto| crypto.get_random_values_with_u8_array(&mut bytes).ok())
        .is_some();
    if !filled {
        // No Web Crypto (exotic webview): Math.random is plenty to keep two
        // same-millisecond events from colliding on id.
        for byte in bytes.iter_mut() {
            *byte = (js_sys::Math::random() * 256.0) as u8;
        }
    }
    bytes[0] = (millis >> 40) as u8;
    bytes[1] = (millis >> 32) as u8;
    bytes[2] = (millis >> 24) as u8;
    bytes[3] = (millis >> 16) as u8;
    bytes[4] = (millis >> 8) as u8;
    bytes[5] = millis as u8;
    bytes[6] = (bytes[6] & 0x0f) | 0x70; // version 7
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC variant
    Uuid::from_bytes(bytes)
}

// ---- outbox ----

pub fn outbox() -> Vec<ProgressEvent> {
    read_json(OUTBOX_KEY)
}

pub fn outbox_push(event: ProgressEvent) {
    let mut events = outbox();
    events.push(event);
    write_json(OUTBOX_KEY, &events);
}

/// Push the outbox to the server. On success only the *pushed* events are
/// removed — new events appended while the request was in flight survive
/// (events are idempotent by id, so a crash between push and remove is
/// harmless). A 4xx answer means the server understood and refused: those
/// events can never succeed, so they are dropped too rather than poisoning
/// every future flush.
pub async fn flush_outbox(client: &yomu_client::YomuClient) {
    let events = outbox();
    if events.is_empty() {
        return;
    }
    let pushed: Vec<Uuid> = events.iter().map(|e| e.id).collect();
    let remove_pushed = || {
        let remaining: Vec<ProgressEvent> = outbox()
            .into_iter()
            .filter(|e| !pushed.contains(&e.id))
            .collect();
        write_json(OUTBOX_KEY, &remaining);
    };
    match client.push_events(&PushEventsRequest { events }).await {
        Ok(outcome) => {
            remove_pushed();
            if outcome.skipped > 0 {
                leptos::logging::warn!(
                    "server skipped {} stale offline event(s) (manga deleted?)",
                    outcome.skipped
                );
            }
            leptos::logging::log!("synced {} offline progress event(s)", outcome.accepted);
        }
        // 401/403 are NOT poison: signing in will make the same batch
        // succeed, so those events must stay queued.
        Err(yomu_client::ClientError::Api { status, message })
            if (400..500).contains(&status) && status != 401 && status != 403 =>
        {
            remove_pushed();
            leptos::logging::warn!(
                "server rejected {} offline event(s) ({status}: {message}); dropped",
                pushed.len()
            );
        }
        Err(err) => leptos::logging::warn!("outbox flush failed (still offline?): {err}"),
    }
}

/// Best local knowledge of a manga's position: the (possibly stale) server
/// answer merged with any unsynced local events — same rule as everywhere.
pub fn effective_position(
    manga_id: Uuid,
    server: Option<Position>,
    now_events: &[ProgressEvent],
) -> Option<Position> {
    let local = merge_position(now_events.iter().filter(|e| e.manga_id == manga_id));
    match (server, local) {
        (Some(server), Some(local)) if local.at > server.at => Some(Position {
            chapter_id: local.chapter_id,
            page: local.page,
            at: local.at,
        }),
        (None, Some(local)) => Some(Position {
            chapter_id: local.chapter_id,
            page: local.page,
            at: local.at,
        }),
        (server, _) => server,
    }
}

// ---- device downloads ----

/// Whether a service worker currently controls this page — i.e. whether
/// fetches actually land in the offline cache. False on the very first
/// visit (registration pending), in webviews without SW support, etc.
pub fn service_worker_active() -> bool {
    web_sys::window()
        .map(|w| w.navigator().service_worker().controller().is_some())
        .unwrap_or(false)
}

/// Fetch chapter metadata and every page image once. The service worker's
/// runtime caching stores each response, after which the chapter (and its
/// metadata) is readable with the server unreachable. Refuses to run when
/// no service worker controls the page: the fetches would succeed but cache
/// nothing, and the chapter would be marked "on device" while it isn't.
pub async fn prefetch_chapter(
    client: &yomu_client::YomuClient,
    chapter_id: Uuid,
) -> Result<u32, String> {
    if !service_worker_active() {
        return Err(
            "offline cache unavailable (no service worker; first visit or unsupported browser)"
                .into(),
        );
    }
    let meta = client
        .chapter_pages(chapter_id)
        .await
        .map_err(|e| e.to_string())?;
    for n in 0..meta.page_count {
        client
            .fetch_page(chapter_id, n)
            .await
            .map_err(|e| format!("page {n}: {e}"))?;
    }
    Ok(meta.page_count)
}

/// A chapter stored on this device.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DeviceMark {
    /// Owning manga, so "on this device" can group by title. Nil for marks
    /// written before this field existed.
    #[serde(default = "Uuid::nil")]
    pub manga: Uuid,
    pub pages: u32,
}

/// Chapters stored on this device, with their page count — enough to open
/// the reader with the server unreachable.
pub fn device_chapters() -> std::collections::BTreeMap<Uuid, DeviceMark> {
    let raw = storage().and_then(|s| s.get_item(DEVICE_KEY).ok().flatten());
    let Some(raw) = raw else {
        return Default::default();
    };
    if let Ok(map) = serde_json::from_str(&raw) {
        return map;
    }
    // pre-manga-id format: plain chapter -> page count
    serde_json::from_str::<std::collections::BTreeMap<Uuid, u32>>(&raw)
        .map(|old| {
            old.into_iter()
                .map(|(id, pages)| {
                    (
                        id,
                        DeviceMark {
                            manga: Uuid::nil(),
                            pages,
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn device_chapter_pages(id: Uuid) -> Option<u32> {
    device_chapters().get(&id).map(|m| m.pages)
}

pub fn mark_device_chapter(manga_id: Uuid, id: Uuid, page_count: u32) {
    let mut chapters = device_chapters();
    chapters.insert(
        id,
        DeviceMark {
            manga: manga_id,
            pages: page_count,
        },
    );
    write_json(DEVICE_KEY, &chapters);
}

/// Manga with device-saved chapters, and how many each has.
pub fn device_manga() -> std::collections::BTreeMap<Uuid, u32> {
    let mut out = std::collections::BTreeMap::new();
    for mark in device_chapters().values() {
        if !mark.manga.is_nil() {
            *out.entry(mark.manga).or_default() += 1;
        }
    }
    out
}

// ---- Tauri shell bridge ----
//
// In the desktop/Android shell there is no service worker; "save to
// device" goes through Tauri commands that download pages to the app's
// data directory, and the reader loads them back over the shell's
// `yomudev` custom protocol (`window.YOMU_DEVICE_BASE`, injected at
// startup). Everything here degrades to None/Err outside the shell.

fn tauri_global() -> Option<js_sys::Object> {
    use leptos::wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    js_sys::Reflect::get(&window, &"__TAURI__".into())
        .ok()?
        .dyn_into()
        .ok()
}

pub fn shell_available() -> bool {
    tauri_global().is_some()
}

/// URL serving page `n` of a device-saved chapter inside the shell.
pub fn shell_page_url(chapter_id: Uuid, n: u32) -> Option<String> {
    let window = web_sys::window()?;
    let base = js_sys::Reflect::get(&window, &"YOMU_DEVICE_BASE".into())
        .ok()?
        .as_string()?;
    Some(format!("{base}chapter/{chapter_id}/{n}"))
}

/// Android shell: hide/show the system bars while reading. The bridge is
/// installed by the Android activity as `window.YomuAndroid`; anywhere it
/// is absent (desktop shell, plain browser, an APK older than the bridge)
/// this is a no-op.
pub fn set_immersive(on: bool) {
    android_bridge("setImmersive", on);
}

/// Android shell: the reader is open — go edge-to-edge so toggling the
/// system bars overlays them over the page instead of resizing the
/// webview (which would visibly shift the reader). Same no-op rules as
/// [`set_immersive`].
pub fn set_reading(on: bool) {
    android_bridge("setReading", on);
}

fn android_bridge(name: &str, on: bool) {
    use leptos::wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(bridge) = js_sys::Reflect::get(&window, &"YomuAndroid".into()) else {
        return;
    };
    let Ok(method) = js_sys::Reflect::get(&bridge, &name.into()) else {
        return;
    };
    let Ok(method) = method.dyn_into::<js_sys::Function>() else {
        return;
    };
    let _ = method.call1(&bridge, &on.into());
}

async fn shell_invoke(
    command: &str,
    args: js_sys::Object,
) -> Result<leptos::wasm_bindgen::JsValue, String> {
    use leptos::wasm_bindgen::JsCast;
    let tauri = tauri_global().ok_or("not running inside the shell")?;
    let core = js_sys::Reflect::get(&tauri, &"core".into()).map_err(|_| "no __TAURI__.core")?;
    let invoke: js_sys::Function = js_sys::Reflect::get(&core, &"invoke".into())
        .map_err(|_| "no invoke")?
        .dyn_into()
        .map_err(|_| "invoke is not a function")?;
    let promise: js_sys::Promise = invoke
        .call2(&core, &command.into(), &args)
        .map_err(|e| format!("{e:?}"))?
        .dyn_into()
        .map_err(|_| "invoke did not return a promise")?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
}

/// Download a chapter into the shell's device storage.
pub async fn shell_save_chapter(
    client: &yomu_client::YomuClient,
    chapter_id: Uuid,
) -> Result<u32, String> {
    let meta = client
        .chapter_pages(chapter_id)
        .await
        .map_err(|e| e.to_string())?;
    let args = js_sys::Object::new();
    let set = |key: &str, value: leptos::wasm_bindgen::JsValue| {
        let _ = js_sys::Reflect::set(&args, &key.into(), &value);
    };
    set("base", client.base().to_string().into());
    set("chapter", chapter_id.to_string().into());
    set("count", (meta.page_count as f64).into());
    shell_invoke("device_save_chapter", args).await?;
    Ok(meta.page_count)
}

/// Delete a device-saved chapter from the shell's storage.
pub async fn shell_delete_chapter(chapter_id: Uuid) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
    shell_invoke("device_delete_chapter", args).await?;
    Ok(())
}

/// Drop a chapter's "on this device" mark (after deleting its files).
pub fn unmark_device_chapter(id: Uuid) {
    let mut marks = device_chapters();
    marks.remove(&id);
    write_json(DEVICE_KEY, &marks);
}

// ---- offline read marks ----

const MARKS_KEY: &str = "yomu-marks-outbox";

/// Read marks made while the server was unreachable: chapter → desired
/// state, last write wins. Flushed by [`flush_marks`].
pub fn pending_marks() -> std::collections::BTreeMap<Uuid, bool> {
    read_json(MARKS_KEY)
}

pub fn queue_marks(ids: &[Uuid], read: bool) {
    let mut marks = pending_marks();
    for id in ids {
        marks.insert(*id, read);
    }
    write_json(MARKS_KEY, &marks);
}

/// Replay queued read marks; entries survive failed flushes. The mark
/// endpoint is a set operation, so replays are idempotent.
pub async fn flush_marks(client: &yomu_client::YomuClient) {
    let marks = pending_marks();
    if marks.is_empty() {
        return;
    }
    let (read, unread): (Vec<_>, Vec<_>) = marks.iter().partition(|(_, r)| **r);
    let read: Vec<Uuid> = read.into_iter().map(|(id, _)| *id).collect();
    let unread: Vec<Uuid> = unread.into_iter().map(|(id, _)| *id).collect();
    let mut flushed: Vec<Uuid> = Vec::new();
    if !read.is_empty() && client.mark_chapters(&read, true).await.is_ok() {
        flushed.extend(read);
    }
    if !unread.is_empty() && client.mark_chapters(&unread, false).await.is_ok() {
        flushed.extend(unread);
    }
    if !flushed.is_empty() {
        let mut marks = pending_marks();
        for id in &flushed {
            marks.remove(id);
        }
        write_json(MARKS_KEY, &marks);
        leptos::logging::log!("synced {} offline read mark(s)", flushed.len());
    }
}

// ---- server-seen (offline gate) ----

const SERVERS_SEEN_KEY: &str = "yomu-servers-seen";

/// Record that a server address answered a health check. Scoped by base
/// URL so pointing the app at a new address still shows the first-run
/// connect form for *that* address if it can't be reached.
pub fn mark_server_seen(base: &str) {
    let mut seen: Vec<String> = read_json(SERVERS_SEEN_KEY);
    if !seen.iter().any(|s| s == base) {
        seen.push(base.to_string());
        write_json(SERVERS_SEEN_KEY, &seen);
    }
}

/// Whether this server address has ever answered a health check. When it
/// has, an unreachable server means "offline", not "misconfigured", so the
/// boot gate proceeds to the cached UI instead of the connect form.
pub fn server_seen(base: &str) -> bool {
    read_json::<Vec<String>>(SERVERS_SEEN_KEY)
        .iter()
        .any(|s| s == base)
}

// ---- last-known-good cache (offline browsing without a service worker) ----

const CACHE_KEY_PREFIX: &str = "yomu-cache:";

pub fn cache_put<T: serde::Serialize>(key: &str, value: &T) {
    write_json(&format!("{CACHE_KEY_PREFIX}{key}"), value);
}

pub fn cache_get<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    storage()
        .and_then(|s| {
            s.get_item(&format!("{CACHE_KEY_PREFIX}{key}"))
                .ok()
                .flatten()
        })
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Last-known-good wrapper for a fetch result: on success cache it under
/// `key` and return it; on failure fall back to the cached copy, propagating
/// the error only when there is nothing cached. Collapses the fetch → cache /
/// cache_get pattern repeated across the pages.
pub fn with_cache<T, E>(key: &str, result: std::result::Result<T, E>) -> std::result::Result<T, E>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    with_cache_flagged(key, result).map(|(value, _)| value)
}

/// Like [`with_cache`] but also reports whether the value came from the cache
/// (i.e. the server was unreachable) — used to flag stale/offline views.
pub fn with_cache_flagged<T, E>(
    key: &str,
    result: std::result::Result<T, E>,
) -> std::result::Result<(T, bool), E>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    match result {
        Ok(value) => {
            cache_put(key, &value);
            Ok((value, false))
        }
        Err(err) => cache_get(key).map(|value| (value, true)).ok_or(err),
    }
}

// ---- theme ----

const THEME_KEY: &str = "yomu-theme";

/// A skin: palette (and for some, typography) applied app-wide through the
/// `data-theme` attribute on `<html>` (see styles.css).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Charcoal + teal (the default).
    #[default]
    Charcoal,
    /// The original dark blue-grey + pink.
    Rose,
    /// Light, warm paper + deep red.
    Paper,
    /// Pure OLED black + crimson.
    Ink,
    /// Deep plum + amber.
    Plum,
    /// Terminal green-on-black, monospace.
    Phosphor,
    /// Windows Terminal scheme: near-black, primary blue (chaos default).
    Campbell,
    /// GitHub dark mode: blue-tinted greys.
    Github,
}

impl Theme {
    pub const ALL: [Theme; 8] = [
        Theme::Charcoal,
        Theme::Rose,
        Theme::Paper,
        Theme::Ink,
        Theme::Plum,
        Theme::Phosphor,
        Theme::Campbell,
        Theme::Github,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Theme::Charcoal => "charcoal",
            Theme::Rose => "rose",
            Theme::Paper => "paper",
            Theme::Ink => "ink",
            Theme::Plum => "plum",
            Theme::Phosphor => "phosphor",
            Theme::Campbell => "campbell",
            Theme::Github => "github",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Charcoal => "Charcoal",
            Theme::Rose => "Rose",
            Theme::Paper => "Paper",
            Theme::Ink => "Ink",
            Theme::Plum => "Plum",
            Theme::Phosphor => "Phosphor",
            Theme::Campbell => "Campbell",
            Theme::Github => "GitHub Dark",
        }
    }

    /// Closest yomu theme for a chaos palette id (`?chaos-theme=` from the
    /// embedding chaos app), so the two stay visually in sync.
    pub fn from_chaos(key: &str) -> Option<Theme> {
        match key {
            "campbell" => Some(Theme::Campbell),
            "github" => Some(Theme::Github),
            "midnight" => Some(Theme::Rose),
            "daylight" => Some(Theme::Paper),
            "glass" => Some(Theme::Plum),
            "terminal" => Some(Theme::Phosphor),
            _ => None,
        }
    }

    fn from_key(key: &str) -> Theme {
        Theme::ALL
            .into_iter()
            .find(|t| t.key() == key)
            .unwrap_or_default()
    }
}

pub fn theme() -> Theme {
    storage()
        .and_then(|s| s.get_item(THEME_KEY).ok().flatten())
        .map(|k| Theme::from_key(&k))
        .unwrap_or_default()
}

pub fn set_theme(theme: Theme) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(THEME_KEY, theme.key());
    }
    apply_theme(theme);
}

/// Reflect the theme onto `<html data-theme>`, where the CSS reads it.
pub fn apply_theme(theme: Theme) {
    if let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    {
        let _ = root.set_attribute("data-theme", theme.key());
    }
}

// ---- reader prefs ----

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderMode {
    #[default]
    Paged,
    Vertical,
}

pub fn reader_mode(manga_id: Uuid) -> ReaderMode {
    match storage()
        .and_then(|s| {
            s.get_item(&format!("{MODE_KEY_PREFIX}{manga_id}"))
                .ok()
                .flatten()
        })
        .as_deref()
    {
        Some("vertical") => ReaderMode::Vertical,
        _ => ReaderMode::Paged,
    }
}

pub fn set_reader_mode(manga_id: Uuid, mode: ReaderMode) {
    if let Some(storage) = storage() {
        let value = match mode {
            ReaderMode::Paged => "paged",
            ReaderMode::Vertical => "vertical",
        };
        let _ = storage.set_item(&format!("{MODE_KEY_PREFIX}{manga_id}"), value);
    }
}

/// How a page is scaled in paged mode. `Screen` shows the whole page at
/// once; `Width` and `Original` trade that for readability and scroll.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderFit {
    #[default]
    Screen,
    Width,
    Original,
}

pub fn reader_fit(manga_id: Uuid) -> ReaderFit {
    match storage()
        .and_then(|s| {
            s.get_item(&format!("{FIT_KEY_PREFIX}{manga_id}"))
                .ok()
                .flatten()
        })
        .as_deref()
    {
        Some("width") => ReaderFit::Width,
        Some("original") => ReaderFit::Original,
        _ => ReaderFit::Screen,
    }
}

pub fn set_reader_fit(manga_id: Uuid, fit: ReaderFit) {
    if let Some(storage) = storage() {
        let value = match fit {
            ReaderFit::Screen => "screen",
            ReaderFit::Width => "width",
            ReaderFit::Original => "original",
        };
        let _ = storage.set_item(&format!("{FIT_KEY_PREFIX}{manga_id}"), value);
    }
}

/// Reading direction in paged mode: which side "next page" lives on.
/// Manga read right-to-left; webtoons and western comics left-to-right.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderDirection {
    #[default]
    Ltr,
    Rtl,
}

pub fn reader_direction(manga_id: Uuid) -> ReaderDirection {
    match storage()
        .and_then(|s| {
            s.get_item(&format!("{DIR_KEY_PREFIX}{manga_id}"))
                .ok()
                .flatten()
        })
        .as_deref()
    {
        Some("rtl") => ReaderDirection::Rtl,
        _ => ReaderDirection::Ltr,
    }
}

pub fn set_reader_direction(manga_id: Uuid, direction: ReaderDirection) {
    if let Some(storage) = storage() {
        let value = match direction {
            ReaderDirection::Ltr => "ltr",
            ReaderDirection::Rtl => "rtl",
        };
        let _ = storage.set_item(&format!("{DIR_KEY_PREFIX}{manga_id}"), value);
    }
}
