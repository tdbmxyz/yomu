//! Downloads: the server-side download queue (pending / downloading /
//! failed) with live per-page progress and retry/dismiss actions, plus a
//! server-vs-device storage overview. Polls while open.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_domain::{DownloadQueueEntry, DownloadState, DownloadsResponse};

use crate::offline;
use crate::use_client;

#[component]
pub fn Downloads() -> impl IntoView {
    let client = use_client();
    // A ticking signal drives the resource; the interval below bumps it every
    // couple of seconds so the queue tracks the worker while the page is open.
    let tick = RwSignal::new(0u32);
    let conn = crate::use_connectivity();
    let data = LocalResource::new({
        let client = client.clone();
        move || {
            tick.track();
            conn.track();
            let client = client.clone();
            async move {
                offline::cached(conn, "downloads", || client.downloads())
                    .await
                    .map(|(value, _)| value)
            }
        }
    });

    if let Ok(handle) =
        set_interval_with_handle(move || tick.update(|n| *n += 1), Duration::from_secs(2))
    {
        on_cleanup(move || handle.clear());
    }

    // Chapters saved on this device (localStorage marks — a per-device count).
    let device_count = offline::device_chapters().len() as u32;
    let local = crate::use_local_downloads();
    let pull = crate::use_pull_queue();

    let refetch = move || tick.update(|n| *n += 1);

    view! {
        <section class="downloads">
            <h2>"Downloads"</h2>
            {move || match data.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(Err(err)) => {
                    view! {
                        <p class="error">"Could not reach yomu server: " {err.to_string()}</p>
                    }
                        .into_any()
                }
                Some(Ok(resp)) => {
                    let refetch = refetch;
                    view! { <DownloadsView resp device_count local pull refetch/> }.into_any()
                }
            }}
        </section>
    }
}

#[component]
fn DownloadsView(
    resp: DownloadsResponse,
    device_count: u32,
    local: crate::LocalDownloads,
    pull: crate::PullQueue,
    refetch: impl Fn() + Clone + 'static + Send,
) -> impl IntoView {
    let split = |want: fn(&DownloadState) -> bool| -> Vec<DownloadQueueEntry> {
        resp.queue
            .iter()
            .filter(|e| want(&e.state))
            .cloned()
            .collect()
    };
    let downloading = split(|s| matches!(s, DownloadState::Downloading));
    let pending = split(|s| matches!(s, DownloadState::Pending));
    let failed = split(|s| matches!(s, DownloadState::Failed { .. }));

    let client = use_client();
    // Bulk action over a set of chapter ids, then refetch.
    let action = {
        let client = client.clone();
        let refetch = refetch.clone();
        move |ids: Vec<uuid::Uuid>, retry: bool| {
            if ids.is_empty() {
                return;
            }
            let client = client.clone();
            let refetch = refetch.clone();
            spawn_local(async move {
                let result = if retry {
                    client.retry_downloads(&ids).await
                } else {
                    client.dismiss_downloads(&ids).await
                };
                if let Err(err) = result {
                    leptos::logging::warn!("download action: {err}");
                }
                refetch();
            });
        }
    };

    let pending_ids: Vec<_> = pending.iter().map(|e| e.unit_id).collect();
    let failed_ids: Vec<_> = failed.iter().map(|e| e.unit_id).collect();
    let empty = downloading.is_empty() && pending.is_empty() && failed.is_empty();

    let cancel_pending = {
        let action = action.clone();
        let ids = pending_ids.clone();
        move |_| action(ids.clone(), false)
    };
    let retry_all = {
        let action = action.clone();
        let ids = failed_ids.clone();
        move |_| action(ids.clone(), true)
    };
    let clear_failed = {
        let action = action.clone();
        let ids = failed_ids.clone();
        move |_| action(ids.clone(), false)
    };

    view! {
        <div class="storage-overview">
            <div class="storage-tile">
                <span class="storage-num">{resp.server_downloaded_chapters}</span>
                <span class="muted">
                    "chapters on server · " {resp.server_downloaded_pages} " pages"
                </span>
            </div>
            <div class="storage-tile">
                <span class="storage-num">{device_count}</span>
                <span class="muted">"chapters on this device"</span>
            </div>
        </div>

        <h3 class="shelf-title downloads-section">"Server"</h3>
        {empty
            .then(|| {
                view! { <p class="muted">"Nothing in the server download queue."</p> }
            })}

        {(!downloading.is_empty())
            .then(|| {
                view! {
                    <h3 class="shelf-title">"Downloading"</h3>
                    <ul class="download-list">
                        {downloading
                            .into_iter()
                            .map(|entry| view! { <QueueRow entry/> })
                            .collect_view()}
                    </ul>
                }
            })}

        {(!pending.is_empty())
            .then(|| {
                let cancel_pending = cancel_pending.clone();
                view! {
                    <div class="download-group-head">
                        <h3 class="shelf-title">{format!("Pending ({})", pending.len())}</h3>
                        <button class="button" on:click=cancel_pending>"Cancel pending"</button>
                    </div>
                    <ul class="download-list">
                        {pending
                            .into_iter()
                            .map(|entry| view! { <QueueRow entry/> })
                            .collect_view()}
                    </ul>
                }
            })}

        {(!failed.is_empty())
            .then(|| {
                let retry_all = retry_all.clone();
                let clear_failed = clear_failed.clone();
                view! {
                    <div class="download-group-head">
                        <h3 class="shelf-title">{format!("Failed ({})", failed.len())}</h3>
                        <button class="button" on:click=retry_all>"Retry all"</button>
                        <button class="button" on:click=clear_failed>"Clear failed"</button>
                    </div>
                    <ul class="download-list">
                        {failed
                            .into_iter()
                            .map(|entry| view! { <QueueRow entry/> })
                            .collect_view()}
                    </ul>
                }
            })}

        <h3 class="shelf-title downloads-section">"On this device"</h3>
        {move || {
            let queued = pull.get();
            (!queued.is_empty())
                .then(|| {
                    view! {
                        <p class="muted downloads-waiting-head">
                            "Waiting for server download"
                        </p>
                        <ul class="download-list">
                            {queued
                                .into_iter()
                                .map(|it| view! { <WaitingRow it pull/> })
                                .collect_view()}
                        </ul>
                    }
                })
        }}
        {move || {
            let items: Vec<_> = local.with(|m| {
                let mut v: Vec<_> = m.iter().map(|(id, d)| (*id, d.clone())).collect();
                v.sort_by(|a, b| a.1.manga_title.cmp(&b.1.manga_title));
                v
            });
            if items.is_empty() {
                (pull.get().is_empty())
                    .then(|| {
                        view! {
                            <p class="muted">
                                {format!("{device_count} chapters on this device")}
                            </p>
                        }
                    })
                    .into_any()
            } else {
                view! {
                    <ul class="download-list">
                        {items
                            .into_iter()
                            .map(|(id, d)| view! { <LocalRow id d local/> })
                            .collect_view()}
                    </ul>
                }
                    .into_any()
            }
        }}
    }
}

