//! Download worker: fetches pending chapters page by page onto disk.
//! Single worker, Notify-woken with a periodic safety poll — the same
//! pattern as the chaos archiver. Rate limiting toward the site is handled
//! inside the source itself.

use std::time::Duration;

use yomu_domain::{Origin, ReadingUnit};

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
                let succeeded = outcome.is_ok();
                match state.db.finish_download(chapter.id, outcome).await {
                    // Publication deleted while we were downloading: the files
                    // just published belong to nothing — remove them.
                    Ok(false) => {
                        let dir = state.unit_dir(chapter.publication_id, chapter.id);
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        if let Some(parent) = dir.parent() {
                            let _ = tokio::fs::remove_dir(parent).await; // only if empty
                        }
                    }
                    Ok(true) => {
                        if succeeded {
                            // Readers switch to the on-disk copy.
                            state.live_pages.invalidate(chapter.id).await;
                        }
                    }
                    Err(err) => tracing::error!(%err, "recording download outcome"),
                }
                // ReadingUnit left the worker: no active progress until the next.
                *state.download_progress.write().await = None;
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
async fn download_chapter(state: &AppState, chapter: &ReadingUnit) -> Result<u32, String> {
    let publication = state
        .db
        .get_publication(chapter.publication_id)
        .await
        .map_err(|e| e.to_string())?;
    // LocalFile publications are never queued; the streamer serves their
    // pages straight from the file.
    let Origin::Source { source_id, .. } = &publication.origin else {
        return Err("publication is not source-backed".into());
    };
    let source = state
        .sources
        .get(source_id)
        .ok_or_else(|| format!("source {source_id:?} not configured"))?;

    tracing::info!(publication = %publication.title, chapter = %chapter.title, "downloading");

    let pages = source
        .pages(&chapter.source_key)
        .await
        .map_err(|e| e.to_string())?;

    let dir = state.unit_dir(chapter.publication_id, chapter.id);
    let partial = dir.with_extension("partial");
    let _ = tokio::fs::remove_dir_all(&partial).await;
    tokio::fs::create_dir_all(&partial)
        .await
        .map_err(|e| format!("creating {}: {e}", partial.display()))?;

    let fetched = fetch_pages(state, chapter.id, source.as_ref(), &pages, &partial).await;
    if let Err(reason) = fetched {
        // Don't leave half a chapter of litter behind a failed attempt.
        let _ = tokio::fs::remove_dir_all(&partial).await;
        return Err(reason);
    }

    // Atomic-ish publish that never destroys a good copy: stage the old
    // directory aside, promote the new one, then drop the old. If the
    // promotion fails (cross-device, permissions, race), restore the old
    // copy so a re-download can't lose an already-complete chapter.
    let backup = dir.with_extension("old");
    let _ = tokio::fs::remove_dir_all(&backup).await;
    let had_old = tokio::fs::rename(&dir, &backup).await.is_ok();
    if let Err(e) = tokio::fs::rename(&partial, &dir).await {
        if had_old {
            let _ = tokio::fs::rename(&backup, &dir).await;
        }
        return Err(format!("publishing {}: {e}", dir.display()));
    }
    let _ = tokio::fs::remove_dir_all(&backup).await;

    Ok(pages.len() as u32)
}

async fn fetch_pages(
    state: &AppState,
    unit_id: uuid::Uuid,
    source: &dyn yomu_source::Source,
    pages: &[url::Url],
    partial: &std::path::Path,
) -> Result<(), String> {
    let total = pages.len() as u32;
    for (index, url) in pages.iter().enumerate() {
        // Publish page progress for the Downloads UI before fetching each
        // page (so a slow page shows as "downloading page N", not stalled).
        *state.download_progress.write().await = Some(crate::state::ActiveDownload {
            unit_id,
            page: index as u32 + 1,
            total,
        });
        let image = source.image(url).await.map_err(|e| e.to_string())?;
        let ext = extension_for(&image.content_type, url);
        let path = partial.join(format!("{index:04}.{ext}"));
        tokio::fs::write(&path, &image.bytes)
            .await
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Downloaded page files for a chapter, in reading order.
pub async fn page_files(
    state: &AppState,
    chapter: &ReadingUnit,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    let dir = state.unit_dir(chapter.publication_id, chapter.id);
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
