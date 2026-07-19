use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use uuid::Uuid;
use yomu_domain::{Category, Chapter, DownloadState, MangaDetailResponse, UpdateMangaRequest};

use super::{NotFound, param_uuid};
use crate::offline;
use crate::use_client;

/// Which download tier a live progress ring belongs to (its color).
#[derive(Clone, Copy, PartialEq)]
enum ProgressTier {
    Server,
    Local,
}

/// What a row renders as its perimeter ring, sourced from either the
/// page-local server map or the app-level `LocalDownloads` store.
#[derive(Clone, Copy)]
struct RowProgress {
    done: u32,
    total: u32,
    tier: ProgressTier,
    failed: bool,
}

/// Server-tier per-chapter progress, page-local (polled from /downloads).
type ServerProgress = RwSignal<std::collections::HashMap<Uuid, (u32, u32)>>;

#[component]
pub fn MangaPage() -> impl IntoView {
    let Some(id) = param_uuid("id") else {
        return view! { <NotFound/> }.into_any();
    };

    let refresh = RwSignal::new(0u32);
    let status = RwSignal::new(None::<String>);
    // Chapter multi-select lives here, above the detail resource, so the
    // periodic refresh while downloads run doesn't wipe an ongoing selection.
    let selected = RwSignal::new(HashSet::<Uuid>::new());
    let anchor = RwSignal::new(None::<usize>);
    // Live download progress per chapter (both tiers), drawn by the rows.
    let server_progress: ServerProgress = RwSignal::new(std::collections::HashMap::new());
    let client = use_client();
    let conn = crate::use_connectivity();
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            refresh.track();
            conn.track();
            let client = client.clone();
            async move {
                // The flag marks the detail as served-from-cache (server
                // unreachable) so rows can show which chapters won't open.
                offline::cached(conn, &format!("manga:{id}"), || client.manga(id)).await
            }
        }
    });
    // Category select data, owned here rather than by MangaDetail: a
    // `refresh` bump recreates MangaDetail, and a resource created there
    // would yield None until its refetch lands — the select would
    // unmount for a beat every bump (visible flicker while downloads
    // animate). Which categories the updater checks is configured on
    // the library page.
    let categories = LocalResource::new({
        let client = client.clone();
        move || {
            conn.track();
            let client = client.clone();
            async move {
                offline::cached(conn, "categories", || client.categories())
                    .await
                    .map(|(value, _)| value)
            }
        }
    });

    // Coming back from the reader must land where the list was left, not
    // at the top. The browser can't restore the position itself: the page
    // is empty until the detail fetch resolves. The position is recorded
    // from scroll events as they happen — not in on_cleanup, which runs
    // after navigation has already reset the scroll to 0 (that reset's own
    // scroll event fires asynchronously, past the listener's removal, so
    // it can't clobber the recording).
    let scroll_key = format!("yomu-scroll:manga:{id}");
    {
        let key = scroll_key.clone();
        let save_handle = window_event_listener(leptos::ev::scroll, move |_| {
            if let Some(storage) = window().session_storage().ok().flatten() {
                let y = window().scroll_y().unwrap_or(0.0);
                let _ = storage.set_item(&key, &y.to_string());
            }
        });
        on_cleanup(move || save_handle.remove());
    }
    let restored = StoredValue::new(false);
    Effect::new(move |_| {
        if detail.get().and_then(|r| r.ok()).is_none() || restored.get_value() {
            return;
        }
        restored.set_value(true);
        let key = scroll_key.clone();
        request_animation_frame(move || {
            if let Some(storage) = window().session_storage().ok().flatten()
                && let Ok(Some(saved)) = storage.get_item(&key)
                && let Ok(y) = saved.parse::<f64>()
            {
                window().scroll_to_with_x_and_y(0.0, y);
            }
        });
    });

    // While a download is queued or running, keep refetching so the chapter
    // buttons flip to "downloaded" without a manual reload. At most one poll
    // is ever in flight: the effect re-runs on any `refresh` (a user clicking
    // Download/Refresh), and without this guard each such re-run would start
    // a second concurrent 2s chain. The handle is cleared on unmount so a
    // pending tick can't fire `refresh` on a disposed signal after navigation.
    let poll = StoredValue::new(None::<TimeoutHandle>);
    Effect::new(move |_| {
        let busy = detail.get().and_then(|r| r.ok()).is_some_and(|(d, _)| {
            d.chapters.iter().any(|c| {
                matches!(
                    c.download,
                    DownloadState::Pending | DownloadState::Downloading
                )
            })
        });
        if busy && poll.with_value(Option::is_none) {
            let handle = set_timeout_with_handle(
                move || {
                    poll.set_value(None);
                    refresh.update(|n| *n += 1);
                },
                std::time::Duration::from_millis(2000),
            )
            .ok();
            poll.set_value(handle);
            // Each tick also pulls the active download's page progress
            // (the server tracks it per page) into the rows' rings.
            if conn.get_untracked() == crate::Connectivity::Online {
                let client = use_client();
                spawn_local(async move {
                    let Ok(downloads) = client.downloads().await else {
                        return;
                    };
                    server_progress.update(|map| {
                        map.clear();
                        for entry in &downloads.queue {
                            if let Some(p) = &entry.progress {
                                map.insert(entry.chapter_id, (p.page, p.total));
                            }
                        }
                    });
                });
            }
        } else if !busy {
            // queue drained: no server ring should linger
            server_progress.update(|map| map.clear());
        }
    });
    on_cleanup(move || {
        if let Some(handle) = poll.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    });

    // The device-pull queue ("download both") is drained app-wide by the
    // background driver (see crate::pull), so it survives leaving this
    // page and app restarts — nothing to do here.

    view! {
        {move || match detail.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Ok((detail, offline))) => {
                view! { <MangaDetail detail offline refresh status selected anchor server_progress categories/> }.into_any()
            }
            Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
        }}
        {move || status.get().map(|s| view! { <p class="status">{s}</p> })}
    }
    .into_any()
}

