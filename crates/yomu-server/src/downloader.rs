//! Download worker: fetches pending chapters page by page onto disk.
//! Single worker, Notify-woken with a periodic safety poll — the same
//! pattern as the chaos archiver. Rate limiting toward the site is handled
//! inside the source itself.

use std::time::Duration;

use yomu_domain::Chapter;

use crate::state::AppState;

pub fn spawn(state: AppState) {
    tokio::spawn(run(state));
}

async fn run(state: AppState) {
    loop {
        match state.db.next_pending_download().await {
            Ok(Some(chapter)) => {
                if let Err(err) = state.db.set_downloading(chapter.id).await {
                    tracing::error!(%err, "marking chapter downloading");
                    continue;
                }
                let outcome = download_chapter(&state, &chapter).await;
                if let Err(err) = state.db.finish_download(chapter.id, outcome).await {
                    tracing::error!(%err, "recording download outcome");
                }
                continue; // drain the queue before sleeping
            }
            Ok(None) => {}
            Err(err) => tracing::error!(%err, "polling download queue"),
        }

        tokio::select! {
            _ = state.download_notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

/// Fetch every page of the chapter into its directory.
/// Files are `0000.<ext>`, `0001.<ext>`… so a directory listing sorts into
/// reading order without any index file.
async fn download_chapter(state: &AppState, chapter: &Chapter) -> Result<u32, String> {
    let manga = state
        .db
        .get_manga(chapter.manga_id)
        .await
        .map_err(|e| e.to_string())?;
    let source = state
        .sources
        .get(&manga.source_id)
        .ok_or_else(|| format!("source {:?} not configured", manga.source_id))?;

    tracing::info!(manga = %manga.title, chapter = %chapter.title, "downloading");

    let pages = source
        .pages(&chapter.source_key)
        .await
        .map_err(|e| e.to_string())?;

    let dir = state.chapter_dir(chapter.manga_id, chapter.id);
    let partial = dir.with_extension("partial");
    let _ = tokio::fs::remove_dir_all(&partial).await;
    tokio::fs::create_dir_all(&partial)
        .await
        .map_err(|e| format!("creating {}: {e}", partial.display()))?;

    for (index, url) in pages.iter().enumerate() {
        let image = source.image(url).await.map_err(|e| e.to_string())?;
        let ext = extension_for(&image.content_type, url);
        let path = partial.join(format!("{index:04}.{ext}"));
        tokio::fs::write(&path, &image.bytes)
            .await
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
    }

    // Atomic-ish publish: the final directory only ever appears complete.
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::rename(&partial, &dir)
        .await
        .map_err(|e| format!("publishing {}: {e}", dir.display()))?;

    Ok(pages.len() as u32)
}

/// Downloaded page files for a chapter, in reading order.
pub async fn page_files(state: &AppState, chapter: &Chapter) -> std::io::Result<Vec<std::path::PathBuf>> {
    let dir = state.chapter_dir(chapter.manga_id, chapter.id);
    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = reader.next_entry().await? {
        entries.push(entry.path());
    }
    entries.sort();
    Ok(entries)
}

pub fn extension_for(content_type: &str, url: &url::Url) -> &'static str {
    match content_type {
        t if t.contains("png") => "png",
        t if t.contains("webp") => "webp",
        t if t.contains("gif") => "gif",
        t if t.contains("avif") => "avif",
        t if t.contains("jpeg") || t.contains("jpg") => "jpg",
        _ => match url.path().rsplit_once('.').map(|(_, e)| e) {
            Some("png") => "png",
            Some("webp") => "webp",
            Some("gif") => "gif",
            Some("avif") => "avif",
            _ => "jpg",
        },
    }
}

/// Content type for a stored page file (by extension).
pub fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        _ => "image/jpeg",
    }
}
