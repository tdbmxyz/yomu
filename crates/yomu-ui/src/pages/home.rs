//! Home: horizontal shelves answering "what do I read now?" — continue
//! reading, new chapters, chapters saved on this device. The full grid
//! lives on the Library tab.

use leptos::prelude::*;
use yomu_domain::PublicationWithLocator;

use crate::offline;
use crate::use_client;

#[component]
pub fn Home() -> impl IntoView {
    let client = use_client();
    // Same resource + last-known-good cache as the Library page, so both
    // tabs work offline in the shell.
    let conn = crate::use_connectivity();
    let library = LocalResource::new({
        let client = client.clone();
        move || {
            conn.track();
            let client = client.clone();
            async move {
                offline::cached(conn, "library", || client.library())
                    .await
                    .map(|(value, _)| value)
            }
        }
    });
    // The shells land here: store any missing library covers for offline
    // (see cover::sweep_device_covers; the Library page does the same).
    {
        let sweep_client = client.clone();
        Effect::new(move |_| {
            if let Some(Ok(entries)) = library.get() {
                let ids = entries.iter().map(|entry| entry.publication.id).collect();
                crate::cover::sweep_device_covers(conn, &sweep_client, ids);
            }
        });
    }

    view! {
        <section class="home">
            {move || match library.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(Err(err)) => {
                    view! { <p class="error">"Could not reach yomu server: " {err.to_string()}</p> }
                        .into_any()
                }
                Some(Ok(list)) if list.is_empty() => {
                    view! {
                        <p class="muted gate-msg">
                            "Nothing tracked yet — use " <a href="/search">"Search"</a>
                            " or browse the " <a href="/sources">"Sources"</a> " catalogs."
                        </p>
                    }
                        .into_any()
                }
                Some(Ok(list)) => {
                    // Finished titles (nothing unread) drop off — there is
                    // nothing to continue; they return when a new chapter lands.
                    let mut resume: Vec<PublicationWithLocator> = list
                        .iter()
                        .filter(|e| e.locator.is_some() && e.unread_count > 0)
                        .cloned()
                        .collect();
                    resume.sort_by(|a, b| {
                        let at = |e: &PublicationWithLocator| e.locator.as_ref().map(|p| p.at);
                        at(b).cmp(&at(a))
                    });
                    resume.truncate(12);
                    let resume_cards: Vec<AnyView> = resume
                        .into_iter()
                        .map(|entry| {
                            let locator = entry.locator.clone().expect("filtered");
                            let subtitle = entry
                                .locator_unit_title
                                .clone()
                                .map(|t| format!("{t} · p. {}", locator.page() + 1))
                                .unwrap_or_else(|| format!("p. {}", locator.page() + 1));
                            view! {
                                <ShelfCard
                                    entry=entry
                                    href_chapter=Some((locator.unit_id, locator.page()))
                                    subtitle=subtitle
                                    badge=None
                                />
                            }
                                .into_any()
                        })
                        .collect();

                    let mut fresh: Vec<PublicationWithLocator> =
                        list.iter().filter(|e| e.unread_count > 0).cloned().collect();
                    fresh.sort_by_key(|e| std::cmp::Reverse(e.latest_unit_at));
                    fresh.truncate(12);
                    let fresh_cards: Vec<AnyView> = fresh
                        .into_iter()
                        .map(|entry| {
                            let badge = format!("+{}", entry.unread_count);
                            let subtitle = format!(
                                "{} chapter{}",
                                entry.unit_count,
                                if entry.unit_count == 1 { "" } else { "s" },
                            );
                            view! {
                                <ShelfCard
                                    entry=entry
                                    href_chapter=None
                                    subtitle=subtitle
                                    badge=Some(badge)
                                />
                            }
                                .into_any()
                        })
                        .collect();

                    let marks = offline::device_manga();
                    let device_cards: Vec<AnyView> = list
                        .iter()
                        .filter_map(|e| marks.get(&e.publication.id).map(|n| (e.clone(), *n)))
                        .map(|(entry, saved)| {
                            let subtitle = format!(
                                "{saved} chapter{} saved",
                                if saved == 1 { "" } else { "s" },
                            );
                            view! {
                                <ShelfCard
                                    entry=entry
                                    href_chapter=None
                                    subtitle=subtitle
                                    badge=None
                                />
                            }
                                .into_any()
                        })
                        .collect();

                    view! {
                        {shelf(
                            "Continue reading",
                            "Nothing in progress — pick something below.",
                            resume_cards,
                        )}
                        {shelf("New chapters", "All caught up.", fresh_cards)}
                        {(!device_cards.is_empty())
                            .then(|| shelf("On this device", "", device_cards))}
                        <p class="home-more">
                            <a href="/library">"Whole library →"</a>
                        </p>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

fn shelf(title: &'static str, empty: &'static str, cards: Vec<AnyView>) -> impl IntoView {
    view! {
        <div class="shelf">
            <h3 class="shelf-title">{title}</h3>
            {if cards.is_empty() {
                view! { <p class="muted shelf-empty">{empty}</p> }.into_any()
            } else {
                view! { <div class="shelf-row">{cards}</div> }.into_any()
            }}
        </div>
    }
}

/// One cover card in a shelf. `href_chapter` links straight into the reader
/// (resume); otherwise the card opens the manga page.
#[component]
fn ShelfCard(
    entry: PublicationWithLocator,
    href_chapter: Option<(uuid::Uuid, u32)>,
    subtitle: String,
    badge: Option<String>,
) -> impl IntoView {
    let id = entry.publication.id;
    let href = match href_chapter {
        Some((chapter, page)) => format!("/read/{id}/{chapter}?page={page}"),
        None => format!("/manga/{id}"),
    };
    view! {
        <a class="shelf-card" href=href>
            <span class="cover-wrap">
                <crate::cover::Cover manga_id=id/>
                {badge.map(|b| view! { <span class="unread-badge">{b}</span> })}
            </span>
            <span class="manga-title">{entry.publication.title.clone()}</span>
            <span class="muted manga-meta">{subtitle}</span>
        </a>
    }
}