/// A chapter queued to pull to this device once its server download
/// finishes ("download both"); Cancel drops it from the queue.
#[component]
fn WaitingRow(it: crate::PullItem, pull: crate::PullQueue) -> impl IntoView {
    let id = it.chapter_id;
    let cancel = move |_| pull.update(|q| q.retain(|e| e.chapter_id != id));
    view! {
        <li class="download-row">
            <div class="download-row-head">
                <a class="download-title" href=format!("/manga/{}", it.manga_id)>
                    <strong>{it.manga_title}</strong>
                    " · " {it.chapter_title}
                </a>
                <button class="button" on:click=cancel>"Cancel"</button>
            </div>
            <span class="muted">"waiting for server download…"</span>
        </li>
    }
}

/// One queue row: manga · chapter, plus a progress bar (downloading) or the
/// error text (failed).
#[component]
fn QueueRow(entry: DownloadQueueEntry) -> impl IntoView {
    let progress = entry.progress.clone();
    let error = match &entry.state {
        DownloadState::Failed { reason, .. } => Some(reason.clone()),
        _ => None,
    };
    view! {
        <li class="download-row">
            <a class="download-title" href=format!("/manga/{}", entry.publication_id)>
                <strong>{entry.publication_title}</strong>
                " · " {entry.unit_title}
            </a>
            {progress
                .map(|p| {
                    let pct = if p.total > 0 {
                        (p.page as f64 / p.total as f64) * 100.0
                    } else {
                        0.0
                    };
                    view! {
                        <div class="download-progress">
                            <div class="download-progress-bar" style:width=format!("{pct}%")></div>
                            <span class="muted download-progress-label">
                                {format!("{}/{}", p.page, p.total)}
                            </span>
                        </div>
                    }
                })}
            {error.map(|reason| view! { <span class="error download-error">{reason}</span> })}
        </li>
    }
}

/// One in-flight device save: manga · chapter, a page progress bar, and a
/// Cancel button that flags the save loop to stop.
#[component]
fn LocalRow(
    id: uuid::Uuid,
    d: crate::LocalDownload,
    local: crate::LocalDownloads,
) -> impl IntoView {
    let cancel = move |_| {
        local.update(|m| {
            if let Some(entry) = m.get_mut(&id) {
                entry.cancel_requested = true;
            }
        });
    };
    let pct = if d.total > 0 {
        (d.done as f64 / d.total as f64) * 100.0
    } else {
        0.0
    };
    let cancelling = d.cancel_requested;
    view! {
        <li class="download-row" class:dl-failed=d.failed>
            <div class="download-row-head">
                <a class="download-title" href=format!("/manga/{}", d.manga_id)>
                    <strong>{d.manga_title}</strong>
                    " · " {d.chapter_title}
                </a>
                <button class="button" on:click=cancel disabled=cancelling>
                    "Cancel"
                </button>
            </div>
            <div class="download-progress">
                <div class="download-progress-bar" style:width=format!("{pct}%")></div>
                <span class="muted download-progress-label">
                    {if cancelling {
                        "Cancelling…".to_string()
                    } else {
                        format!("{}/{}", d.done, d.total)
                    }}
                </span>
            </div>
        </li>
    }
}
