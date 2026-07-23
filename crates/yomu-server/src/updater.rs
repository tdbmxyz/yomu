//! Periodic new-chapter checker: walks the whole library on an interval,
//! re-syncing each publication against its source (which auto-queues downloads
//! for publications that want them — see `sync::refresh_publication`).

use std::time::Duration;

use crate::state::AppState;
use crate::sync;

pub fn spawn(state: AppState) {
    if state.config.updater.enabled {
        tokio::spawn(run(state));
    }
}

async fn run(state: AppState) {
    // Only the updater notifies: manual refreshes mean the user is in the
    // app, and adding a publication would announce its whole backlog.
    let notifier = crate::notifier::Notifier::new(state.config.notify.clone());
    // Clamp: interval_secs = 0 would busy-loop hammering every source.
    let interval = Duration::from_secs(state.config.updater.interval_secs.max(60));
    loop {
        // Sleep first: startup should not hammer every source at once,
        // and a fresh library was just synced by its add flow anyway.
        tokio::time::sleep(interval).await;

        if let Err(err) = state
            .db
            .prune_updates(chrono::Utc::now() - chrono::Duration::days(30))
            .await
        {
            tracing::warn!(%err, "pruning updates feed");
        }

        // Only categories with update_enabled (paused/finished series
        // don't need to hammer their sources).
        let manga = match state.db.list_publications_for_update().await {
            Ok(manga) => manga,
            Err(err) => {
                tracing::error!(%err, "listing library for update check");
                continue;
            }
        };
        tracing::info!(count = manga.len(), "checking library for new chapters");
        for entry in manga {
            match sync::refresh_publication(&state, &entry).await {
                Ok(new) if !new.is_empty() => {
                    // Feed for shell notifications; only the updater
                    // writes it, mirroring the ntfy rule above.
                    if let Err(err) = state.db.add_update(entry.id, &new).await {
                        tracing::warn!(%err, "recording update event");
                    }
                    notifier.notify_new_units(&entry.title, &new).await;
                }
                Ok(_) => {}
                Err(err) => {
                    // One broken source must not stop the sweep.
                    tracing::warn!(manga = %entry.title, %err, "update check failed");
                }
            }
        }
    }
}
