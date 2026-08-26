//! Tauri shell: loads the bundled web UI and tells it where the server is.
//!
//! The UI resolves its API base from `window.YOMU_API_BASE` first (see
//! yomu-web/src/main.rs); the shell injects it before the bundle runs. The
//! address comes from, in order: the `YOMU_SERVER` env var (desktop dev),
//! `$XDG_CONFIG_HOME/yomu/server` (one line, desktop), or nothing — then the
//! UI's own resolution takes over (localStorage override set through the
//! in-app connect screen, which is the path on Android).
//!
//! Device downloads: webviews here have no service worker, so "save to
//! device" drives [`device_begin_chapter`] / [`device_save_page`] /
//! [`device_finish_chapter`], which store pages under the app data
//! directory; the reader loads them back through the `yomudev` custom
//! protocol (base URL injected as `window.YOMU_DEVICE_BASE`).

pub mod auth;

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

fn configured_server() -> Option<String> {
    if let Ok(url) = std::env::var("YOMU_SERVER") {
        return Some(url.trim().to_string());
    }
    let config = dirs_config()?.join("yomu/server");
    let raw = std::fs::read_to_string(config).ok()?;
    let url = raw.trim();
    (!url.is_empty()).then(|| url.to_string())
}

fn dirs_config() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http(s) links open externally".into());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = url;
        Err("use the Android URL bridge".into())
    }
}

fn handle_auth_callback<R: tauri::Runtime>(app: &tauri::AppHandle<R>, raw: &str) {
    let app = app.clone();
    let raw = raw.to_string();
    tauri::async_runtime::spawn(async move {
        let Some((code, state)) = auth::parse_callback(&raw) else {
            return;
        };
        if let Err(err) = auth::finish(&app, &code, &state).await {
            eprintln!("yomu: sign-in callback failed: {err}");
        }
    });
}

// ---- durable client state ----

#[tauri::command]
async fn store_snapshot(
    store: State<'_, yomu_store::Store>,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    store.state_snapshot().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn store_put(
    store: State<'_, yomu_store::Store>,
    key: String,
    value: String,
) -> Result<(), String> {
    store
        .put_state(&key, &value)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn store_remove(store: State<'_, yomu_store::Store>, key: String) -> Result<(), String> {
    store.remove_state(&key).await.map_err(|e| e.to_string())
}

// ---- device chapter storage ----

struct Http(reqwest::Client);

fn chapters_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("chapters"))
}

/// Chapter ids come from the (trusted) UI, but they are also path segments:
/// only accept plain UUID-looking strings.
fn checked_id(chapter: &str) -> Result<&str, String> {
    let ok = !chapter.is_empty()
        && chapter
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then_some(chapter)
        .ok_or_else(|| "invalid chapter id".into())
}

fn extension_for(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/avif" => "avif",
        _ => "jpg",
    }
}

fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        _ => "image/jpeg",
    }
}

/// Device chapter saves run page by page (the UI drives the loop so it
/// can show progress), against a `.partial-` directory that only becomes
/// the chapter on `device_finish_chapter` — a stored chapter is always
/// complete (same rule as the server's downloader).
#[tauri::command]
fn device_begin_chapter(app: tauri::AppHandle, chapter: String) -> Result<(), String> {
    checked_id(&chapter)?;
    let partial = chapters_dir(&app)?.join(format!(".partial-{chapter}"));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| e.to_string())
}

/// Download one page of a chapter into its `.partial-` directory.
#[tauri::command]
async fn device_save_page(
    app: tauri::AppHandle,
    http: State<'_, Http>,
    url: String,
    chapter: String,
    page: u32,
) -> Result<(), String> {
    checked_id(&chapter)?;
    let url = url::Url::parse(&url).map_err(|e| e.to_string())?;
    let resp = http.0.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("page {page}: HTTP {}", resp.status()));
    }
    let ext = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(extension_for)
        .unwrap_or("jpg");
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let partial = chapters_dir(&app)?.join(format!(".partial-{chapter}"));
    std::fs::write(partial.join(format!("{page:04}.{ext}")), &bytes).map_err(|e| e.to_string())
}

