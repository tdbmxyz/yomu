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
    let client = use_client();
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            refresh.track();
            let client = client.clone();
            async move { client.manga(id).await }
        }
    });

    // While a download is queued or running, keep refetching so the chapter
    // buttons flip to "downloaded" without a manual reload. Each completed
    // fetch schedules at most one follow-up, so this stops by itself.
    Effect::new(move |_| {
        let busy = detail.get().and_then(|r| r.ok()).is_some_and(|d| {
            d.chapters.iter().any(|c| {
                matches!(
                    c.download,
                    DownloadState::Pending | DownloadState::Downloading
                )
            })
        });
        if busy {
            set_timeout(
                move || refresh.update(|n| *n += 1),
                std::time::Duration::from_millis(2000),
            );
        }
    });

    view! {
        {move || match detail.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Ok(detail)) => view! { <MangaDetail detail refresh status/> }.into_any(),
            Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
        }}
        {move || status.get().map(|s| view! { <p class="status">{s}</p> })}
    }
    .into_any()
}

#[component]
fn MangaDetail(
    detail: MangaDetailResponse,
    refresh: RwSignal<u32>,
    status: RwSignal<Option<String>>,
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
                position_chapter=position.map(|p| p.chapter_id)
                refresh
            />
        </section>
    }
}

#[component]
fn ChapterList(
    manga_id: Uuid,
    chapters: Vec<Chapter>,
    position_chapter: Option<Uuid>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    view! {
        <ul class="chapter-list">
            {chapters
                .into_iter()
                .map(|chapter| {
                    let current = position_chapter == Some(chapter.id);
                    view! { <ChapterItem manga_id chapter current refresh/> }
                })
                .collect_view()}
        </ul>
    }
}

#[component]
fn ChapterItem(
    manga_id: Uuid,
    chapter: Chapter,
    current: bool,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let client = use_client();
    let id = chapter.id;

    let (download_label, downloadable) = match &chapter.download {
        DownloadState::None => ("download", true),
        DownloadState::Pending => ("queued…", false),
        DownloadState::Downloading => ("downloading…", false),
        DownloadState::Downloaded { .. } => ("downloaded", false),
        DownloadState::Failed { .. } => ("retry download", true),
    };
    let failed_reason = match &chapter.download {
        DownloadState::Failed { reason, .. } => Some(reason.clone()),
        _ => None,
    };

    let download = move |_| {
        let client = client.clone();
        spawn_local(async move {
            match client.download_chapter(id).await {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(err) => leptos::logging::warn!("download: {err}"),
            }
        });
    };

    // "On this device": walk every page through fetch so the service worker
    // caches it; afterwards the chapter reads fully offline.
    let on_device = RwSignal::new(offline::device_chapters().contains(&id));
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
                let result = offline::prefetch_chapter(&client, id).await;
                device_busy.set(false);
                match result {
                    Ok(()) => {
                        offline::mark_device_chapter(id);
                        on_device.set(true);
                    }
                    Err(err) => leptos::logging::warn!("device download: {err}"),
                }
            });
        }
    };

    view! {
        <li class="chapter-item" class:current=current>
            <a class="chapter-title" href=format!("/read/{manga_id}/{id}")>
                {chapter.title.clone()}
            </a>
            {chapter
                .page_count
                .map(|c| view! { <span class="muted">{c} " p."</span> })}
            {failed_reason
                .map(|reason| {
                    view! {
                        <span class="error" title=reason>
                            "✕"
                        </span>
                    }
                })}
            <span class="grow"></span>
            <button
                title="Store on this device for offline reading"
                disabled=move || device_busy.get() || on_device.get()
                on:click=device_download
            >
                {move || {
                    if on_device.get() {
                        "on device ✓"
                    } else if device_busy.get() {
                        "saving…"
                    } else {
                        "save to device"
                    }
                }}
            </button>
            <button disabled=!downloadable on:click=download>
                {download_label}
            </button>
        </li>
    }
}
