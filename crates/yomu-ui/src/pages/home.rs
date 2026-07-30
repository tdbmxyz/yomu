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
    let refresh = RwSignal::new(0u32);
    let library = crate::cache::keep_alive(crate::cache::use_library_cache(), (), refresh, conn, {
        let client = client.clone();
        move || {
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
            if let Some(entries) = library.value.get() {
                let ids = entries.iter().map(|entry| entry.publication.id).collect();
                crate::cover::sweep_device_covers(conn, &sweep_client, ids);
            }
        });
    }

    crate::cache::remember_scroll("yomu-scroll:home".to_string(), move || {
        library.value.get().is_some()
    });
    let pull = crate::refresh::use_pull_to_refresh(move || refresh.update(|n| *n += 1));
    // Any settled outcome ends the spinner — a refresh that fails must not
    // leave it turning.
    Effect::new(move |_| {
        let _ = (library.value.get(), library.error.get());
        pull.refreshing.set(false);
    });

    view! {
        <section class="home">
            <div
                class="pull-refresh"
                class:armed=move || pull.armed.get()
                class:spinning=move || pull.refreshing.get()
                style:height=move || format!("{}px", pull.distance.get())
            >
                <span class="pull-refresh-dot"></span>
            </div>
            // A failed refresh is shown beside the shelves, never instead
            // of them.
            {move || {
                library
                    .error
                    .get()
                    .map(|err| {
                        view! { <p class="error">"Could not reach yomu server: " {err}</p> }
                    })
            }}
            {move || match library.value.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(list) if list.is_empty() => {
                    view! {
                        <p class="muted gate-msg">
                            "Nothing tracked yet — use " <a href="/search">"Search"</a>
                            " or browse the " <a href="/sources">"Sources"</a> " catalogs."
                        </p>
                    }
                        .into_any()
                }
                Some(list) => {
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

                    let fresh = fresh_shelf(&list, offline::library_category().as_deref());
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

/// The "New chapters" shelf: publications with something unread, newest
/// first, capped at a shelf's worth.
///
/// Restricted to the category the reader picked on the Library tab. Without
/// that, a bulk import of local files — dozens of publications, every unit
/// unread, all stamped with the import instant — takes every slot and the
/// shelf stops answering "what landed recently?".
fn fresh_shelf(
    list: &[PublicationWithLocator],
    category: Option<&str>,
) -> Vec<PublicationWithLocator> {
    let mut fresh: Vec<PublicationWithLocator> = list
        .iter()
        .filter(|e| e.unread_count > 0)
        .filter(|e| category.is_none_or(|c| e.publication.category == c))
        .cloned()
        .collect();
    fresh.sort_by_key(|e| std::cmp::Reverse(e.latest_unit_at));
    fresh.truncate(12);
    fresh
}

#[cfg(test)]
mod tests {
    use super::fresh_shelf;
    use chrono::{TimeZone, Utc};
    use yomu_domain::{Kind, Origin, Publication, PublicationWithLocator};

    fn entry(title: &str, category: &str, unread: u32, day: u32) -> PublicationWithLocator {
        PublicationWithLocator {
            publication: Publication {
                id: uuid::Uuid::from_u128(day as u128 * 1000 + unread as u128),
                kind: Kind::Comics,
                origin: Origin::LocalFile { path: title.into() },
                title: title.into(),
                description: None,
                cover_url: None,
                auto_download: false,
                category: category.into(),
                genres: Vec::new(),
                added_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                last_checked_at: None,
                missing_since: None,
                unsupported_count: 0,
                unsupported_formats: Vec::new(),
            },
            unit_count: 10,
            unread_count: unread,
            downloaded_count: 0,
            latest_unit_at: Some(Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap()),
            locator: None,
            locator_unit_title: None,
        }
    }

    /// The case that motivated the filter: a bulk import stamped with today's
    /// date must not push the one series the reader follows off the shelf.
    #[test]
    fn the_shelf_honours_the_selected_category() {
        let mut list = vec![entry("Followed", "reading", 3, 1)];
        for n in 0..20 {
            list.push(entry(&format!("Imported {n}"), "finished", 40, 28));
        }

        let all = fresh_shelf(&list, None);
        assert_eq!(all.len(), 12);
        assert!(
            !all.iter().any(|e| e.publication.title == "Followed"),
            "unfiltered, the import crowds the followed series out"
        );

        let reading = fresh_shelf(&list, Some("reading"));
        assert_eq!(reading.len(), 1);
        assert_eq!(reading[0].publication.title, "Followed");
    }

    #[test]
    fn caught_up_publications_never_reach_the_shelf() {
        let list = vec![
            entry("Read", "reading", 0, 1),
            entry("New", "reading", 2, 2),
        ];
        let shelf = fresh_shelf(&list, Some("reading"));
        assert_eq!(shelf.len(), 1);
        assert_eq!(shelf[0].publication.title, "New");
    }
}
