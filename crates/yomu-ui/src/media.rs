//! Renew image capabilities without remounting a reader or losing its position.
use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_client::YomuClient;

#[derive(Clone, Copy)]
pub(crate) struct Media {
    pub token: RwSignal<Option<String>>,
    pub revision: RwSignal<u32>,
    refreshing: RwSignal<bool>,
}

impl Media {
    pub fn new() -> Self {
        Self {
            token: RwSignal::new(crate::auth::media_token()),
            revision: RwSignal::new(0),
            refreshing: RwSignal::new(false),
        }
    }

    pub fn refresh(self, client: YomuClient) {
        if self.refreshing.get_untracked() {
            return;
        }
        self.refreshing.set(true);
        spawn_local(async move {
            let session = crate::auth::session_token();
            if let Ok(response) = client.with_token(session.clone()).media_token().await
                && session == crate::auth::session_token()
            {
                crate::auth::store_media_token(&response.token);
                self.token.set(Some(response.token));
                // Also retry transient failures when the server minted the same
                // token twice in one second. Never reuse a failed cached URL.
                self.revision.update(|n| *n = n.wrapping_add(1));
            }
            self.refreshing.set(false);
        });
    }
}

pub(crate) fn use_media() -> Media {
    use_context().expect("Media provided by App")
}
