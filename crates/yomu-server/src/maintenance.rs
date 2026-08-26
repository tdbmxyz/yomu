//! Low-impact periodic database housekeeping.

use std::time::Duration;

use crate::state::AppState;

pub fn spawn(state: AppState) {
    let seconds = state.config.operations.maintenance_interval_secs;
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let period = Duration::from_secs(seconds.max(60));
        loop {
            tokio::time::sleep(period).await;
            match state.db.cleanup_expired_sessions().await {
                Ok(removed) => {
                    state
                        .metrics
                        .sessions_cleaned
                        .fetch_add(removed, std::sync::atomic::Ordering::Relaxed);
                    if removed > 0 {
                        tracing::info!(removed, "removed expired sessions");
                    }
                }
                Err(err) => tracing::warn!(%err, "expired-session cleanup failed"),
            }
            if let Err(err) = state.db.checkpoint_wal().await {
                tracing::warn!(%err, "passive WAL checkpoint failed");
            }
        }
    });
}
