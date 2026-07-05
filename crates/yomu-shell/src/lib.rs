//! Tauri shell: loads the bundled web UI and tells it where the server is.
//!
//! The UI resolves its API base from `window.YOMU_API_BASE` first (see
//! yomu-web/src/main.rs); the shell injects it before the bundle runs. The
//! address comes from, in order: the `YOMU_SERVER` env var (desktop dev),
//! `$XDG_CONFIG_HOME/yomu/server` (one line, desktop), or nothing — then the
//! UI's own resolution takes over (localStorage override set through the
//! in-app connect screen, which is the path on Android).

use tauri::{WebviewUrl, WebviewWindowBuilder};

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
        .setup(|app| {
            let mut window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("yomu");
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
