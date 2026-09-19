//! Narrow boundary to browser ownership and Service Worker persistence.
use leptos::wasm_bindgen::{JsCast, JsValue};

fn call(method: &str, args: &js_sys::Array) -> Result<JsValue, String> {
    let window = web_sys::window().ok_or("no window")?;
    let context =
        js_sys::Reflect::get(&window, &"YomuOffline".into()).map_err(|e| format!("{e:?}"))?;
    let function = js_sys::Reflect::get(&context, &method.into())
        .map_err(|e| format!("{e:?}"))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| "Offline adapter unavailable; reload online")?;
    function.apply(&context, args).map_err(|e| format!("{e:?}"))
}
pub async fn invoke(method: &str, args: &js_sys::Array) -> Result<JsValue, String> {
    let value = call(method, args)?;
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&value))
        .await
        .map_err(|e| format!("{e:?}"))
}
pub async fn initialize(base: &str) -> Result<(), String> {
    invoke("init", &js_sys::Array::of1(&base.into()))
        .await
        .map(|_| ())
}
pub fn key(logical: &str) -> String {
    call("key", &js_sys::Array::of1(&logical.into()))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("yomu-quarantine:{logical}"))
}
pub fn has_legacy_work() -> bool {
    call("pendingLegacy", &js_sys::Array::new())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
pub fn active() -> bool {
    call("active", &js_sys::Array::new())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
pub fn scope() -> Option<String> {
    call("scope", &js_sys::Array::new())
        .ok()
        .and_then(|v| v.as_string())
}
pub fn matches(expected: &Option<String>) -> bool {
    active() && &scope() == expected
}
pub async fn verify() -> bool {
    invoke("verify", &js_sys::Array::new())
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
pub async fn worker(
    kind: &str,
    chapter: uuid::Uuid,
    page: Option<u32>,
    url: Option<&str>,
) -> Result<JsValue, String> {
    let message = js_sys::Object::new();
    for (key, value) in [
        ("type", kind.into()),
        ("chapter", chapter.to_string().into()),
    ] {
        js_sys::Reflect::set(&message, &key.into(), &value).map_err(|e| format!("{e:?}"))?;
    }
    if let Some(page) = page {
        let key = if kind == "save-page" { "page" } else { "pages" };
        js_sys::Reflect::set(&message, &key.into(), &(page as f64).into())
            .map_err(|e| format!("{e:?}"))?;
    }
    if let Some(url) = url {
        js_sys::Reflect::set(&message, &"url".into(), &url.into()).map_err(|e| format!("{e:?}"))?;
    }
    invoke("worker", &js_sys::Array::of1(&message)).await
}
pub async fn logout() -> Result<(), String> {
    invoke("logout", &js_sys::Array::new()).await.map(|_| ())
}
pub fn persist(key: &str, value: Option<String>) {
    let args = js_sys::Array::of2(
        &key.into(),
        &value.map(JsValue::from).unwrap_or(JsValue::NULL),
    );
    // Enqueue synchronously, before a later acknowledgement/removal can run.
    if let Err(error) = call("persist", &args) {
        leptos::logging::warn!("{error}");
    }
}
