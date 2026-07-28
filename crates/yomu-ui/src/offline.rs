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
use yomu_domain::{Locations, Locator, ProgressEvent, PushEventsRequest, merge_position};

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
    publication_id: Uuid,
    server: Option<Locator>,
    now_events: &[ProgressEvent],
) -> Option<Locator> {
    let local = merge_position(
        now_events
            .iter()
            .filter(|e| e.publication_id == publication_id),
    );
    match (server, local) {
        (Some(server), Some(local)) if local.at > server.at => Some(Locator {
            unit_id: local.unit_id,
            locations: Locations::Page { page: local.page },
            at: local.at,
        }),
        (None, Some(local)) => Some(Locator {
            unit_id: local.unit_id,
            locations: Locations::Page { page: local.page },
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
    let Some(window) = web_sys::window() else {
        return false;
    };
    let sw = window.navigator().service_worker();
    // `navigator.serviceWorker` is undefined in an insecure context (http on a
    // non-localhost host); reading `.controller` on it throws rather than
    // returning null, which would abort the page render. Same guard as the
    // registration in yomu-web/src/main.rs.
    if sw.is_undefined() {
        return false;
    }
    sw.controller().is_some()
}

/// Save a chapter to this device page by page, calling `on_page(done,
/// total)` after each — the caller draws the progress. In the shell,
/// pages land in the app's data directory (`.partial-` staging, renamed
/// whole at the end); in the browser the service worker's runtime caching
/// stores each fetched response, refused when no worker controls the page
/// (the fetches would succeed but cache nothing, and the chapter would be
/// marked "on device" while it isn't).
/// Result of a device save: how many pages, or that the caller cancelled.
pub enum SaveOutcome {
    Done(u32),
    Cancelled,
}

pub async fn save_chapter_with_progress(
    client: &yomu_client::YomuClient,
    chapter_id: Uuid,
    on_page: impl Fn(u32, u32),
    should_cancel: impl Fn() -> bool,
) -> Result<SaveOutcome, String> {
    let shell = shell_available();
    if !shell && !service_worker_active() {
        return Err(
            "offline cache unavailable (no service worker; first visit or unsupported browser)"
                .into(),
        );
    }
    if should_cancel() {
        return Ok(SaveOutcome::Cancelled);
    }
    let meta = client
        .unit_pages(chapter_id)
        .await
        .map_err(|e| e.to_string())?;
    let total = meta.page_count;
    on_page(0, total);
    if shell {
        shell_chapter_command("device_begin_chapter", chapter_id, None).await?;
    }
    for n in 0..total {
        if should_cancel() {
            // Drop the partial staging dir so a later save starts clean.
            if shell {
                let _ = shell_delete_chapter(chapter_id).await;
            }
            return Ok(SaveOutcome::Cancelled);
        }
        if shell {
            let args = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&args, &"base".into(), &client.base().to_string().into());
            let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
            let _ = js_sys::Reflect::set(&args, &"page".into(), &(n as f64).into());
            shell_invoke("device_save_page", args)
                .await
                .map_err(|e| format!("page {n}: {e}"))?;
        } else {
            client
                .fetch_page(chapter_id, n)
                .await
                .map_err(|e| format!("page {n}: {e}"))?;
        }
        on_page(n + 1, total);
    }
    if shell {
        shell_chapter_command("device_finish_chapter", chapter_id, None).await?;
    }
    Ok(SaveOutcome::Done(total))
}

async fn shell_chapter_command(
    command: &str,
    chapter_id: Uuid,
    extra: Option<(&str, leptos::wasm_bindgen::JsValue)>,
) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
    if let Some((key, value)) = extra {
        let _ = js_sys::Reflect::set(&args, &key.into(), &value);
    }
    shell_invoke(command, args).await.map(|_| ())
}

/// A chapter stored on this device.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DeviceMark {
    /// Owning manga, so "on this device" can group by title. Nil for marks
    /// written before this field existed.
    #[serde(default = "Uuid::nil")]
    pub manga: Uuid,
    pub pages: u32,
    /// The chapter's number when it was saved. The cheap identity: a source
    /// that re-keys its URLs changes every unit id, but a chapter's number
    /// still names it without hashing a single page. Absent on marks
    /// written before this field existed, and whenever the publication
    /// wasn't cached at save time.
    #[serde(default)]
    pub number: Option<f64>,
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
                            number: None,
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

/// The number recorded with a stored chapter, as [`mark_device_chapter`]
/// wrote it — so an in-memory mark can carry what storage carries.
pub fn device_chapter_number(id: Uuid) -> Option<f64> {
    device_chapters().get(&id).and_then(|m| m.number)
}

pub fn mark_device_chapter(manga_id: Uuid, id: Uuid, page_count: u32) {
    let mut chapters = device_chapters();
    // The number comes from the publication view this device already
    // cached — the one the save was started from — so recording it costs
    // no request. An older mark's number is kept when that lookup misses.
    let number =
        unit_number_hint(manga_id, id).or_else(|| chapters.get(&id).and_then(|m| m.number));
    chapters.insert(
        id,
        DeviceMark {
            manga: manga_id,
            pages: page_count,
            number,
        },
    );
    write_json(DEVICE_KEY, &chapters);
}

fn unit_number_hint(manga_id: Uuid, id: Uuid) -> Option<f64> {
    cache_get::<yomu_domain::PublicationDetailResponse>(&format!("manga:{manga_id}"))?
        .units
        .into_iter()
        .find(|unit| unit.id == id)?
        .number
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

pub(crate) async fn shell_invoke(
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

// ---- device covers (shell) ----

/// URL serving the device-saved cover of a manga inside the shell. The
/// protocol 404s when no copy is stored — display falls back per-image
/// (`onerror`), so there is no bookkeeping to drift from the files.
pub fn shell_cover_url(manga_id: Uuid) -> Option<String> {
    let window = web_sys::window()?;
    let base = js_sys::Reflect::get(&window, &"YOMU_DEVICE_BASE".into())
        .ok()?
        .as_string()?;
    Some(format!("{base}cover/{manga_id}"))
}

/// Ask the shell to store a manga's cover, so the library keeps its covers
/// offline (no service worker there). The shell short-circuits covers it
/// already has, so submitting a whole library is cheap.
pub async fn shell_save_cover(
    client: &yomu_client::YomuClient,
    manga_id: Uuid,
) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"base".into(), &client.base().to_string().into());
    let _ = js_sys::Reflect::set(&args, &"manga".into(), &manga_id.to_string().into());
    shell_invoke("device_save_cover", args).await?;
    Ok(())
}

/// Delete a device-saved chapter from the shell's storage.
pub async fn shell_delete_chapter(chapter_id: Uuid) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
    shell_invoke("device_delete_chapter", args).await?;
    Ok(())
}

