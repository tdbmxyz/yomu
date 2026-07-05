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
    if let Some(crypto) = web_sys::window().and_then(|w| w.crypto().ok()) {
        let _ = crypto.get_random_values_with_u8_array(&mut bytes);
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

/// Push the outbox to the server; on success the outbox is cleared (events
/// are idempotent by id, so a crash between push and clear is harmless).
pub async fn flush_outbox(client: &yomu_client::YomuClient) {
    let events = outbox();
    if events.is_empty() {
        return;
    }
    let count = events.len();
    match client.push_events(&PushEventsRequest { events }).await {
        Ok(()) => {
            write_json(OUTBOX_KEY, &Vec::<ProgressEvent>::new());
            leptos::logging::log!("synced {count} offline progress event(s)");
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

/// Fetch chapter metadata and every page image once. The service worker's
/// runtime caching stores each response, after which the chapter (and its
/// metadata) is readable with the server unreachable.
pub async fn prefetch_chapter(
    client: &yomu_client::YomuClient,
    chapter_id: Uuid,
) -> Result<(), String> {
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
    Ok(())
}

pub fn device_chapters() -> Vec<Uuid> {
    read_json(DEVICE_KEY)
}

pub fn mark_device_chapter(id: Uuid) {
    let mut ids = device_chapters();
    if !ids.contains(&id) {
        ids.push(id);
        write_json(DEVICE_KEY, &ids);
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
