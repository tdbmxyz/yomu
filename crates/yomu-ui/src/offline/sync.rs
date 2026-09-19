//! One app-wide drain. No overlapping acknowledgements; bounded exponential
//! backoff preserves transient failures without hammering an unavailable server.
use super::{context, outbox, pending_marks};
use leptos::prelude::*;

pub(crate) fn start(
    conn: RwSignal<crate::Connectivity>,
    client: yomu_client::YomuClient,
    library: crate::cache::LibraryCache,
    detail: crate::cache::DetailCache,
) {
    let running = StoredValue::new(false);
    let next = StoredValue::new(0_f64);
    let delay = StoredValue::new(5_u32);
    let tick = move || {
        if running.get_value()
            || conn.get_untracked() != crate::Connectivity::Online
            || !context::active()
            || js_sys::Date::now() < next.get_value()
            || (outbox().is_empty() && pending_marks().is_empty())
        {
            return;
        }
        running.set_value(true);
        let client = client.clone();
        leptos::task::spawn_local(async move {
            let ok = if context::verify().await {
                let progress = super::flush_outbox(&client, library, detail).await;
                let marks = super::flush_marks(&client, library, detail).await;
                progress && marks
            } else {
                false
            };
            let wait = if ok { 5 } else { delay.get_value() };
            next.set_value(js_sys::Date::now() + f64::from(wait) * 1000.0);
            delay.set_value(if ok { 5 } else { (wait * 2).min(300) });
            running.set_value(false);
        });
    };
    let on_change = tick.clone();
    Effect::new(move |_| {
        let _ = conn.get();
        on_change();
    });
    let handle = set_interval_with_handle(tick, std::time::Duration::from_secs(5));
    on_cleanup(move || {
        if let Ok(handle) = handle {
            handle.clear();
        }
    });
}
