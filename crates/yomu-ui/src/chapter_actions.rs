//! Which bulk actions the chapter-selection menu offers, from the
//! union of the selected chapters' storage states and what this client
//! can do. Pure so the matrix is unit-testable.

/// Storage state of one selected chapter, as the menu cares about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChapterState {
    pub on_server: bool,
    pub on_device: bool,
}

/// What this client/context can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub online: bool,
    /// Shell storage or an active service worker.
    pub local_tier: bool,
    /// Shell only (web can't reliably evict its cache).
    pub local_remove: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    DownloadServer,
    DownloadBoth,
    DownloadLocal,
    RemoveServer,
    RemoveLocal,
    MarkRead,
    MarkUnread,
    MarkBeforeRead,
    MarkAfterUnread,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::DownloadServer => "Download (server)",
            Action::DownloadBoth => "Download (both)",
            Action::DownloadLocal => "Download (local)",
            Action::RemoveServer => "Remove (server)",
            Action::RemoveLocal => "Remove (local)",
            Action::MarkRead => "Mark read",
            Action::MarkUnread => "Mark unread",
            Action::MarkBeforeRead => "Mark all before as read",
            Action::MarkAfterUnread => "Mark all after as unread",
        }
    }
}

/// Menu entries for a selection: every action that would affect at
/// least one selected chapter (mixed selections show the union; the
/// action handlers skip no-op chapters).
pub fn menu_actions(states: &[ChapterState], caps: Caps) -> Vec<Action> {
    let mut out = Vec::new();
    let any_missing_server = states.iter().any(|s| !s.on_server);
    let any_server = states.iter().any(|s| s.on_server);
    let any_server_not_local = states.iter().any(|s| s.on_server && !s.on_device);
    let any_local = states.iter().any(|s| s.on_device);
    if caps.online && any_missing_server {
        out.push(Action::DownloadServer);
        if caps.local_tier {
            out.push(Action::DownloadBoth);
        }
    }
    if caps.online && caps.local_tier && any_server_not_local {
        out.push(Action::DownloadLocal);
    }
    if caps.online && any_server {
        out.push(Action::RemoveServer);
    }
    if caps.local_remove && any_local {
        out.push(Action::RemoveLocal);
    }
    // Read marks work offline through the marks outbox.
    out.extend([
        Action::MarkRead,
        Action::MarkUnread,
        Action::MarkBeforeRead,
        Action::MarkAfterUnread,
    ]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S00: ChapterState = ChapterState {
        on_server: false,
        on_device: false,
    };
    const S10: ChapterState = ChapterState {
        on_server: true,
        on_device: false,
    };
    const S11: ChapterState = ChapterState {
        on_server: true,
        on_device: true,
    };

    const APP_ONLINE: Caps = Caps {
        online: true,
        local_tier: true,
        local_remove: true,
    };
    const WEB_ONLINE: Caps = Caps {
        online: true,
        local_tier: true,
        local_remove: false,
    };
    const APP_OFFLINE: Caps = Caps {
        online: false,
        local_tier: true,
        local_remove: true,
    };

    fn has(actions: &[Action], a: Action) -> bool {
        actions.contains(&a)
    }

    #[test]
    fn undownloaded_online_offers_server_and_both() {
        let a = menu_actions(&[S00], APP_ONLINE);
        assert!(has(&a, Action::DownloadServer) && has(&a, Action::DownloadBoth));
        assert!(!has(&a, Action::RemoveServer) && !has(&a, Action::RemoveLocal));
    }

    #[test]
    fn server_only_offers_local_pull_and_server_remove() {
        let a = menu_actions(&[S10], APP_ONLINE);
        assert!(has(&a, Action::DownloadLocal) && has(&a, Action::RemoveServer));
        assert!(!has(&a, Action::DownloadServer));
    }

    #[test]
    fn both_offers_both_removals_only() {
        let a = menu_actions(&[S11], APP_ONLINE);
        assert!(has(&a, Action::RemoveServer) && has(&a, Action::RemoveLocal));
        assert!(!has(&a, Action::DownloadServer) && !has(&a, Action::DownloadLocal));
    }

    #[test]
    fn mixed_selection_shows_the_union() {
        let a = menu_actions(&[S00, S10, S11], APP_ONLINE);
        for action in [
            Action::DownloadServer,
            Action::DownloadBoth,
            Action::DownloadLocal,
            Action::RemoveServer,
            Action::RemoveLocal,
        ] {
            assert!(has(&a, action), "{action:?} missing from union");
        }
    }

    #[test]
    fn web_never_offers_local_remove() {
        assert!(!has(&menu_actions(&[S11], WEB_ONLINE), Action::RemoveLocal));
    }

    #[test]
    fn offline_offers_only_local_remove_and_marks() {
        let a = menu_actions(&[S11], APP_OFFLINE);
        assert_eq!(
            a,
            vec![
                Action::RemoveLocal,
                Action::MarkRead,
                Action::MarkUnread,
                Action::MarkBeforeRead,
                Action::MarkAfterUnread,
            ],
        );
    }
}