#[component]
fn MangaDetail(
    detail: MangaDetailResponse,
    /// The detail came from the offline cache: the server is unreachable,
    /// so chapters not saved on this device won't open.
    offline: bool,
    refresh: RwSignal<u32>,
    status: RwSignal<Option<String>>,
    selected: RwSignal<HashSet<Uuid>>,
    anchor: RwSignal<Option<usize>>,
    server_progress: ServerProgress,
    /// Owned by MangaPage so refresh-driven remounts of this component
    /// re-render the select instantly from the already-loaded value.
    categories: LocalResource<Result<Vec<Category>, yomu_client::ClientError>>,
) -> impl IntoView {
    let client = use_client();
    let manga = detail.manga.clone();
    let id = manga.id;
    // "Continue" goes to the last known position — server's answer merged
    // with any unsynced offline events — or the first chapter.
    let position = offline::effective_position(id, detail.position.clone(), &offline::outbox());
    let continue_target = position
        .as_ref()
        .map(|p| (p.chapter_id, p.page))
        .or_else(|| detail.chapters.first().map(|c| (c.id, 0)));
    let continue_label = if position.is_some() {
        "Continue reading"
    } else {
        "Start reading"
    };

    let do_refresh = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            spawn_local(async move {
                match client.refresh_manga(id).await {
                    Ok(r) => {
                        status.set(Some(match r.new_chapters {
                            0 => "No new chapters".into(),
                            n => format!("{n} new chapter(s)"),
                        }));
                        refresh.update(|n| *n += 1);
                    }
                    Err(err) => status.set(Some(format!("Refresh failed: {err}"))),
                }
            });
        }
    };

    let auto_download = manga.auto_download;
    let toggle_auto = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            spawn_local(async move {
                let req = UpdateMangaRequest {
                    auto_download: !auto_download,
                    category: None,
                };
                match client.update_manga(id, &req).await {
                    Ok(_) => refresh.update(|n| *n += 1),
                    Err(err) => status.set(Some(format!("Update failed: {err}"))),
                }
            });
        }
    };

    let current_category = manga.category.clone();
    let set_category = {
        let client = client.clone();
        move |ev: leptos::ev::Event| {
            let value = event_target_value(&ev);
            let client = client.clone();
            spawn_local(async move {
                let req = UpdateMangaRequest {
                    auto_download,
                    category: Some(value),
                };
                match client.update_manga(id, &req).await {
                    Ok(_) => refresh.update(|n| *n += 1),
                    Err(err) => status.set(Some(format!("Update failed: {err}"))),
                }
            });
        }
    };

    let delete = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            spawn_local(async move {
                match client.delete_manga(id).await {
                    Ok(()) => {
                        let _ = window().location().set_href("/");
                    }
                    Err(err) => status.set(Some(format!("Delete failed: {err}"))),
                }
            });
        }
    };

    view! {
        <section class="manga-detail">
            <div class="manga-head">
                <crate::cover::Cover manga_id=id large=true/>
                <div class="manga-head-body">
                    <h2>{manga.title.clone()}</h2>
                    {manga
                        .description
                        .clone()
                        .map(|d| view! { <p class="muted">{d}</p> })}
                    <p class="muted">
                        "Source: " {manga.source_id.clone()} " · " {detail.chapters.len()}
                        " chapters"
                    </p>
                    <div class="manga-actions">
                        {continue_target
                            .map(|(chapter_id, page)| {
                                view! {
                                    <a
                                        class="button primary"
                                        href=format!("/read/{id}/{chapter_id}?page={page}")
                                    >
                                        {continue_label}
                                    </a>
                                }
                            })}
                        <button on:click=do_refresh>"Check for new chapters"</button>
                        <button on:click=toggle_auto>
                            {if auto_download {
                                "Auto-download: on"
                            } else {
                                "Auto-download: off"
                            }}
                        </button>
                        {move || {
                            let current = current_category.clone();
                            let on_change = set_category.clone();
                            categories
                                .get()
                                .and_then(|r| r.ok())
                                .map(|list: Vec<Category>| {
                                    view! {
                                        <select
                                            class="category-select"
                                            title="Category"
                                            on:change=on_change
                                        >
                                            {list
                                                .into_iter()
                                                .map(|c| {
                                                    let selected = c.id == current;
                                                    view! {
                                                        <option value=c.id selected=selected>
                                                            {c.name}
                                                        </option>
                                                    }
                                                })
                                                .collect_view()}
                                        </select>
                                    }
                                })
                        }}
                        <button class="danger" on:click=delete>
                            "Remove from library"
                        </button>
                    </div>
                </div>
            </div>
            <ChapterList
                manga_id=id
                chapters=detail.chapters
                offline
                position_chapter=position.map(|p| p.chapter_id)
                refresh
                status
                selected
                anchor
                server_progress
                manga_title=manga.title.clone()
            />
        </section>
    }
}

