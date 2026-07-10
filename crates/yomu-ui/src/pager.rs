//! Pure decisions for the paged reader's sliding track: does a released
//! drag commit or cancel, which way a drag turns, and what each of the
//! three panels shows over the virtual position range [-1 .. count]
//! (the ends are chapter-transition panels). Wasm-free so the matrix is
//! unit-testable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Commit,
    Cancel,
}

/// A released drag commits past 30% of the viewport width, or as a
/// flick: fast recent movement (>0.5 px/ms) in the drag's own direction
/// with at least 40px travelled.
pub fn verdict(dx: f64, width: f64, velocity: f64) -> Verdict {
    let flick = dx.abs() > 40.0 && velocity.abs() > 0.5 && velocity * dx > 0.0;
    if dx.abs() > width * 0.30 || flick {
        Verdict::Commit
    } else {
        Verdict::Cancel
    }
}

/// Position delta for a drag: pulling the page left (dx < 0) reveals
/// the panel on the right — the next page in LTR, the previous in RTL.
pub fn step(dx: f64, rtl: bool) -> i64 {
    let forward = if dx < 0.0 { 1 } else { -1 };
    if rtl { -forward } else { forward }
}

/// What a slot at `position` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Page(u32),
    /// "Previous chapter this way" panel at -1.
    TransitionPrev,
    /// "Finished this chapter — next up" panel at count.
    TransitionNext,
    /// Chapter edge with no neighbor: shown, but never landed on.
    DeadEnd,
    /// Beyond the virtual range: nothing.
    Empty,
}

pub fn panel(position: i64, count: u32, has_prev: bool, has_next: bool) -> Panel {
    if count == 0 {
        return Panel::Empty;
    }
    if (0..count as i64).contains(&position) {
        return Panel::Page(position as u32);
    }
    match position {
        -1 if has_prev => Panel::TransitionPrev,
        -1 => Panel::DeadEnd,
        p if p == count as i64 && has_next => Panel::TransitionNext,
        p if p == count as i64 => Panel::DeadEnd,
        _ => Panel::Empty,
    }
}

/// Lowest / highest position a turn may land on (dead ends rubber-band).
pub fn bounds(count: u32, has_prev: bool, has_next: bool) -> (i64, i64) {
    let lo = if has_prev { -1 } else { 0 };
    let hi = if has_next {
        count as i64
    } else {
        count as i64 - 1
    };
    (lo, hi)
}

/// Damped offset for dragging against a dead end.
pub fn damp(dx: f64) -> f64 {
    dx / 3.0
}

/// Flick velocity over the last ~100ms of touch samples.
#[derive(Default)]
pub struct Flick {
    samples: Vec<(f64, f64)>, // (x px, t ms)
}

impl Flick {
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn push(&mut self, x: f64, t: f64) {
        self.samples.push((x, t));
        self.samples.retain(|(_, st)| t - st <= 100.0);
    }

    /// px/ms over the retained window; 0 without two spaced samples.
    pub fn velocity(&self) -> f64 {
        let (Some((x0, t0)), Some((x1, t1))) = (self.samples.first(), self.samples.last()) else {
            return 0.0;
        };
        if t1 - t0 <= 0.0 {
            return 0.0;
        }
        (x1 - x0) / (t1 - t0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_drag_commits_short_drag_cancels() {
        assert_eq!(verdict(-200.0, 400.0, 0.0), Verdict::Commit); // 50% > 30%
        assert_eq!(verdict(-100.0, 400.0, 0.0), Verdict::Cancel); // 25%
    }

    #[test]
    fn flick_commits_but_only_in_its_own_direction() {
        assert_eq!(verdict(-50.0, 400.0, -0.8), Verdict::Commit);
        assert_eq!(verdict(-50.0, 400.0, 0.8), Verdict::Cancel); // reversed flick
        assert_eq!(verdict(-30.0, 400.0, -0.8), Verdict::Cancel); // too short
    }

    #[test]
    fn drag_left_is_forward_in_ltr_backward_in_rtl() {
        assert_eq!(step(-80.0, false), 1);
        assert_eq!(step(80.0, false), -1);
        assert_eq!(step(-80.0, true), -1);
        assert_eq!(step(80.0, true), 1);
    }

    #[test]
    fn panels_cover_pages_transitions_and_edges() {
        assert_eq!(panel(0, 5, true, true), Panel::Page(0));
        assert_eq!(panel(4, 5, true, true), Panel::Page(4));
        assert_eq!(panel(-1, 5, true, true), Panel::TransitionPrev);
        assert_eq!(panel(5, 5, true, true), Panel::TransitionNext);
        assert_eq!(panel(-1, 5, false, true), Panel::DeadEnd);
        assert_eq!(panel(5, 5, true, false), Panel::DeadEnd);
        assert_eq!(panel(-2, 5, true, true), Panel::Empty);
        assert_eq!(panel(6, 5, true, true), Panel::Empty);
        assert_eq!(panel(0, 0, true, true), Panel::Empty);
    }

    #[test]
    fn bounds_exclude_missing_neighbours() {
        assert_eq!(bounds(5, true, true), (-1, 5));
        assert_eq!(bounds(5, false, false), (0, 4));
    }

    #[test]
    fn flick_velocity_uses_recent_window_only() {
        let mut f = Flick::default();
        f.push(0.0, 0.0);
        f.push(-10.0, 1000.0); // old sample evicted by the next push
        f.push(-110.0, 1100.0);
        assert!((f.velocity() - (-1.0)).abs() < 1e-9);
        f.clear();
        assert_eq!(f.velocity(), 0.0);
    }
}