/// Land a completed `.partial-` directory as the stored chapter.
#[tauri::command]
fn device_finish_chapter(app: tauri::AppHandle, chapter: String) -> Result<(), String> {
    checked_id(&chapter)?;
    let dir = chapters_dir(&app)?;
    let partial = dir.join(format!(".partial-{chapter}"));
    let target = dir.join(&chapter);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())
}

fn covers_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("covers"))
}

/// Download a manga's (server-cached) cover into device storage, so the
/// library keeps its covers with the server unreachable — webviews here
/// have no service worker to do it. An existing copy short-circuits, so
/// the UI can re-submit its whole library cheaply on every load (which
/// also self-heals after any file loss — there is no separate bookkeeping
/// to drift from the files).
#[tauri::command]
async fn device_save_cover(
    app: tauri::AppHandle,
    http: State<'_, Http>,
    url: String,
    manga: String,
) -> Result<(), String> {
    checked_id(&manga)?;
    if device_cover_file(&app, &manga).is_some() {
        return Ok(());
    }
    let url = url::Url::parse(&url).map_err(|e| e.to_string())?;
    let resp = http.0.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("cover: HTTP {}", resp.status()));
    }
    let ext = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(extension_for)
        .unwrap_or("jpg");
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let dir = covers_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{manga}.{ext}")), &bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn device_cover_file(app: &tauri::AppHandle, manga: &str) -> Option<PathBuf> {
    let dir = covers_dir(app).ok()?;
    let stem = checked_id(manga).ok()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_stem()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f == stem)
        })
}

