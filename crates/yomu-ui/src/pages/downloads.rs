//! Downloads: what is moving right now (device saves and the server's
//! current fetch) on top, then the queues behind it — chapters waiting to
//! be pulled to this device, then the server's pending / failed work, with
//! retry and dismiss actions. Polls while open.
//!
//! Ordering is "what happens next, first": the server list arrives in the
//! downloader's own order (see `Db::download_queue`) and the pull queue is
//! already oldest-first, so neither is re-sorted here.

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
    let Groups {
        downloading,
        pending,
        failed,
        unavailable,
    } = groups(&resp.queue);

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

    let pending_ids = ids(&pending);
    let failed_ids = ids(&failed);
    let unavailable_ids = ids(&unavailable);
    // The server's in-flight fetch joins the device saves in the top block,
    // so it is rendered from a stored copy rather than in place.
    let server_active = StoredValue::new(downloading);

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
    // Dismiss only: retrying a chapter the source does not offer would just
    // fetch the same paywall again.
    let dismiss_unavailable = {
        let action = action.clone();
        let ids = unavailable_ids.clone();
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

        // What is actually moving: this device's save first (it is the one
        // the user is waiting on), then the server's current fetch.
        {move || {
            let device = local.with(device_rows);
            let server = server_active.get_value();
            if device.is_empty() && server.is_empty() {
                return view! {
                    <h3 class="shelf-title downloads-section">"In progress"</h3>
                    <p class="muted">"Nothing downloading right now."</p>
                }
                    .into_any();
            }
            view! {
                <h3 class="shelf-title downloads-section">"In progress"</h3>
                <ul class="download-list">
                    {device
                        .into_iter()
                        .map(|(id, d)| view! { <LocalRow id d local/> })
                        .collect_view()}
                    {server
                        .into_iter()
                        .map(|entry| view! { <QueueRow entry where_server=true/> })
                        .collect_view()}
                </ul>
            }
                .into_any()
        }}

        // Queued for this device: waiting on the server to finish its copy.
        {move || {
            let queued = pull.get();
            (!queued.is_empty())
                .then(|| {
                    view! {
                        <h3 class="shelf-title downloads-section">
                            {format!("Waiting for server download ({})", queued.len())}
                        </h3>
                        <ul class="download-list">
                            {queued
                                .into_iter()
                                .map(|it| view! { <WaitingRow it pull/> })
                                .collect_view()}
                        </ul>
                    }
                })
        }}

        {(!pending.is_empty())
            .then(|| {
                let cancel_pending = cancel_pending.clone();
                view! {
                    <div class="download-group-head">
                        <h3 class="shelf-title downloads-section">
                            {format!("Server · Pending ({})", pending.len())}
                        </h3>
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
                        <h3 class="shelf-title downloads-section">
                            {format!("Server · Failed ({})", failed.len())}
                        </h3>
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

        // Not a fault: the source does not offer these chapters, so there is
        // nothing to retry — only to acknowledge and clear.
        {(!unavailable.is_empty())
            .then(|| {
                let dismiss_unavailable = dismiss_unavailable.clone();
                view! {
                    <div class="download-group-head">
                        <h3 class="shelf-title downloads-section">
                            {format!("Not available ({})", unavailable.len())}
                        </h3>
                        <button class="button" on:click=dismiss_unavailable>"Dismiss"</button>
                    </div>
                    <ul class="download-list">
                        {unavailable
                            .into_iter()
                            .map(|entry| view! { <QueueRow entry/> })
                            .collect_view()}
                    </ul>
                }
            })}

    }
}

/// The queue split into the groups the page renders.
#[derive(Debug, Default, PartialEq)]
struct Groups {
    downloading: Vec<DownloadQueueEntry>,
    pending: Vec<DownloadQueueEntry>,
    failed: Vec<DownloadQueueEntry>,
    unavailable: Vec<DownloadQueueEntry>,
}

/// Split the queue into its display groups. The bulk actions collect their
/// ids from these lists, so this one function is what keeps an unavailable
/// chapter out of `Retry all`: it is a state of its own, never a `Failed`.
/// The view builds its groups here and nowhere else, so there is no second
/// place for the distinction to be got wrong.
fn groups(queue: &[DownloadQueueEntry]) -> Groups {
    let mut g = Groups::default();
    for entry in queue {
        let bucket = match entry.state {
            DownloadState::Downloading => &mut g.downloading,
            DownloadState::Pending => &mut g.pending,
            DownloadState::Failed { .. } => &mut g.failed,
            DownloadState::Unavailable { .. } => &mut g.unavailable,
            _ => continue,
        };
        bucket.push(entry.clone());
    }
    g
}

fn ids(entries: &[DownloadQueueEntry]) -> Vec<uuid::Uuid> {
    entries.iter().map(|e| e.unit_id).collect()
}

/// In-flight device saves, ordered for display: a save that just failed
/// stays at the bottom (it lingers ~1.5s before the row disappears) so it
/// never pushes live progress out of view; the rest go by title, then
/// chapter, so a multi-chapter pull reads as one stable list.
fn device_rows(
    map: &std::collections::HashMap<uuid::Uuid, crate::LocalDownload>,
) -> Vec<(uuid::Uuid, crate::LocalDownload)> {
    let mut v: Vec<_> = map.iter().map(|(id, d)| (*id, d.clone())).collect();
    v.sort_by(|a, b| {
        a.1.failed
            .cmp(&b.1.failed)
            .then_with(|| a.1.manga_title.cmp(&b.1.manga_title))
            .then_with(|| a.1.chapter_title.cmp(&b.1.chapter_title))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::{device_rows, groups, ids};
    use crate::LocalDownload;
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

    /// The whole point of the state: `Retry all` re-queues the failed group,
    /// and a chapter the source will not serve must never end up in it —
    /// retrying only re-fetches the same paywall. This asserts on the very
    /// function the view groups with, so a slip there fails here.
    #[test]
    fn retry_all_never_collects_an_unavailable_unit() {
        let queue = vec![
            entry(
                1,
                DownloadState::Failed {
                    at: at(),
                    reason: "boom".into(),
                },
            ),
            entry(
                2,
                DownloadState::Unavailable {
                    at: at(),
                    reason: "premium on this source".into(),
                },
            ),
            entry(3, DownloadState::Pending),
            entry(4, DownloadState::Downloading),
            entry(5, DownloadState::Downloaded { at: at() }),
        ];
        let g = groups(&queue);
        assert_eq!(ids(&g.failed), vec![Uuid::from_u128(1)]);
        assert_eq!(ids(&g.unavailable), vec![Uuid::from_u128(2)]);
        assert_eq!(ids(&g.pending), vec![Uuid::from_u128(3)]);
        assert_eq!(ids(&g.downloading), vec![Uuid::from_u128(4)]);
        // A finished download belongs to no group and to no bulk action.
        assert!(
            !ids(&g.failed).contains(&Uuid::from_u128(5))
                && !ids(&g.pending).contains(&Uuid::from_u128(5))
        );
    }

    fn save(title: &str, chapter: &str, failed: bool) -> LocalDownload {
        LocalDownload {
            manga_id: Uuid::nil(),
            manga_title: title.into(),
            chapter_title: chapter.into(),
            done: 0,
            total: 10,
            failed,
            cancel_requested: false,
        }
    }

    #[test]
    fn device_rows_keep_live_saves_above_a_failed_one() {
        let map = std::collections::HashMap::from([
            (Uuid::from_u128(1), save("Zeta", "Chapter 2", false)),
            (Uuid::from_u128(2), save("Alpha", "Chapter 9", true)),
            (Uuid::from_u128(3), save("Alpha", "Chapter 1", false)),
            (Uuid::from_u128(4), save("Alpha", "Chapter 2", false)),
        ]);
        let rows: Vec<_> = device_rows(&map)
            .into_iter()
            .map(|(_, d)| (d.manga_title, d.chapter_title))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Alpha".to_string(), "Chapter 1".to_string()),
                ("Alpha".to_string(), "Chapter 2".to_string()),
                ("Zeta".to_string(), "Chapter 2".to_string()),
                // The failed save sinks to the bottom of the block.
                ("Alpha".to_string(), "Chapter 9".to_string()),
            ]
        );
    }

    /// A HashMap iterates in an arbitrary order; the display order must not
    /// depend on it, or rows would shuffle under the user between polls.
    #[test]
    fn device_rows_are_stable_regardless_of_map_order() {
        let entries = [
            (Uuid::from_u128(10), save("B", "Chapter 1", false)),
            (Uuid::from_u128(11), save("A", "Chapter 1", false)),
            (Uuid::from_u128(12), save("C", "Chapter 1", false)),
        ];
        let forward: std::collections::HashMap<_, _> = entries.iter().cloned().collect();
        let backward: std::collections::HashMap<_, _> = entries.iter().rev().cloned().collect();
        assert_eq!(device_rows(&forward), device_rows(&backward));
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
fn QueueRow(
    entry: DownloadQueueEntry,
    /// Tag the row as the server's work. Only set in the mixed "In
    /// progress" block, where device and server rows sit side by side; the
    /// server-only queues below say so in their heading.
    #[prop(default = false)]
    where_server: bool,
) -> impl IntoView {
    let progress = entry.progress.clone();
    let error = match &entry.state {
        DownloadState::Failed { reason, .. } => Some(reason.clone()),
        _ => None,
    };
    // Deliberately not the error styling: the reason is information, not a
    // report of something broken.
    let unavailable = match &entry.state {
        DownloadState::Unavailable { reason, .. } => Some(reason.clone()),
        _ => None,
    };
    let is_unavailable = unavailable.is_some();
    view! {
        <li class="download-row" class:download-unavailable=is_unavailable>
            <div class="download-row-head">
                <a class="download-title" href=format!("/manga/{}", entry.publication_id)>
                    <strong>{entry.publication_title}</strong>
                    " · " {entry.unit_title}
                </a>
                {where_server.then(|| view! { <span class="download-where">"server"</span> })}
            </div>
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
            {unavailable
                .map(|reason| view! { <span class="muted download-reason">{reason}</span> })}
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
                <span class="download-where">"device"</span>
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
