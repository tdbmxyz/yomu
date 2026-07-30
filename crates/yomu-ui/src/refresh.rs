//! Pull-to-refresh. Touch-only by design: on the web and the desktop
//! shell a page reload is the refresh, and a mouse never triggers this.
//!
//! Not to be confused with `pull.rs`, which drains the device-download
//! queue.

use leptos::ev;
use leptos::prelude::*;

/// Resistance, so the indicator trails the thumb rather than tracking it.
const DAMPING: f64 = 0.45;
/// How far the damped travel must reach before letting go refreshes.
const THRESHOLD: f64 = 72.0;
/// Anything above this is "not at the top": a swipe down mid-list must
/// scroll, not refresh.
const TOP: f64 = 0.5;

fn travel(raw: f64) -> f64 {
    (raw * DAMPING).max(0.0)
}

fn armed(travel: f64) -> bool {
    travel >= THRESHOLD
}

fn can_start(scroll_y: f64) -> bool {
    scroll_y <= TOP
}

/// What the indicator draws from.
#[derive(Clone, Copy)]
pub struct PullState {
    /// Damped pixels the list has been dragged down. 0 when idle.
    pub distance: RwSignal<f64>,
    /// A refresh is running; keep the spinner up.
    pub refreshing: RwSignal<bool>,
    /// Far enough that letting go will refresh.
    pub armed: RwSignal<bool>,
}

/// Listen on `window`, so the gesture works whatever actually scrolls.
///
/// `window_event_listener` + `on_cleanup` rather than a hand-rolled
/// `Closure`: `Closure` is neither `Send` nor `Sync`, so it cannot live in
/// a `StoredValue`, and rolling one reimplements what leptos provides.
pub fn use_pull_to_refresh(on_refresh: impl Fn() + Copy + 'static) -> PullState {
    let state = PullState {
        distance: RwSignal::new(0.0),
        refreshing: RwSignal::new(false),
        armed: RwSignal::new(false),
    };
    // Where the finger went down, if the gesture is eligible at all.
    let origin = StoredValue::new(None::<f64>);

    let start = window_event_listener(ev::touchstart, move |e| {
        let Some(touch) = e.touches().get(0) else {
            return;
        };
        origin.set_value(
            can_start(window().scroll_y().unwrap_or(0.0)).then(|| touch.client_y() as f64),
        );
    });
    let moved = window_event_listener(ev::touchmove, move |e| {
        let (Some(from), Some(touch)) = (origin.get_value(), e.touches().get(0)) else {
            return;
        };
        let distance = travel(touch.client_y() as f64 - from);
        state.distance.set(distance);
        state.armed.set(armed(distance));
    });
    let end = window_event_listener(ev::touchend, move |_| {
        origin.set_value(None);
        let fire = state.armed.get_untracked() && !state.refreshing.get_untracked();
        state.distance.set(0.0);
        state.armed.set(false);
        if fire {
            state.refreshing.set(true);
            on_refresh();
        }
    });
    on_cleanup(move || {
        start.remove();
        moved.remove();
        end.remove();
    });
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Damped, so the indicator feels weighted rather than glued to the
    /// thumb, and never negative on an upward drag.
    #[test]
    fn travel_is_damped_and_never_negative() {
        assert_eq!(travel(0.0), 0.0);
        assert_eq!(travel(100.0), 45.0);
        assert_eq!(travel(-40.0), 0.0);
    }

    /// Only past the threshold does letting go refresh.
    #[test]
    fn arming_needs_the_threshold() {
        assert!(!armed(travel(100.0)));
        assert!(armed(travel(200.0)));
    }

    /// Swiping down mid-list must not refresh — only a pull from the very
    /// top arms the gesture.
    #[test]
    fn only_the_top_of_the_page_arms_the_gesture() {
        assert!(can_start(0.0));
        assert!(can_start(0.4));
        assert!(!can_start(120.0));
    }
}
