//! Background driver for the device-pull queue ("download both"): once a
//! queued chapter's server download finishes, save it to this device, in
//! queue (oldest-first) order. Runs app-wide so it survives leaving the
//! manga page; the queue itself is persisted (see offline::save_pull_queue).

use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashMap;
use uuid::Uuid;
use yomu_client::YomuClient;
use yomu_domain::{DownloadQueueEntry, DownloadState};

use crate::{Connectivity, DeviceMarks, LocalDownloads, PullQueue};

/// Start the 3s poller; call once from `App`.
pub fn start(
    conn: RwSignal<Connectivity>,
    client: YomuClient,
    queue: PullQueue,
    local: LocalDownloads,
    marks: DeviceMarks,
) {
    let running = StoredValue::new(false);
    let tick = move || {
        if running.get_value()
            || conn.get_untracked() != Connectivity::Online
            || queue.with_untracked(|q| q.is_empty())
        {
            return;
        }
        running.set_value(true);
        let client = client.clone();
        spawn_local(async move {
            drive(&client, queue, local, marks).await;
            running.set_value(false);
        });
    };
    let closure = leptos::wasm_bindgen::closure::Closure::<dyn Fn()>::new(tick);
    if let Some(window) = web_sys::window() {
        use leptos::wasm_bindgen::JsCast;
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            3000,
        );
    }
    closure.forget(); // lives for the whole app
}

async fn drive(client: &YomuClient, queue: PullQueue, local: LocalDownloads, marks: DeviceMarks) {
    let Ok(downloads) = client.downloads().await else {
        return; // transient; next tick retries, queue untouched
    };
    let views = server_views(&downloads.queue);
    // Walk oldest-first; pull the leading ready run, stop at the first
    // still-downloading item so ascending order is preserved.
    while let Some(item) = queue.with_untracked(|q| q.first().cloned()) {
        let id = item.chapter_id;
        if marks.with_untracked(|m| m.contains_key(&id)) {
            remove(queue, id); // already on device
            continue;
        }
        match view_of(&views, id) {
            ServerView::Failed => {
                leptos::logging::warn!("pull queue: server download failed for {id}");
                remove(queue, id); // the server gave up; nothing to pull
                continue;
            }
            ServerView::Unavailable => {
                // Not a failure: the source does not offer this chapter, so
                // the pages the pull would ask for do not exist. Dropping it
                // quietly is the whole point — going ahead would fetch the
                // paywall live and paint the row red.
                leptos::logging::log!(
                    "pull queue: {id} is not available from the source; nothing to pull"
                );
                remove(queue, id);
                continue;
            }
            ServerView::Busy => break, // not ready yet — keep it and the rest queued
            ServerView::Ready => {}
        }
        if local.with_untracked(|m| m.contains_key(&id)) {
            break; // its pull is already in flight
        }
        remove(queue, id);
        let _ = crate::pages::save_locally(
            client,
            item.manga_id,
            item.manga_title.clone(),
            id,
            item.chapter_title.clone(),
            local,
            marks,
        )
        .await;
    }
}

fn remove(queue: PullQueue, id: Uuid) {
    queue.update(|q| q.retain(|it| it.chapter_id != id));
}

/// What the server's queue says about a chapter this device wants to pull.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerView {
    /// The server is still working on it: wait, and keep the rest queued.
    Busy,
    /// The server gave up. Nothing will arrive, so drop it.
    Failed,
    /// The source does not offer the chapter. Also nothing to pull, but no
    /// failure — pulling anyway would resolve the pages live and fail there.
    Unavailable,
    /// Downloaded, dismissed, or gone from the queue: go ahead and pull.
    Ready,
}

/// Classify the server queue for the pull driver. Kept pure and total so
/// there is exactly one place that decides what a state means; anything not
/// listed here (notably a chapter absent from the queue) is `Ready`.
fn server_views(queue: &[DownloadQueueEntry]) -> HashMap<Uuid, ServerView> {
    queue
        .iter()
        .map(|e| {
            let view = match e.state {
                DownloadState::Pending | DownloadState::Downloading => ServerView::Busy,
                DownloadState::Failed { .. } => ServerView::Failed,
                DownloadState::Unavailable { .. } => ServerView::Unavailable,
                _ => ServerView::Ready,
            };
            (e.unit_id, view)
        })
        .collect()
}

fn view_of(views: &HashMap<Uuid, ServerView>, id: Uuid) -> ServerView {
    views.get(&id).copied().unwrap_or(ServerView::Ready)
}

#[cfg(test)]
mod tests {
    use super::{ServerView, server_views, view_of};
    use uuid::Uuid;
    use yomu_domain::{DownloadQueueEntry, DownloadState};

    fn entry(n: u128, state: DownloadState) -> DownloadQueueEntry {
        DownloadQueueEntry {
            unit_id: Uuid::from_u128(n),
            publication_id: Uuid::nil(),
            publication_title: "A publication".into(),
            unit_title: "Chapter 1".into(),
            state,
            progress: None,
        }
    }

    fn at() -> chrono::DateTime<chrono::Utc> {
        "2026-07-29T00:00:00Z".parse().unwrap()
    }

    /// A chapter the source will not serve must never look ready to the pull
    /// driver: "ready" means fetch the pages, which for a chapter the server
    /// never downloaded resolves live and dies on the paywall — a red row
    /// and a "Local save failed" for something that simply is not offered.
    #[test]
    fn an_unavailable_chapter_is_dropped_rather_than_pulled() {
        let queue = vec![
            entry(1, DownloadState::Pending),
            entry(2, DownloadState::Downloading),
            entry(
                3,
                DownloadState::Failed {
                    at: at(),
                    reason: "boom".into(),
                },
            ),
            entry(
                4,
                DownloadState::Unavailable {
                    at: at(),
                    reason: "premium on this source".into(),
                },
            ),
            entry(5, DownloadState::Downloaded { at: at() }),
        ];
        let views = server_views(&queue);
        let view = |n| view_of(&views, Uuid::from_u128(n));
        assert_eq!(view(1), ServerView::Busy);
        assert_eq!(view(2), ServerView::Busy);
        assert_eq!(view(3), ServerView::Failed);
        assert_eq!(view(4), ServerView::Unavailable);
        assert_eq!(view(5), ServerView::Ready);
        // Gone from the server queue (dismissed, or never queued there): the
        // device pull is free to go ahead.
        assert_eq!(view(99), ServerView::Ready);
    }
}
