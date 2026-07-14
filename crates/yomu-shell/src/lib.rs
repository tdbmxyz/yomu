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
//! device" calls the [`device_save_chapter`] command, which stores pages
//! under the app data directory; the reader loads them back through the
//! `yomudev` custom protocol (base URL injected as `window.YOMU_DEVICE_BASE`).

use std::path::PathBuf;

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

/// Download every page of a chapter from the server into device storage.
/// Written to a `.partial-` directory and renamed, so a stored chapter is
/// always complete (same rule as the server's downloader).
#[tauri::command]
async fn device_save_chapter(
    app: tauri::AppHandle,
    http: State<'_, Http>,
    base: String,
    chapter: String,
    count: u32,
) -> Result<(), String> {
    checked_id(&chapter)?;
    let base = url::Url::parse(&base).map_err(|e| e.to_string())?;
    let dir = chapters_dir(&app)?;
    let partial = dir.join(format!(".partial-{chapter}"));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| e.to_string())?;

    for n in 0..count {
        let url = base
            .join(&format!("api/v1/chapters/{chapter}/pages/{n}"))
            .map_err(|e| e.to_string())?;
        let resp = http.0.get(url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("page {n}: HTTP {}", resp.status()));
        }
        let ext = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(extension_for)
            .unwrap_or("jpg");
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        std::fs::write(partial.join(format!("{n:04}.{ext}")), &bytes).map_err(|e| e.to_string())?;
    }

    let target = dir.join(&chapter);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    Ok(())
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
/// have no service worker to do it. Idempotent per manga: an existing file
/// is replaced.
#[tauri::command]
async fn device_save_cover(
    app: tauri::AppHandle,
    http: State<'_, Http>,
    base: String,
    manga: String,
) -> Result<(), String> {
    checked_id(&manga)?;
    let base = url::Url::parse(&base).map_err(|e| e.to_string())?;
    let url = base
        .join(&format!("api/v1/manga/{manga}/cover"))
        .map_err(|e| e.to_string())?;
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
    // drop any stale copy under another extension before writing
    for old in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = old.path();
        if path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == manga)
        {
            let _ = std::fs::remove_file(path);
        }
    }
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

    tauri::Builder::default()
        .manage(Http(reqwest::Client::new()))
        .invoke_handler(tauri::generate_handler![
            device_save_chapter,
            device_delete_chapter,
            device_save_cover
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
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running yomu shell");
}