// ---- recovering orphaned device downloads ----
//
// A device names each stored chapter after its unit id. When a source
// changes its URLs the server re-keys every unit, and the files on this
// device — which never moved — stop matching anything the app asks for.
// No mapping survives the re-key, so the only thing left that can still
// recognise a directory is what is inside it: the page count and the hash
// of its first and last page, which the server can compute for the same
// chapter.

/// A stored chapter described by its content rather than by the name that
/// went stale. Mirrors the shell's `device_chapter_fingerprint` answer —
/// the field names below are the JS keys that command returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFingerprint {
    pub page_count: u32,
    pub sha256_first: String,
    pub sha256_last: String,
}

/// What can be done about a set of orphaned directories: the renames that
/// are certain, and how many were left alone because more than one current
/// chapter fits, or none does.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecoveryPlan {
    /// `(stale id, current id)`, in input order.
    pub renames: Vec<(Uuid, Uuid)>,
    pub ambiguous: usize,
    pub unmatched: usize,
}

/// Match stored directories against the server's fingerprints for the same
/// publication.
///
/// A rename is only proposed when the match is unique in **both**
/// directions: exactly one current chapter has that content, and no other
/// *orphaned* directory claims it. Two chapters can genuinely share an end
/// page (a cover, a "read on the site" splash), and renaming a directory
/// onto the wrong id would leave the wrong pages under a correct-looking
/// name with nothing to trigger a re-download. So anything less than
/// certain is left exactly where it is and counted.
///
/// `stored` is every directory the device holds. The server lists *all* of
/// this publication's downloaded units, including ones this device already
/// has correctly named, so a current chapter whose directory is already
/// here is not a target: it is a download that arrived by the normal route,
/// and moving anything onto it would mean destroying it. Those are dropped
/// from the candidates before uniqueness is judged, so an orphan whose only
/// match is such a chapter reads as unmatched rather than as a rename the
/// shell would (today) refuse.
pub fn plan_recovery(
    local: &[(Uuid, DeviceFingerprint)],
    server: &[yomu_domain::UnitFingerprint],
    stored: &std::collections::BTreeSet<String>,
) -> RecoveryPlan {
    let mut plan = RecoveryPlan::default();
    let mut candidates: Vec<(Uuid, Uuid)> = Vec::new();
    for (stale, fingerprint) in local {
        let mut hits = server.iter().filter(|unit| {
            unit.page_count == fingerprint.page_count
                && unit.page0_sha256 == fingerprint.sha256_first
                && unit.page_last_sha256 == fingerprint.sha256_last
                && !stored.contains(&unit.unit_id.to_string())
        });
        match (hits.next(), hits.next()) {
            (Some(hit), None) => candidates.push((*stale, hit.unit_id)),
            (Some(_), Some(_)) => plan.ambiguous += 1,
            _ => plan.unmatched += 1,
        }
    }
    for (stale, target) in &candidates {
        if candidates
            .iter()
            .filter(|(_, other)| other == target)
            .count()
            > 1
        {
            plan.ambiguous += 1;
        } else {
            plan.renames.push((*stale, *target));
        }
    }
    plan
}