/// Save one chapter to this device (shell storage or, in the browser,
/// the service-worker cache), drawing per-page progress on the row's
/// ring, and record the mark. A failure flashes the ring red before the
/// row falls back to its previous state.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_locally(
    client: &yomu_client::YomuClient,
    manga_id: Uuid,
    manga_title: String,
    id: Uuid,
    chapter_title: String,
    local: crate::LocalDownloads,
    device_marks: crate::DeviceMarks,
) -> Result<(), String> {
    local.update(|map| {
        map.insert(
            id,
            crate::LocalDownload {
                manga_id,
                manga_title: manga_title.clone(),
                chapter_title: chapter_title.clone(),
                done: 0,
                total: 0,
                failed: false,
                cancel_requested: false,
            },
        );
    });
    let should_cancel =
        move || local.with_untracked(|m| m.get(&id).is_some_and(|d| d.cancel_requested));
    let result = offline::save_chapter_with_progress(
        client,
        id,
        |done, total| {
            local.update(|map| {
                if let Some(d) = map.get_mut(&id) {
                    d.done = done;
                    d.total = total;
                }
            });
        },
        should_cancel,
    )
    .await;
    match result {
        Ok(offline::SaveOutcome::Done(count)) => {
            offline::mark_device_chapter(manga_id, id, count);
            device_marks.update(|m| {
                m.insert(
                    id,
                    offline::DeviceMark {
                        manga: manga_id,
                        pages: count,
                    },
                );
            });
            local.update(|map| {
                map.remove(&id);
            });
            Ok(())
        }
        Ok(offline::SaveOutcome::Cancelled) => {
            local.update(|map| {
                map.remove(&id);
            });
            Ok(())
        }
        Err(err) => {
            local.update(|map| {
                if let Some(d) = map.get_mut(&id) {
                    d.failed = true;
                } else {
                    map.insert(
                        id,
                        crate::LocalDownload {
                            manga_id,
                            manga_title,
                            chapter_title,
                            done: 0,
                            total: 1,
                            failed: true,
                            cancel_requested: false,
                        },
                    );
                }
            });
            set_timeout(
                move || {
                    local.try_update(|map| {
                        map.remove(&id);
                    });
                },
                std::time::Duration::from_millis(1500),
            );
            Err(err)
        }
    }
}

