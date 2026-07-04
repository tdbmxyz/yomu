mod library;
mod manga;
mod reader;
mod search;

pub use library::Library;
pub use manga::MangaPage;
pub use reader::Reader;
pub use search::Search;

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;

/// Parse a UUID route param; renders the error case as a simple message.
pub(crate) fn param_uuid(name: &str) -> Option<Uuid> {
    use_params_map()
        .get_untracked()
        .get(name)
        .and_then(|raw| Uuid::parse_str(&raw).ok())
}

#[component]
pub(crate) fn NotFound() -> impl IntoView {
    view! { <p class="error">"Invalid address."</p> }
}