/// Directory names the shell actually holds (unit ids, staging dirs
/// omitted) — the ground truth a stale mark is checked against.
pub async fn shell_list_chapters() -> Result<Vec<String>, String> {
    let value = shell_invoke("device_list_chapters", js_sys::Object::new()).await?;
    let array = js_sys::Array::from(&value);
    Ok(array.iter().filter_map(|item| item.as_string()).collect())
}

/// Describe one stored chapter by its content.
pub async fn shell_chapter_fingerprint(chapter_id: Uuid) -> Result<DeviceFingerprint, String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"chapter".into(), &chapter_id.to_string().into());
    let value = shell_invoke("device_chapter_fingerprint", args).await?;
    let page_count = js_sys::Reflect::get(&value, &"page_count".into())
        .ok()
        .and_then(|v| v.as_f64())
        .ok_or("fingerprint without a page count")?;
    let sha256_first = js_sys::Reflect::get(&value, &"sha256_first".into())
        .ok()
        .and_then(|v| v.as_string())
        .ok_or("fingerprint without a first-page hash")?;
    let sha256_last = js_sys::Reflect::get(&value, &"sha256_last".into())
        .ok()
        .and_then(|v| v.as_string())
        .ok_or("fingerprint without a last-page hash")?;
    Ok(DeviceFingerprint {
        page_count: page_count as u32,
        sha256_first,
        sha256_last,
    })
}

/// Re-key a stored chapter's directory. Errors (notably: the device
/// already holds a chapter under the new id) are the caller's to count,
/// not to recover from — the files stay where they are either way.
pub async fn shell_rename_chapter(from: Uuid, to: Uuid) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"from".into(), &from.to_string().into());
    let _ = js_sys::Reflect::set(&args, &"to".into(), &to.to_string().into());
    shell_invoke("device_rename_chapter", args)
        .await
        .map(|_| ())
}