#[component]
fn ChapterList(
    manga_id: Uuid,
    chapters: Vec<Chapter>,
    offline: bool,
    position_chapter: Option<Uuid>,
    refresh: RwSignal<u32>,
    status: RwSignal<Option<String>>,
    selected: RwSignal<HashSet<Uuid>>,
    anchor: RwSignal<Option<usize>>,
    server_progress: ServerProgress,
    manga_title: String,
) -> impl IntoView {
    // Display newest chapter first. Only the on-page list is reversed —
    // `list_chapters` stays in reading order (Chapter 1 → N), which the
    // reader relies on for prev/next navigation, the continuous strip, and
    // the server's prefix "mark read" logic. Reversing here keeps this
    // component's indices (ids, selection range, rendering) consistent
    // among themselves.
    let mut chapters: Vec<Chapter> = chapters.into_iter().rev().collect();
    // Read marks queued while offline overlay the server's answer, so
    // marking works (and shows) without a connection.
    let pending = offline::pending_marks();
    if !pending.is_empty() {
        for chapter in &mut chapters {
            if let Some(read) = pending.get(&chapter.id) {
                chapter.read = *read;
            }
        }
    }
    let ids = StoredValue::new(chapters.iter().map(|c| c.id).collect::<Vec<_>>());
    let titles = StoredValue::new(
        chapters
            .iter()
            .map(|c| (c.id, c.title.clone()))
            .collect::<std::collections::HashMap<Uuid, String>>(),
    );
    let manga_title = StoredValue::new(manga_title);
    let local_downloads = crate::use_local_downloads();
    let device_marks = crate::use_device_marks();
    let pull_queue = crate::use_pull_queue();
    // Storage state per display index, for the selection menu's matrix.
    let states = StoredValue::new({
        let device = offline::device_chapters();
        chapters
            .iter()
            .map(|c| crate::chapter_actions::ChapterState {
                on_server: matches!(c.download, DownloadState::Downloaded { .. }),
                on_device: device.contains_key(&c.id),
            })
            .collect::<Vec<_>>()
    });

    // Long-press: the first one starts a selection; while one is active,
    // long-pressing another row selects everything between it and the last
    // touched row ("select between").
    let press = Callback::new(move |index: usize| {
        let id = ids.with_value(|v| v[index]);
        selected.update(|s| match anchor.get_untracked() {
            Some(a) if !s.is_empty() => {
                let (lo, hi) = if a <= index { (a, index) } else { (index, a) };
                ids.with_value(|v| s.extend(v[lo..=hi].iter().copied()));
            }
            _ => {
                s.insert(id);
            }
        });
        anchor.set(Some(index));
    });
    // Plain tap while a selection is active toggles a single row.
    let toggle = Callback::new(move |index: usize| {
        let id = ids.with_value(|v| v[index]);
        selected.update(|s| {
            if !s.remove(&id) {
                s.insert(id);
            }
        });
        anchor.set(Some(index));
    });

    // "Select" from the menu enters selection mode before anything is
    // picked; a non-empty selection keeps it active either way.
    let forced_selection = RwSignal::new(false);
    let selection_active =
        Memo::new(move |_| forced_selection.get() || !selected.with(|s| s.is_empty()));

    // The selection's display indices and per-action id subsets.
    let selected_indices = move || {
        let picked = selected.get_untracked();
        ids.with_value(|v| {
            v.iter()
                .enumerate()
                .filter(|(_, id)| picked.contains(id))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        })
    };
    let ids_where = move |pred: fn(&crate::chapter_actions::ChapterState) -> bool| {
        let indices = selected_indices();
        states.with_value(|st| {
            ids.with_value(|v| {
                indices
                    .iter()
                    .filter(|i| pred(&st[**i]))
                    .map(|i| v[*i])
                    .collect::<Vec<Uuid>>()
            })
        })
    };
    let clear = move || {
        selected.set(HashSet::new());
        anchor.set(None);
        forced_selection.set(false);
    };
    let select_all = move |_| {
        selected.set(ids.with_value(|v| v.iter().copied().collect()));
    };

    // Marks work offline: a failed server call lands in the marks outbox
    // and the overlay shows it until the next flush syncs it.
    let mark_ids = move |mark_ids: Vec<Uuid>, read: bool| {
        if mark_ids.is_empty() {
            return;
        }
        let client = use_client();
        spawn_local(async move {
            if client.mark_chapters(&mark_ids, read).await.is_err() {
                offline::queue_marks(&mark_ids, read);
                status.set(Some("Marked offline — will sync when back online".into()));
            }
            refresh.update(|n| *n += 1);
        });
    };

    let caps = crate::chapter_actions::Caps {
        online: !offline,
        local_tier: offline::shell_available() || offline::service_worker_active(),
        local_remove: offline::shell_available(),
    };
    let menu_open = RwSignal::new(false);
    let run_action = move |action: crate::chapter_actions::Action| {
        use crate::chapter_actions::Action;
        menu_open.set(false);
        match action {
            Action::DownloadServer | Action::DownloadBoth => {
                // Bulk order: oldest chapter first. The list shows newest
                // first, so reverse the display-order selection.
                let mut dl = ids_where(|s| !s.on_server);
                dl.reverse();
                if action == Action::DownloadBoth {
                    // Every selected chapter not yet on this device is
                    // queued; the background driver (crate::pull) pulls each
                    // once its server download finishes — already-downloaded
                    // ones are pulled on its next tick.
                    let mut both = ids_where(|s| !s.on_device);
                    both.reverse();
                    let mtitle = manga_title.get_value();
                    let ctitles = titles.get_value();
                    pull_queue.update(|q| {
                        for id in both {
                            if !q.iter().any(|e| e.chapter_id == id) {
                                q.push(crate::PullItem {
                                    chapter_id: id,
                                    manga_id,
                                    manga_title: mtitle.clone(),
                                    chapter_title: ctitles.get(&id).cloned().unwrap_or_default(),
                                });
                            }
                        }
                    });
                }
                let client = use_client();
                spawn_local(async move {
                    match client.download_chapters(&dl).await {
                        Ok(r) => {
                            status.set(Some(match r.affected {
                                0 => "Nothing new to download".into(),
                                n => format!("{n} chapter(s) queued — downloads run one by one"),
                            }));
                            refresh.update(|n| *n += 1);
                        }
                        Err(err) => status.set(Some(format!("Download failed: {err}"))),
                    }
                });
            }
            Action::DownloadLocal => {
                let mut pull = ids_where(|s| s.on_server && !s.on_device);
                pull.reverse(); // oldest chapter first
                let client = use_client();
                let mtitle = manga_title.get_value();
                let ctitles = titles.get_value();
                spawn_local(async move {
                    for id in pull {
                        let ct = ctitles.get(&id).cloned().unwrap_or_default();
                        if let Err(err) = save_locally(
                            &client,
                            manga_id,
                            mtitle.clone(),
                            id,
                            ct,
                            local_downloads,
                            device_marks,
                        )
                        .await
                        {
                            status.set(Some(format!("Local save failed: {err}")));
                            leptos::logging::warn!("local download: {err}");
                        }
                    }
                    refresh.update(|n| *n += 1);
                });
            }
            Action::RemoveServer => {
                let rm = ids_where(|s| s.on_server);
                let client = use_client();
                spawn_local(async move {
                    match client.remove_downloads(&rm).await {
                        Ok(r) => {
                            status.set(Some(format!("{} server download(s) removed", r.affected)));
                            refresh.update(|n| *n += 1);
                        }
                        Err(err) => status.set(Some(format!("Remove failed: {err}"))),
                    }
                });
            }
            Action::RemoveLocal => {
                let rm = ids_where(|s| s.on_device);
                spawn_local(async move {
                    for id in rm {
                        match offline::shell_delete_chapter(id).await {
                            Ok(()) => {
                                offline::unmark_device_chapter(id);
                                device_marks.update(|m| {
                                    m.remove(&id);
                                });
                            }
                            Err(err) => leptos::logging::warn!("local remove: {err}"),
                        }
                    }
                    refresh.update(|n| *n += 1);
                });
            }
            Action::MarkRead => mark_ids(ids_where(|_| true), true),
            Action::MarkUnread => mark_ids(ids_where(|_| true), false),
            // The list displays newest first, so "before" (older) means
            // HIGHER display indices and "after" (newer) means lower.
            Action::MarkBeforeRead => {
                if let Some(max) = selected_indices().into_iter().max() {
                    let older = ids.with_value(|v| v[max + 1..].to_vec());
                    mark_ids(older, true);
                }
            }
            Action::MarkAfterUnread => {
                if let Some(min) = selected_indices().into_iter().min() {
                    let newer = ids.with_value(|v| v[..min].to_vec());
                    mark_ids(newer, false);
                }
            }
        }
        clear();
    };

    view! {
        <div class="chapter-list-head">
            <span class="grow"></span>
            <button
                class="icon-btn chapter-menu-btn"
                title="Chapter actions"
                on:click=move |_| menu_open.update(|open| *open = !*open)
            >
                "⋮"
            </button>
            <Show when=move || menu_open.get()>
                <div class="chapter-menu">
                    {move || {
                        if !selection_active.get() {
                            view! {
                                <button on:click=move |_| {
                                    forced_selection.set(true);
                                    menu_open.set(false);
                                }>"Select"</button>
                            }
                                .into_any()
                        } else {
                            // Entries from the union of the selected
                            // chapters' states; each action only touches
                            // the chapters it applies to.
                            selected.track();
                            let sel_states = {
                                let indices = selected_indices();
                                states
                                    .with_value(|st| {
                                        indices.iter().map(|i| st[*i]).collect::<Vec<_>>()
                                    })
                            };
                            crate::chapter_actions::menu_actions(&sel_states, caps)
                                .into_iter()
                                .map(|action| {
                                    view! {
                                        <button on:click=move |_| run_action(
                                            action,
                                        )>{action.label()}</button>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }
                    }}
                </div>
            </Show>
        </div>
        <ul class="chapter-list">
            {chapters
                .into_iter()
                .enumerate()
                .map(|(index, chapter)| {
                    let current = position_chapter == Some(chapter.id);
                    view! {
                        <ChapterItem
                            manga_id
                            chapter
                            offline
                            current
                            index
                            selected
                            selection_active
                            press
                            toggle
                            server_progress
                        />
                    }
                })
                .collect_view()}
        </ul>
        <Show when=move || selection_active.get()>
            <div class="select-bar">
                <span class="select-count">{move || selected.with(|s| s.len())}</span>
                <button on:click=select_all>"All"</button>
                <button title="Clear selection" on:click=move |_| clear()>
                    "✕"
                </button>
            </div>
        </Show>
    }
}

#[component]
fn ChapterItem(
    manga_id: Uuid,
    chapter: Chapter,
    offline: bool,
    current: bool,
    index: usize,
    selected: RwSignal<HashSet<Uuid>>,
    selection_active: Memo<bool>,
    press: Callback<usize>,
    toggle: Callback<usize>,
    server_progress: ServerProgress,
) -> impl IntoView {
    let id = chapter.id;
    let read = chapter.read;
    let local_downloads = crate::use_local_downloads();
    let row_progress = move || {
        if let Some(d) = local_downloads.with(|m| m.get(&id).cloned()) {
            Some(RowProgress {
                done: d.done,
                total: d.total,
                tier: ProgressTier::Local,
                failed: d.failed,
            })
        } else {
            server_progress
                .with(|m| m.get(&id).copied())
                .map(|(done, total)| RowProgress {
                    done,
                    total,
                    tier: ProgressTier::Server,
                    failed: false,
                })
        }
    };
    let is_selected = Memo::new(move |_| selected.with(|s| s.contains(&id)));

    // Long-press detection: a primary pointer held ~500ms without moving.
    // Buttons inside the row are exempt so holding "download" can't start a
    // selection by accident.
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let long_fired = StoredValue::new(false);
    let start = StoredValue::new((0.0f64, 0.0f64));
    let cancel = move || {
        if let Some(handle) = timer.try_update_value(|t| t.take()).flatten() {
            handle.clear();
        }
    };
    let on_target_button = |ev: &leptos::ev::PointerEvent| {
        use leptos::wasm_bindgen::JsCast;
        ev.target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| el.closest("button").ok().flatten().is_some())
    };
    let pointer_down = move |ev: leptos::ev::PointerEvent| {
        if ev.button() != 0 || on_target_button(&ev) {
            return;
        }
        long_fired.set_value(false);
        start.set_value((ev.client_x() as f64, ev.client_y() as f64));
        cancel();
        let handle = set_timeout_with_handle(
            move || {
                timer.set_value(None);
                long_fired.set_value(true);
                press.run(index);
            },
            std::time::Duration::from_millis(500),
        )
        .ok();
        timer.set_value(handle);
    };
    let pointer_move = move |ev: leptos::ev::PointerEvent| {
        let (x, y) = start.get_value();
        let moved = (ev.client_x() as f64 - x).abs() + (ev.client_y() as f64 - y).abs();
        if moved > 12.0 {
            cancel();
        }
    };
    let click = move |ev: leptos::ev::MouseEvent| {
        // The click that ends a long press must not navigate or re-toggle.
        if long_fired.try_update_value(std::mem::take) == Some(true) {
            ev.prevent_default();
            ev.stop_propagation();
            return;
        }
        if selection_active.get_untracked() {
            ev.prevent_default();
            toggle.run(index);
        }
    };
    let context_menu = move |ev: leptos::ev::MouseEvent| {
        // Android fires contextmenu on long press; keep it from covering
        // our selection. Desktop right-click (no pending press) still works.
        if timer.with_value(|t| t.is_some()) || selection_active.get_untracked() {
            ev.prevent_default();
        }
    };

    // Storage state, shown as the row's outline color (blue = on the
    // server, green = on this device, split = both) instead of the old
    // per-tier buttons; actions live in the selection menu.
    let device_marks = crate::use_device_marks();
    let on_device = move || device_marks.with(|m| m.contains_key(&id));
    let on_server = matches!(chapter.download, DownloadState::Downloaded { .. });
    let dl_busy = matches!(
        chapter.download,
        DownloadState::Pending | DownloadState::Downloading
    );
    let dl_failed = matches!(chapter.download, DownloadState::Failed { .. });
    let failed_reason = match &chapter.download {
        DownloadState::Failed { reason, .. } => Some(format!("Download failed: {reason}")),
        _ => None,
    };

    view! {
        <li
            class="chapter-item"
            class:current=current
            class:read=read
            class:selected=move || is_selected.get()
            // Served from the offline cache: chapters that aren't on this
            // device can't open until the server is reachable again.
            class:unavailable=move || offline && !on_device()
            class:dl-server=move || on_server && !on_device()
            class:dl-local=move || on_device() && !on_server
            class:dl-both=move || on_server && on_device()
            class:dl-busy=dl_busy
            class:dl-active=move || row_progress().is_some()
            class:dl-failed=dl_failed
            title=move || {
                if offline && !on_device() {
                    Some("Not available offline".to_string())
                } else {
                    failed_reason.clone()
                }
            }
            on:pointerdown=pointer_down
            on:pointermove=pointer_move
            on:pointerup=move |_| cancel()
            on:pointercancel=move |_| cancel()
            on:pointerleave=move |_| cancel()
            on:click=click
            on:contextmenu=context_menu
        >
            // Live download progress: a stroke tracing the row's border
            // clockwise from the top-left, proportional to pages done —
            // green for device saves, blue for server downloads.
            {move || {
                row_progress()
                    .map(|p| {
                        let pct = if p.total > 0 {
                            f64::from(p.done) / f64::from(p.total) * 100.0
                        } else {
                            0.0
                        };
                        let class = match (p.failed, p.tier) {
                            (true, _) => "dl-ring ring-failed",
                            (false, ProgressTier::Local) => "dl-ring ring-local",
                            (false, ProgressTier::Server) => "dl-ring ring-server",
                        };
                        view! {
                            <svg class=class aria-hidden="true">
                                <rect
                                    pathLength="100"
                                    stroke-dasharray=format!("{pct:.1} 100")
                                ></rect>
                            </svg>
                        }
                    })
            }}
            <Show when=move || selection_active.get()>
                <span class="select-box" aria-hidden="true">
                    {move || if is_selected.get() { "☑" } else { "☐" }}
                </span>
            </Show>
            <a class="chapter-title" href=format!("/read/{manga_id}/{id}")>
                {chapter.title.clone()}
            </a>
            {chapter
                .published_at
                .map(|at| {
                    view! {
                        <span class="muted chapter-date">
                            {crate::format::published_label(at, chrono::Utc::now())}
                        </span>
                    }
                })}
            <span class="grow"></span>
        </li>
    }
}
