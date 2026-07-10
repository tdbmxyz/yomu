use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use uuid::Uuid;
use yomu_domain::{Category, Chapter, DownloadState, MangaDetailResponse, UpdateMangaRequest};

use super::{NotFound, param_uuid};
use crate::offline;
use crate::use_client;

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
    let client = use_client();
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            refresh.track();
            let client = client.clone();
            async move {
                // The flag marks the detail as served-from-cache (server
                // unreachable) so rows can show which chapters won't open.
                offline::with_cache_flagged(&format!("manga:{id}"), client.manga(id).await)
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
        }
    });
    on_cleanup(move || {
        if let Some(handle) = poll.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    });

    view! {
        {move || match detail.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Ok((detail, offline))) => {
                view! { <MangaDetail detail offline refresh status selected anchor/> }.into_any()
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
) -> impl IntoView {
    let client = use_client();
    let manga = detail.manga.clone();
    let id = manga.id;
    let cover = client.cover_url(id);

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

    // Category select (Reading / Paused / Finished …); which categories the
    // updater checks is configured on the library page.
    let categories = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.categories().await }
        }
    });
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
                {cover
                    .map(|url| {
                        view! { <img class="manga-cover large" src=url.to_string() alt=""/> }
                    })}
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
            />
        </section>
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
) -> impl IntoView {
    // Display newest chapter first. Only the on-page list is reversed —
    // `list_chapters` stays in reading order (Chapter 1 → N), which the
    // reader relies on for prev/next navigation, the continuous strip, and
    // the server's prefix "mark read" logic. Reversing here keeps this
    // component's indices (ids, selection range, rendering) consistent
    // among themselves.
    let chapters: Vec<Chapter> = chapters.into_iter().rev().collect();
    let ids = StoredValue::new(chapters.iter().map(|c| c.id).collect::<Vec<_>>());

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

    let selection_active = Memo::new(move |_| !selected.with(|s| s.is_empty()));

    // Bulk actions run on the selection in reading order.
    let selection_ids = move || {
        let picked = selected.get_untracked();
        ids.with_value(|v| {
            v.iter()
                .filter(|id| picked.contains(id))
                .copied()
                .collect::<Vec<_>>()
        })
    };
    let clear = move || {
        selected.set(HashSet::new());
        anchor.set(None);
    };
    let download_selected = move |_| {
        let ids = selection_ids();
        let client = use_client();
        spawn_local(async move {
            match client.download_chapters(&ids).await {
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
        clear();
    };
    let mark = move |read: bool| {
        let ids = selection_ids();
        let client = use_client();
        spawn_local(async move {
            match client.mark_chapters(&ids, read).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(err) => status.set(Some(format!("Mark failed: {err}"))),
            }
        });
        clear();
    };
    let select_all = move |_| {
        selected.set(ids.with_value(|v| v.iter().copied().collect()));
    };

    view! {
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
                            refresh
                            index
                            selected
                            selection_active
                            press
                            toggle
                        />
                    }
                })
                .collect_view()}
        </ul>
        <Show when=move || selection_active.get()>
            <div class="select-bar">
                <span class="select-count">{move || selected.with(|s| s.len())}</span>
                <button on:click=select_all>"All"</button>
                <button on:click=download_selected>"Download"</button>
                <button on:click=move |_| mark(true)>"Read"</button>
                <button on:click=move |_| mark(false)>"Unread"</button>
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
    refresh: RwSignal<u32>,
    index: usize,
    selected: RwSignal<HashSet<Uuid>>,
    selection_active: Memo<bool>,
    press: Callback<usize>,
    toggle: Callback<usize>,
) -> impl IntoView {
    let client = use_client();
    let id = chapter.id;
    let read = chapter.read;
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

    // Tachidesk-style state icons: one arrow-in-a-circle per storage tier,
    // told apart by color (accent = on the server, green = on this device).
    let (server_glyph, server_title, downloadable) = match &chapter.download {
        DownloadState::None => ("↓", "Download to the server".to_string(), true),
        DownloadState::Pending => ("↻", "Queued for download…".to_string(), false),
        DownloadState::Downloading => ("↻", "Downloading…".to_string(), false),
        DownloadState::Downloaded { .. } => ("✓", "On the server".to_string(), false),
        DownloadState::Failed { reason, .. } => {
            ("!", format!("Download failed: {reason} — retry"), true)
        }
    };
    let server_busy = matches!(
        chapter.download,
        DownloadState::Pending | DownloadState::Downloading
    );
    let server_done = matches!(chapter.download, DownloadState::Downloaded { .. });
    let server_failed = matches!(chapter.download, DownloadState::Failed { .. });

    let download = move |_| {
        let client = client.clone();
        spawn_local(async move {
            match client.download_chapter(id).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(err) => leptos::logging::warn!("download: {err}"),
            }
        });
    };

    // "On this device": in the browser, walk every page through fetch so
    // the service worker caches it; in the Tauri shell, have the shell
    // download the pages to disk. Either way the chapter then reads with
    // the server unreachable.
    let on_device = RwSignal::new(offline::device_chapters().contains_key(&id));
    let device_busy = RwSignal::new(false);
    let device_download = {
        let client = use_client();
        move |_| {
            if device_busy.get_untracked() || on_device.get_untracked() {
                return;
            }
            device_busy.set(true);
            let client = client.clone();
            spawn_local(async move {
                let result = if offline::shell_available() {
                    offline::shell_save_chapter(&client, id).await
                } else {
                    offline::prefetch_chapter(&client, id).await
                };
                device_busy.set(false);
                match result {
                    Ok(page_count) => {
                        offline::mark_device_chapter(manga_id, id, page_count);
                        on_device.set(true);
                    }
                    Err(err) => leptos::logging::warn!("device download: {err}"),
                }
            });
        }
    };

    view! {
        <li
            class="chapter-item"
            class:current=current
            class:read=read
            class:selected=move || is_selected.get()
            // Served from the offline cache: chapters that aren't on this
            // device can't open until the server is reachable again.
            class:unavailable=move || offline && !on_device.get()
            title=move || {
                (offline && !on_device.get()).then_some("Not available offline")
            }
            on:pointerdown=pointer_down
            on:pointermove=pointer_move
            on:pointerup=move |_| cancel()
            on:pointercancel=move |_| cancel()
            on:pointerleave=move |_| cancel()
            on:click=click
            on:contextmenu=context_menu
        >
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
            <Show when=move || !selection_active.get()>
                <button
                    class="icon-btn device-dl"
                    class:done=move || on_device.get()
                    class:busy=move || device_busy.get()
                    title=move || {
                        if on_device.get() {
                            "Saved on this device"
                        } else {
                            "Store on this device for offline reading"
                        }
                    }
                    disabled=move || device_busy.get() || on_device.get()
                    on:click=device_download.clone()
                >
                    {move || {
                        if on_device.get() {
                            "✓"
                        } else if device_busy.get() {
                            "↻"
                        } else {
                            "↓"
                        }
                    }}
                </button>
                <button
                    class="icon-btn server-dl"
                    class:done=server_done
                    class:busy=server_busy
                    class:failed=server_failed
                    title=server_title.clone()
                    disabled=!downloadable
                    on:click=download.clone()
                >
                    {server_glyph}
                </button>
            </Show>
        </li>
    }
}