/// Move a mark from a stale unit id to the current one, keeping its page
/// count and taking `number` from the chapter it now names. A no-op when
/// nothing is marked under `from`, so a second recovery run finds nothing
/// to do.
///
/// The number is refreshed rather than carried: these marks are exactly the
/// ones written before the number was recorded (`None`), and the current
/// unit's number is already in hand at the call site. It is the cheap
/// identity a future re-key would want to fall back on, so this is the
/// moment to fill it in. A current unit without a number leaves whatever
/// the mark had.
pub fn rekey_device_mark(from: Uuid, to: Uuid, number: Option<f64>) {
    let mut marks = device_chapters();
    if let Some(mut mark) = marks.remove(&from) {
        mark.number = number.or(mark.number);
        marks.insert(to, mark);
        write_json(DEVICE_KEY, &marks);
    }
}

/// A mark stranded by a run interrupted between the two writes a recovery
/// makes: the directory is renamed, then the mark is re-keyed. Killed in
/// between (the OS reclaims the app, the phone sleeps) the device is left
/// with the files under the *current* id and the mark still under the stale
/// one — and the stale mark is no longer repairable by content, because the
/// directory it names is gone. The chapter then reads as not saved while
/// its pages sit right there, and nothing in a later run would notice.
///
/// So each run reconciles first. `stale` is this publication's marks that
/// name neither a current chapter nor a directory on disk; `landed` is its
/// current chapters that have a directory but no mark. A pair is only
/// re-keyed when the page counts match one-to-one, for the same reason a
/// rename is: an unrelated phantom mark (files deleted behind the app's
/// back) must not be able to attach itself to an unrelated directory.
/// Returns `(stale id, current id)` pairs.
pub fn plan_mark_repair(stale: &[(Uuid, u32)], landed: &[(Uuid, u32)]) -> Vec<(Uuid, Uuid)> {
    let mut pairs = Vec::new();
    for (from, pages) in stale {
        let mut hits = landed.iter().filter(|(_, other)| other == pages);
        let (Some((to, _)), None) = (hits.next(), hits.next()) else {
            continue;
        };
        // The other direction: two stranded marks of the same length are two
        // guesses about one directory, not one repair.
        if stale.iter().filter(|(_, other)| other == pages).count() == 1 {
            pairs.push((*from, *to));
        }
    }
    pairs
}

/// Drop a chapter's "on this device" mark (after deleting its files).
const PULL_QUEUE_KEY: &str = "yomu-pull-queue";

/// The persisted device-pull queue ("download both"), oldest-first.
pub fn load_pull_queue() -> Vec<crate::PullItem> {
    read_json(PULL_QUEUE_KEY)
}

pub fn save_pull_queue(items: &[crate::PullItem]) {
    write_json(PULL_QUEUE_KEY, &items.to_vec());
}

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
    if !read.is_empty() && client.mark_units(&read, true).await.is_ok() {
        flushed.extend(read);
    }
    if !unread.is_empty() && client.mark_units(&unread, false).await.is_ok() {
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

/// Connectivity-aware last-known-good read; the one data path every page
/// resource goes through. Online: fetch, cache the result under `key`,
/// fall back to the cached copy on failure — and record the failure by
/// flipping the app [`Connectivity`] to `Offline`, so the *first* failed
/// request is the last one that touches the network. Not online: serve the
/// cached copy immediately without fetching; only when nothing is cached
/// does the fetch still run (it fails fast now, and the server may be
/// back). The bool is "came from the cache" — used to flag stale views.
///
/// Callers' resource closures should read the connectivity signal in their
/// tracked (sync) part, so a successful badge retry refetches every open
/// view.
pub async fn cached<T, E, Fut>(
    conn: leptos::prelude::RwSignal<crate::Connectivity>,
    key: &str,
    fetch: impl FnOnce() -> Fut,
) -> std::result::Result<(T, bool), E>
where
    Fut: std::future::Future<Output = std::result::Result<T, E>>,
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    use crate::Connectivity;
    use leptos::prelude::{GetUntracked, Set};
    if conn.get_untracked() != Connectivity::Online
        && let Some(value) = cache_get(key)
    {
        return Ok((value, true));
    }
    match fetch().await {
        // NB: a success does NOT promote the app to Online. On the web a
        // service worker answers cached API reads while the server is
        // unreachable, so per-request success is not evidence of
        // connectivity — treating it as such oscillates against real
        // failures (fetch loop). Only the health probe (boot gate, badge
        // retry, browser `online` event) sets Online; the probe is
        // network-only in the service worker for the same reason.
        Ok(value) => {
            cache_put(key, &value);
            Ok((value, false))
        }
        Err(err) => {
            if should_downgrade(conn.get_untracked(), document_hidden()) {
                conn.set(Connectivity::Offline);
            }
            cache_get(key).map(|value| (value, true)).ok_or(err)
        }
    }
}

