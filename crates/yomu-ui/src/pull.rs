//! Background driver for the device-pull queue ("download both"): once a
//! queued chapter's server download finishes, save it to this device, in
//! queue (oldest-first) order. Runs app-wide so it survives leaving the
//! manga page; the queue itself is persisted (see offline::save_pull_queue).

use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;
use uuid::Uuid;
use yomu_client::YomuClient;
use yomu_domain::DownloadState;

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
    let mut busy: HashSet<Uuid> = HashSet::new();
    let mut failed: HashSet<Uuid> = HashSet::new();
    for e in &downloads.queue {
        match e.state {
            DownloadState::Pending | DownloadState::Downloading => {
                busy.insert(e.chapter_id);
            }
            DownloadState::Failed { .. } => {
                failed.insert(e.chapter_id);
            }
            _ => {}
        }
    }
    // Walk oldest-first; pull the leading ready run, stop at the first
    // still-downloading item so ascending order is preserved.
    while let Some(item) = queue.with_untracked(|q| q.first().cloned()) {
        let id = item.chapter_id;
        if marks.with_untracked(|m| m.contains_key(&id)) || failed.contains(&id) {
            if failed.contains(&id) {
                leptos::logging::warn!("pull queue: server download failed for {id}");
            }
            remove(queue, id); // already on device, or the server gave up
            continue;
        }
        if busy.contains(&id) {
            break; // not ready yet — keep it and the rest queued
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
