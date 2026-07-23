//! Shell update notifications: poll `GET /updates` while the app is
//! alive and raise OS notifications through the Tauri notification
//! plugin. Watermark ("everything up to here was announced") lives in
//! localStorage — except on Android, where the `YomuAndroid` bridge owns
//! it (SharedPreferences) so the WorkManager background poller and this
//! loop share one cursor and never double-announce.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::{JsCast, JsValue};
use yomu_client::YomuClient;
use yomu_domain::UpdateEvent;

use crate::Connectivity;

const WATERMARK_KEY: &str = "yomu-updates-seen";
const POLL_MINUTES: u32 = 15;

/// Start the polling loop; call once from `App` when running inside a
/// shell. Polls immediately once connectivity reaches Online, then every
/// 15 minutes.
pub fn start(conn: RwSignal<Connectivity>, client: YomuClient) {
    // Tell the Android background worker where the server is (and keep
    // it scheduled); a no-op everywhere else.
    bridge_call("configureUpdates", &[client.base().as_str().into()]);

    let booted = StoredValue::new(false);
    let poll_client = client.clone();
    Effect::new(move |_| {
        if conn.get() != Connectivity::Online || booted.get_value() {
            return;
        }
        booted.set_value(true);
        let client = poll_client.clone();
        spawn_local(async move { poll(&client).await });
    });

    let closure = leptos::wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
        if conn.get_untracked() != Connectivity::Online {
            return;
        }
        let client = client.clone();
        spawn_local(async move { poll(&client).await });
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            (POLL_MINUTES * 60 * 1000) as i32,
        );
    }
    closure.forget(); // lives for the whole app
}

async fn poll(client: &YomuClient) {
    let Some(seen) = watermark() else {
        // First run: nothing was ever announced — start announcing from
        // now rather than dumping the feed's backlog.
        set_watermark(&now_rfc3339());
        return;
    };
    let Ok(response) = client.updates(&seen).await else {
        return; // next tick retries; watermark untouched
    };
    if response.updates.is_empty() {
        return;
    }
    if !ensure_permission().await {
        // Denied: stay quiet but advance — a later grant should not
        // replay weeks of backlog.
        advance_watermark(&response.updates);
        return;
    }
    for event in &response.updates {
        notify(event).await;
    }
    advance_watermark(&response.updates);
}

fn advance_watermark(updates: &[UpdateEvent]) {
    if let Some(newest) = updates.iter().map(|u| u.created_at).max() {
        set_watermark(&newest.to_rfc3339());
    }
}

/// Body matching the server's ntfy push for the same event.
fn message(event: &UpdateEvent) -> String {
    match event.unit_count {
        1 => event.first_title.clone(),
        n => format!(
            "{n} new chapters — {} … {}",
            event.first_title, event.last_title
        ),
    }
}

async fn notify(event: &UpdateEvent) {
    let options = js_sys::Object::new();
    // Stable per-manga id: repeated finds replace instead of stacking.
    // Fold the whole UUID — the first bytes alone are a v7 timestamp,
    // shared by manga added around the same time.
    let id = event
        .publication_id
        .as_bytes()
        .chunks(4)
        .map(|c| i32::from_le_bytes(c.try_into().expect("4 bytes")))
        .fold(0i32, |acc, w| acc ^ w)
        & 0x7fff_ffff;
    let _ = js_sys::Reflect::set(&options, &"id".into(), &(id as f64).into());
    let _ = js_sys::Reflect::set(
        &options,
        &"title".into(),
        &event.publication_title.as_str().into(),
    );
    let _ = js_sys::Reflect::set(&options, &"body".into(), &message(event).as_str().into());
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"options".into(), &options);
    if let Err(err) = crate::offline::shell_invoke("plugin:notification|notify", args).await {
        leptos::logging::warn!("notification failed: {err}");
    }
}

/// Plugin permission dance (Android 13+ prompts; desktop is a yes).
async fn ensure_permission() -> bool {
    let granted = crate::offline::shell_invoke(
        "plugin:notification|is_permission_granted",
        js_sys::Object::new(),
    )
    .await;
    if matches!(&granted, Ok(v) if v.as_bool() == Some(true)) {
        return true;
    }
    matches!(
        crate::offline::shell_invoke(
            "plugin:notification|request_permission",
            js_sys::Object::new(),
        )
        .await,
        Ok(v) if v.as_string().as_deref() == Some("granted")
    )
}

fn now_rfc3339() -> String {
    js_sys::Date::new_0().to_iso_string().into()
}

fn watermark() -> Option<String> {
    if let Some(v) = bridge_call("updatesWatermark", &[]) {
        let s = v.as_string().unwrap_or_default();
        return (!s.is_empty()).then_some(s);
    }
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(WATERMARK_KEY)
        .ok()?
}

fn set_watermark(ts: &str) {
    if bridge_call("setUpdatesWatermark", &[ts.into()]).is_some() {
        return;
    }
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(WATERMARK_KEY, ts);
    }
}

/// Call a `YomuAndroid` bridge method if the bridge (and the method)
/// exists; `None` means "not on Android" and the caller falls back.
fn bridge_call(name: &str, args: &[JsValue]) -> Option<JsValue> {
    let window = web_sys::window()?;
    let bridge = js_sys::Reflect::get(&window, &"YomuAndroid".into()).ok()?;
    let method: js_sys::Function = js_sys::Reflect::get(&bridge, &name.into())
        .ok()?
        .dyn_into()
        .ok()?;
    match args {
        [] => method.call0(&bridge).ok(),
        [a] => method.call1(&bridge, a).ok(),
        _ => None,
    }
}