/// Whether a failed request should flip the whole app to `Offline`.
///
/// Only a downgrade, and only from `Online`: while a probe is `Checking`,
/// the probe's own verdict is about to land. And never while the page is
/// hidden — a backgrounded app (screen off, app switcher, a phone that
/// dozed) routinely loses in-flight requests, and on Android the WebView
/// can freeze a fetch mid-flight. A failure nobody was watching says
/// nothing about the server, but the resulting `Offline` outlives the
/// background: it stops the pull driver (see `pull::start`) and puts every
/// read in cache-first mode until something probes again.
pub(crate) fn should_downgrade(conn: crate::Connectivity, hidden: bool) -> bool {
    conn == crate::Connectivity::Online && !hidden
}

/// Whether returning to the app should re-probe the server: anything but a
/// live `Online` deserves a fresh look, including a `Checking` left behind
/// by a probe that was frozen before it could land.
pub(crate) fn should_probe_on_resume(conn: crate::Connectivity) -> bool {
    conn != crate::Connectivity::Online
}

/// `document.hidden`: false when the document object isn't reachable, so a
/// non-browser context degrades to the old always-downgrade behaviour.
pub(crate) fn document_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .map(|d| d.hidden())
        .unwrap_or(false)
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

const LIBRARY_KIND_KEY: &str = "yomu-library-kind";

/// The library kind this device last viewed; restored on relaunch so a
/// phone reopens straight into Comics.
pub fn library_kind() -> yomu_domain::Kind {
    match storage()
        .and_then(|s| s.get_item(LIBRARY_KIND_KEY).ok().flatten())
        .as_deref()
    {
        Some("novels") => yomu_domain::Kind::Novels,
        Some("pdf") => yomu_domain::Kind::Pdf,
        _ => yomu_domain::Kind::Comics,
    }
}

