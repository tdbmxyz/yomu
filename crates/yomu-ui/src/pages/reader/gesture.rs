//! Pure touch-gesture bookkeeping and geometry for the paged stage: no
//! reactive or signal access, just the state a swipe/pinch/pan carries and
//! the arithmetic over a `TouchList`.

/// Touch-gesture bookkeeping for the paged stage (swipe / pinch / pan).
#[derive(Default)]
pub(super) struct Gesture {
    /// First finger's start position, while a one-finger gesture is live.
    pub(super) start: Option<(f64, f64)>,
    /// Pan offset when the drag started.
    pub(super) pan0: (f64, f64),
    /// (finger distance, zoom) at the moment a pinch started.
    pub(super) pinch0: Option<(f64, f64)>,
    /// The finger travelled: not a tap anymore.
    pub(super) moved: bool,
    /// Horizontal intent at zoom 1: the drag drives the sliding track.
    pub(super) h_capture: bool,
    /// Eat the synthetic click that follows a swipe/pinch/pan.
    pub(super) suppress_click: bool,
}

pub(super) fn touch_xy(touches: &web_sys::TouchList, index: u32) -> Option<(f64, f64)> {
    let touch = touches.item(index)?;
    Some((touch.client_x() as f64, touch.client_y() as f64))
}

pub(super) fn touch_distance(touches: &web_sys::TouchList) -> Option<f64> {
    let (ax, ay) = touch_xy(touches, 0)?;
    let (bx, by) = touch_xy(touches, 1)?;
    Some(((ax - bx).powi(2) + (ay - by).powi(2)).sqrt())
}