/// Remove a chapter from device storage.
#[tauri::command]
fn device_delete_chapter(app: tauri::AppHandle, chapter: String) -> Result<(), String> {
    checked_id(&chapter)?;
    let dir = chapters_dir(&app)?.join(&chapter);
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

// ---- recovering chapters the server re-keyed ----
//
// A stored chapter is named after its unit id, so when a source changed its
// URLs and the server re-keyed every unit, the directories here stopped
// matching anything and the downloads became invisible. No mapping survives
// to undo that, but the bytes do: these pages are byte-for-byte what the
// server stored, so a chapter can be recognised by its page count and the
// hash of its first page and then renamed to the id it now has.

/// Chapter directories currently on the device. A `.partial-` directory is a
/// download in progress, not a chapter, so it is not listed.
#[tauri::command]
fn device_list_chapters(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(chapters_dir(&app)?) {
        Ok(entries) => entries,
        // Nothing has ever been saved: an empty device, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut chapters: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| !name.starts_with(".partial-"))
        .collect();
    chapters.sort();
    Ok(chapters)
}

/// What identifies a stored chapter by content rather than by name. The field
/// names cross the bridge verbatim (serde → the JS object the UI reads in
/// `offline::shell_chapter_fingerprint`), so they must stay in step with it.
#[derive(serde::Serialize)]
struct DeviceFingerprint {
    page_count: u32,
    sha256_first: String,
    sha256_last: String,
}

/// Fingerprint one stored chapter: how many page files it holds, and the hash
/// of its lowest- and highest-numbered page. Page files are zero-padded
/// (`0000.jpg`), so name order is page order — the same order the server hands
/// back. Both ends are hashed because two chapters sharing a first page (a
/// credits page, a site splash) is ordinary, and one hash cannot then tell
/// them apart; on a one-page chapter both hashes are that page.
#[tauri::command]
fn device_chapter_fingerprint(
    app: tauri::AppHandle,
    chapter: String,
) -> Result<DeviceFingerprint, String> {
    checked_id(&chapter)?;
    let dir = chapters_dir(&app)?.join(&chapter);
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    pages.sort();
    let first = pages
        .first()
        .ok_or_else(|| format!("chapter {chapter} has no pages"))?;
    let last = pages.last().expect("non-empty: first() succeeded");
    let first = std::fs::read(first).map_err(|e| e.to_string())?;
    let last = std::fs::read(last).map_err(|e| e.to_string())?;
    Ok(DeviceFingerprint {
        page_count: pages.len() as u32,
        sha256_first: hex::encode(Sha256::digest(&first)),
        sha256_last: hex::encode(Sha256::digest(&last)),
    })
}

/// Re-key a stored chapter. Refuses when the target already exists: that is
/// a chapter the device holds under its current id, and overwriting it would
/// destroy a good download to save a stale one.
#[tauri::command]
fn device_rename_chapter(app: tauri::AppHandle, from: String, to: String) -> Result<(), String> {
    checked_id(&from)?;
    checked_id(&to)?;
    let dir = chapters_dir(&app)?;
    let target = dir.join(&to);
    if target.exists() {
        return Err(format!("chapter {to} is already stored on this device"));
    }
    std::fs::rename(dir.join(&from), &target).map_err(|e| e.to_string())
}

fn device_page_file(app: &tauri::AppHandle, chapter: &str, n: u32) -> Option<PathBuf> {
    let dir = chapters_dir(app).ok()?.join(checked_id(chapter).ok()?);
    let prefix = format!("{n:04}.");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with(&prefix))
        })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMABUF renderer draws a blank window on the NVIDIA
    // driver; disable it there unless the user decided themselves.
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/driver/nvidia").exists()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    let builder = tauri::Builder::default();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
        for arg in argv {
            handle_auth_callback(app, &arg);
        }
    }));

    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_deep_link::init())
        .manage(Http(reqwest::Client::new()))
        .invoke_handler(tauri::generate_handler![
            open_external,
            auth::auth_start,
            auth::auth_status,
            auth::auth_sign_out,
            store_snapshot,
            store_put,
            store_remove,
            device_begin_chapter,
            device_save_page,
            device_finish_chapter,
            device_delete_chapter,
            device_save_cover,
            device_list_chapters,
            device_chapter_fingerprint,
            device_rename_chapter
        ])
        // Serves device-saved content: yomudev://localhost/chapter/<id>/<n>
        // and yomudev://localhost/cover/<manga>
        // (http://yomudev.localhost/… on Android/Windows).
        .register_uri_scheme_protocol("yomudev", |ctx, request| {
            let not_found = || {
                tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .expect("static response")
            };
            let path = request.uri().path().trim_start_matches('/').to_string();
            let mut parts = path.split('/');
            let file = match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("chapter"), Some(chapter), Some(n), None) => {
                    let Ok(n) = n.parse::<u32>() else {
                        return not_found();
                    };
                    device_page_file(ctx.app_handle(), chapter, n)
                }
                (Some("cover"), Some(manga), None, _) => device_cover_file(ctx.app_handle(), manga),
                _ => None,
            };
            let Some(file) = file else {
                return not_found();
            };
            match std::fs::read(&file) {
                Ok(bytes) => tauri::http::Response::builder()
                    .header("content-type", content_type_for(&file))
                    .body(bytes)
                    .unwrap_or_else(|_| not_found()),
                Err(_) => not_found(),
            }
        })
        .setup(|app| {
            let store_path = app.path().app_data_dir()?.join("client-state.db");
            let store = tauri::async_runtime::block_on(yomu_store::Store::open(&store_path))?;
            app.manage(store);

            let device_base = if cfg!(any(windows, target_os = "android")) {
                "http://yomudev.localhost/"
            } else {
                "yomudev://localhost/"
            };
            let mut window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("yomu")
                    .initialization_script(format!("window.YOMU_DEVICE_BASE = '{device_base}';"));
            if let Some(server) = configured_server().filter(|s| url::Url::parse(s).is_ok()) {
                // serde_json-free single-quoted injection: the URL was just
                // validated, but escape quotes anyway.
                let escaped = server.replace('\\', "\\\\").replace('\'', "\\'");
                window =
                    window.initialization_script(format!("window.YOMU_API_BASE = '{escaped}';"));
            }
            window.build()?;

            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_auth_callback(&handle, url.as_str());
                }
            });
            match app.deep_link().get_current() {
                Ok(Some(urls)) => {
                    for url in urls {
                        handle_auth_callback(app.handle(), url.as_str());
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    auth::record_status(app.handle(), &format!("deep link unavailable: {err}"))
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running yomu shell");
}