pub fn set_library_kind(kind: yomu_domain::Kind) {
    let key = match kind {
        yomu_domain::Kind::Comics => "comics",
        yomu_domain::Kind::Novels => "novels",
        yomu_domain::Kind::Pdf => "pdf",
    };
    if let Some(storage) = storage() {
        let _ = storage.set_item(LIBRARY_KIND_KEY, key);
    }
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

/// Learned average page height (px) of a manga's vertical strip, from
/// the last reading session: seeds the strip's placeholders so opening
/// geometry is realistic before any image loads.
pub fn page_height_hint(manga_id: Uuid) -> Option<f64> {
    storage()?
        .get_item(&format!("yomu-page-height:{manga_id}"))
        .ok()
        .flatten()?
        .parse()
        .ok()
}

pub fn set_page_height_hint(manga_id: Uuid, height: f64) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(
            &format!("yomu-page-height:{manga_id}"),
            &format!("{height:.0}"),
        );
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

#[cfg(test)]
mod tests {
    use super::{
        DeviceFingerprint, plan_mark_repair, plan_recovery, should_downgrade,
        should_probe_on_resume,
    };
    use crate::Connectivity;
    use std::collections::BTreeSet;
    use uuid::Uuid;
    use yomu_domain::UnitFingerprint;

    fn id(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn local(n: u8, pages: u32, first: &str, last: &str) -> (Uuid, DeviceFingerprint) {
        (
            id(n),
            DeviceFingerprint {
                page_count: pages,
                sha256_first: first.into(),
                sha256_last: last.into(),
            },
        )
    }

    fn server(n: u8, pages: u32, first: &str, last: &str) -> UnitFingerprint {
        UnitFingerprint {
            unit_id: id(n),
            page_count: pages,
            page0_sha256: first.into(),
            page_last_sha256: last.into(),
        }
    }

    /// The directories on the device, as `plan_recovery` takes them.
    fn stored(ids: &[u8]) -> BTreeSet<String> {
        ids.iter().map(|n| id(*n).to_string()).collect()
    }

    /// The case this exists for: the directory is still on the phone under
    /// the id the source's re-key threw away, and exactly one current
    /// chapter has those bytes.
    #[test]
    fn one_current_chapter_with_the_same_content_is_a_rename() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 18, "aa", "zz"), server(8, 12, "bb", "yy")],
            &stored(&[1]),
        );
        assert_eq!(plan.renames, vec![(id(1), id(9))]);
        assert_eq!((plan.ambiguous, plan.unmatched), (0, 0));
    }

    /// The server lists every downloaded unit of the publication, including
    /// the ones this device already holds correctly named. Such a chapter is
    /// not a target: its directory is a good download, and proposing a
    /// rename onto it is proposing to destroy it. Nothing but the shell's
    /// own refusal stood between this and that.
    #[test]
    fn a_chapter_already_stored_on_this_device_is_not_a_target() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 18, "aa", "zz")],
            &stored(&[1, 9]),
        );
        assert!(
            plan.renames.is_empty(),
            "id 9 is already on disk; the orphan has nowhere to go"
        );
        assert_eq!((plan.ambiguous, plan.unmatched), (0, 1));
    }

    /// A first page is shared often enough — a credits page, a "read on the
    /// site" splash — that it cannot carry the identity alone. With the two
    /// colliding chapters both present the uniqueness guard catches it; with
    /// only one of them left it never fires, so the last page has to be part
    /// of the comparison.
    #[test]
    fn the_last_page_has_to_match_too() {
        let plan = plan_recovery(
            &[local(1, 18, "splash", "zz")],
            &[server(9, 18, "splash", "yy")],
            &stored(&[1]),
        );
        assert!(plan.renames.is_empty());
        assert_eq!((plan.ambiguous, plan.unmatched), (0, 1));
    }

    /// Two chapters can share a first page — a cover, a "read on the site"
    /// splash. Renaming onto the wrong one would leave wrong pages under a
    /// right-looking name forever, so neither is touched.
    #[test]
    fn two_current_chapters_with_the_same_content_are_left_alone() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 18, "aa", "zz"), server(8, 18, "aa", "zz")],
            &stored(&[1]),
        );
        assert!(plan.renames.is_empty());
        assert_eq!((plan.ambiguous, plan.unmatched), (1, 0));
    }

    /// Ambiguity runs the other way too: two stored directories that both
    /// fit one current chapter are two guesses, not one match.
    #[test]
    fn two_directories_claiming_one_chapter_are_left_alone() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz"), local(2, 18, "aa", "zz")],
            &[server(9, 18, "aa", "zz")],
            &stored(&[1, 2]),
        );
        assert!(plan.renames.is_empty());
        assert_eq!((plan.ambiguous, plan.unmatched), (2, 0));
    }

    /// Nothing on the server has that content — the chapter was dropped,
    /// or the server's own copy is gone. Counted, never guessed at.
    #[test]
    fn no_current_chapter_with_that_content_is_left_alone() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 18, "bb", "yy")],
            &stored(&[1]),
        );
        assert!(plan.renames.is_empty());
        assert_eq!((plan.ambiguous, plan.unmatched), (0, 1));
    }

    /// The page count is part of the identity: same first page, different
    /// length, is a different chapter.
    #[test]
    fn the_page_count_has_to_match_too() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 19, "aa", "zz")],
            &stored(&[1]),
        );
        assert!(plan.renames.is_empty());
        assert_eq!(plan.unmatched, 1);
    }

    /// A second run has nothing left to do: the marks it repaired are no
    /// longer orphans, so nothing reaches the matcher.
    #[test]
    fn a_second_run_finds_nothing() {
        let plan = plan_recovery(&[], &[server(9, 18, "aa", "zz")], &stored(&[9]));
        assert_eq!(plan, super::RecoveryPlan::default());
    }

    /// The whole interrupted-run story, at the level of the pure logic.
    ///
    /// Run one matches the orphan and the directory is renamed — then the
    /// app is killed before the mark follows. Run two therefore sees the
    /// files under the current id with nothing marking them, and a mark on
    /// a stale id whose directory no longer exists: unmatchable by content,
    /// and left forever without this repair. The pairing puts it back.
    #[test]
    fn a_run_interrupted_between_the_rename_and_the_mark_is_repaired_next_time() {
        let plan = plan_recovery(
            &[local(1, 18, "aa", "zz")],
            &[server(9, 18, "aa", "zz")],
            &stored(&[1]),
        );
        assert_eq!(plan.renames, vec![(id(1), id(9))]);

        // The rename landed, the re-key did not. On disk: 9. Marked: 1.
        let after = stored(&[9]);
        assert!(
            !after.contains(&id(1).to_string()),
            "the stale mark has no directory left to fingerprint"
        );
        let orphan_pass = plan_recovery(&[], &[server(9, 18, "aa", "zz")], &after);
        assert_eq!(
            orphan_pass,
            super::RecoveryPlan::default(),
            "the orphan pass cannot see it: there is nothing to hash"
        );

        // The reconciliation can: one stranded mark, one unmarked directory
        // belonging to a current chapter, same length.
        assert_eq!(
            plan_mark_repair(&[(id(1), 18)], &[(id(9), 18)]),
            vec![(id(1), id(9))]
        );
    }

    /// Two stranded marks of the same length are two guesses about one
    /// directory. A phantom mark (files deleted behind the app's back) must
    /// not be able to attach itself to somebody else's chapter.
    #[test]
    fn an_ambiguous_stranded_mark_is_left_alone() {
        assert!(plan_mark_repair(&[(id(1), 18), (id(2), 18)], &[(id(9), 18)]).is_empty());
        assert!(plan_mark_repair(&[(id(1), 18)], &[(id(9), 18), (id(8), 18)]).is_empty());
        // A different length is a different chapter: no repair, no guess.
        assert!(plan_mark_repair(&[(id(1), 18)], &[(id(9), 19)]).is_empty());
    }

    /// Nothing stranded, nothing unmarked: the ordinary run does no repair.
    #[test]
    fn a_device_with_nothing_stranded_is_left_untouched() {
        assert!(plan_mark_repair(&[], &[(id(9), 18)]).is_empty());
        assert!(plan_mark_repair(&[(id(1), 18)], &[]).is_empty());
    }

    #[test]
    fn a_failure_while_visible_takes_the_app_offline() {
        assert!(should_downgrade(Connectivity::Online, false));
    }

    /// The reported bug: local saves running, app backgrounded, one poll
    /// fails against a VPN that is perfectly fine — and the app came back
    /// "offline", stalling the pull queue.
    #[test]
    fn a_failure_while_hidden_does_not() {
        assert!(!should_downgrade(Connectivity::Online, true));
    }

    #[test]
    fn only_online_can_be_downgraded() {
        for hidden in [false, true] {
            assert!(!should_downgrade(Connectivity::Offline, hidden));
            // A probe is mid-flight; its verdict wins, not a racing read's.
            assert!(!should_downgrade(Connectivity::Checking, hidden));
        }
    }

    #[test]
    fn coming_back_probes_unless_already_online() {
        assert!(should_probe_on_resume(Connectivity::Offline));
        // A `Checking` that outlived its probe (frozen webview) must not
        // wedge the app: resuming retries it.
        assert!(should_probe_on_resume(Connectivity::Checking));
        assert!(!should_probe_on_resume(Connectivity::Online));
    }
}
