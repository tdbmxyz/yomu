use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_domain::{Category, MangaWithPosition, UpdateCategoryRequest};

use crate::offline;
use crate::use_client;

#[component]
pub fn Library() -> impl IntoView {
    let client = use_client();
    let refresh = RwSignal::new(0u32);
    // Last-known-good fallbacks: without a service worker (Tauri shell)
    // the library stays browsable while the server is unreachable.
    let library = LocalResource::new({
        let client = client.clone();
        move || {
            refresh.track();
            let client = client.clone();
            async move { offline::with_cache("library", client.library().await) }
        }
    });
    let categories = LocalResource::new({
        let client = client.clone();
        move || {
            refresh.track();
            let client = client.clone();
            async move { offline::with_cache("categories", client.categories().await) }
        }
    });
    // None = "All".
    let selected = RwSignal::new(None::<String>);
    // In-library filters, applied client-side over the loaded list.
    let search = RwSignal::new(String::new());
    let active_genre = RwSignal::new(None::<String>);

    view! {
        <section>
            <h2>"Library"</h2>
            {move || {
                categories
                    .get()
                    .and_then(|r| r.ok())
                    .map(|list| {
                        let entries = library.get().and_then(|r| r.ok()).unwrap_or_default();
                        view! { <CategoryTabs list entries selected refresh/> }
                    })
            }}
            {move || {
                let entries = library.get().and_then(|r| r.ok()).unwrap_or_default();
                (!entries.is_empty())
                    .then(|| view! { <LibraryFilters entries search active_genre/> })
            }}
            {move || match library.get() {
                None => view! { <p class="muted">"Loading library…"</p> }.into_any(),
                Some(Ok(list)) if list.is_empty() => {
                    view! {
                        <p class="muted">
                            "Nothing tracked yet — use " <a href="/search">"Search"</a>
                            " or browse the " <a href="/sources">"Sources"</a> " catalogs."
                        </p>
                    }
                        .into_any()
                }
                Some(Ok(list)) => {
                    let client = use_client();
                    let needle = search.get().trim().to_lowercase();
                    let genre = active_genre.get();
                    let filtered: Vec<_> = list
                        .into_iter()
                        .filter(|entry| {
                            selected
                                .get()
                                .as_ref()
                                .is_none_or(|c| entry.manga.category == *c)
                        })
                        .filter(|entry| {
                            needle.is_empty()
                                || entry.manga.title.to_lowercase().contains(&needle)
                        })
                        .filter(|entry| {
                            genre
                                .as_ref()
                                .is_none_or(|g| entry.manga.genres.contains(g))
                        })
                        .collect();
                    if filtered.is_empty() {
                        return view! {
                            <p class="muted">"No manga match these filters."</p>
                        }
                            .into_any();
                    }
                    // Chapters saved on this device, grouped per manga
                    // (localStorage marks — a per-device notion, so counted
                    // client-side).
                    let device_counts: std::collections::HashMap<uuid::Uuid, u32> = {
                        let mut counts = std::collections::HashMap::new();
                        for mark in offline::device_chapters().values() {
                            *counts.entry(mark.manga).or_insert(0) += 1;
                        }
                        counts
                    };
                    view! {
                        <div class="manga-grid">
                            {filtered
                                .into_iter()
                                .map(|entry| {
                                    let cover = client.cover_url(entry.manga.id);
                                    let device = device_counts
                                        .get(&entry.manga.id)
                                        .copied()
                                        .unwrap_or(0);
                                    let meta = if entry.unread_count > 0 {
                                        format!("{} new", entry.unread_count)
                                    } else {
                                        format!(
                                            "{} chapter{}",
                                            entry.chapter_count,
                                            if entry.chapter_count == 1 { "" } else { "s" },
                                        )
                                    };
                                    let badge = (entry.unread_count > 0)
                                        .then(|| entry.unread_count.to_string());
                                    view! {
                                        <a
                                            class="manga-card"
                                            href=format!("/manga/{}", entry.manga.id)
                                        >
                                            <span class="cover-wrap">
                                                {cover
                                                    .map(|url| {
                                                        view! {
                                                            <img
                                                                class="manga-cover"
                                                                src=url.to_string()
                                                                loading="lazy"
                                                                alt=""
                                                            />
                                                        }
                                                    })}
                                                {badge
                                                    .map(|b| {
                                                        view! { <span class="unread-badge">{b}</span> }
                                                    })}
                                                {(entry.chapter_count > 0
                                                    || entry.downloaded_count > 0
                                                    || device > 0)
                                                    .then(|| {
                                                        view! {
                                                            <span class="count-strip">
                                                                {(entry.chapter_count > 0)
                                                                    .then(|| {
                                                                        view! { <span>{entry.chapter_count}</span> }
                                                                    })}
                                                                {(entry.downloaded_count > 0)
                                                                    .then(|| {
                                                                        view! {
                                                                            <span class="count-server">
                                                                                "↓" {entry.downloaded_count}
                                                                            </span>
                                                                        }
                                                                    })}
                                                                {(device > 0)
                                                                    .then(|| {
                                                                        view! {
                                                                            <span class="count-device">"↓" {device}</span>
                                                                        }
                                                                    })}
                                                            </span>
                                                        }
                                                    })}
                                            </span>
                                            <span class="manga-title">{entry.manga.title.clone()}</span>
                                            <span class="muted manga-meta">{meta}</span>
                                        </a>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
                Some(Err(err)) => {
                    view! { <p class="error">"Could not reach yomu server: " {err.to_string()}</p> }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Title search box + genre chips. Genres are the union across the loaded
/// library (client-side, so filtering stays instant and offline-friendly).
#[component]
fn LibraryFilters(
    entries: Vec<MangaWithPosition>,
    search: RwSignal<String>,
    active_genre: RwSignal<Option<String>>,
) -> impl IntoView {
    let mut genres: Vec<String> = entries
        .iter()
        .flat_map(|e| e.manga.genres.iter().cloned())
        .collect();
    genres.sort_by_key(|g| g.to_lowercase());
    genres.dedup();

    view! {
        <div class="library-filters">
            <input
                class="library-search"
                type="search"
                placeholder="Search library…"
                prop:value=search
                on:input=move |ev| search.set(event_target_value(&ev))
            />
            {(!genres.is_empty())
                .then(|| {
                    view! {
                        <div class="genre-chips">
                            {genres
                                .into_iter()
                                .map(|genre| {
                                    let g = genre.clone();
                                    let is_active = {
                                        let g = g.clone();
                                        move || active_genre.get().as_deref() == Some(g.as_str())
                                    };
                                    let toggle = {
                                        let g = g.clone();
                                        move |_| {
                                            active_genre
                                                .update(|cur| {
                                                    *cur = if cur.as_deref() == Some(g.as_str()) {
                                                        None
                                                    } else {
                                                        Some(g.clone())
                                                    };
                                                })
                                        }
                                    };
                                    view! {
                                        <button
                                            class="genre-chip"
                                            class:active=is_active
                                            on:click=toggle
                                        >
                                            {genre}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                })}
        </div>
    }
}

/// "All | Reading (3) | Paused (1) | Finished (2)" filter row; the active
/// category also exposes its updater toggle ("new-chapter checks: on/off").
#[component]
fn CategoryTabs(
    list: Vec<Category>,
    entries: Vec<MangaWithPosition>,
    selected: RwSignal<Option<String>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let count_of = move |id: &str| entries.iter().filter(|e| e.manga.category == id).count();

    // Reactive: appears when a category tab is active, reflects its flag.
    let toggle_update = {
        let list = list.clone();
        move || {
            let category = list
                .iter()
                .find(|c| Some(&c.id) == selected.get().as_ref())
                .cloned()?;
            let client = use_client();
            let label = if category.update_enabled {
                "new-chapter checks: on"
            } else {
                "new-chapter checks: off"
            };
            let on_click = move |_| {
                let client = client.clone();
                let req = UpdateCategoryRequest {
                    update_enabled: !category.update_enabled,
                };
                let id = category.id.clone();
                spawn_local(async move {
                    match client.update_category(&id, &req).await {
                        Ok(_) => refresh.update(|n| *n += 1),
                        Err(err) => leptos::logging::warn!("category update: {err}"),
                    }
                });
            };
            Some(view! {
                <button
                    class="category-update-toggle"
                    title="Include this category in the periodic new-chapter check"
                    on:click=on_click
                >
                    {label}
                </button>
            })
        }
    };

    view! {
        <div class="category-tabs">
            <button
                class:active=move || selected.get().is_none()
                on:click=move |_| selected.set(None)
            >
                "All"
            </button>
            {list
                .into_iter()
                .map(|category| {
                    let id = category.id.clone();
                    let is_active = {
                        let id = id.clone();
                        move || selected.get().as_deref() == Some(id.as_str())
                    };
                    let select_id = category.id.clone();
                    view! {
                        <button
                            class:active=is_active
                            on:click=move |_| selected.set(Some(select_id.clone()))
                        >
                            {format!("{} ({})", category.name, count_of(&id))}
                        </button>
                    }
                })
                .collect_view()}
            <span class="grow"></span>
            {toggle_update}
        </div>
    }
}
