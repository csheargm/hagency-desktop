//! Turn a horizontal trackpad gesture into one navigation-back event.
use makepad_widgets::event::{ScrollEvent, ScrollPhase};

#[derive(Default)]
pub(crate) struct BackSwipe {
    active: bool,
    rejected: bool,
    x: f64,
    y: f64,
    flip_scroll: bool,
}

impl BackSwipe {
    pub fn update(&mut self, event: &ScrollEvent) -> bool {
        if event.phase == ScrollPhase::Began {
            self.flip_scroll = !natural_scroll_enabled();
        }
        self.sample(event.phase, event.is_mouse, event.scroll.x, event.scroll.y)
    }

    pub fn update_left(&mut self, event: &ScrollEvent) -> bool {
        if event.phase == ScrollPhase::Began { self.flip_scroll = !natural_scroll_enabled(); }
        self.sample(event.phase, event.is_mouse, -event.scroll.x, event.scroll.y)
    }

    fn sample(&mut self, phase: ScrollPhase, mouse: bool, dx: f64, dy: f64) -> bool {
        if mouse { return false; }
        let dx = if self.flip_scroll { -dx } else { dx };
        match phase {
            ScrollPhase::Began => {
                *self = Self { active: true, flip_scroll: self.flip_scroll, ..Self::default() };
            }
            ScrollPhase::Changed if self.active => {}
            ScrollPhase::Ended if self.active => {
                self.x += dx;
                self.y += dy.abs();
                // Makepad negates Cocoa's scrollingDeltaX: a natural-scroll
                // rightward swipe has a negative X delta. Commit on lift-off.
                let back = !self.rejected && self.x < -72.0 && -self.x > self.y * 2.0;
                *self = Self::default();
                return back;
            }
            // Touches, wheel events without phases, and OS momentum must never
            // navigate. In particular, momentum must not pop a second page.
            _ => return false,
        }
        self.x += dx;
        self.y += dy.abs();
        if self.y > 18.0 && self.y > self.x.abs() * 1.2 { self.rejected = true; }
        false
    }
}

#[cfg(target_os = "macos")]
fn natural_scroll_enabled() -> bool {
    use makepad_apple_sys::*;
    // Read on gesture start so changing the system setting needs no app restart.
    // Cocoa gives Makepad content-scroll deltas, not physical finger direction.
    unsafe {
        let defaults: ObjcId = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let value: ObjcId = msg_send![defaults, objectForKey: str_to_nsstring("com.apple.swipescrolldirection")];
        if value == nil { return true; }
        let enabled: BOOL = msg_send![value, boolValue];
        enabled == YES
    }
}

#[cfg(not(target_os = "macos"))]
fn natural_scroll_enabled() -> bool { true }

#[cfg(test)]
mod tests {
    use super::*;
    use ScrollPhase::*;

    #[test]
    fn right_swipe_commits_once_at_lift_off() {
        let mut swipe = BackSwipe::default();
        assert!(!swipe.sample(Began, false, 0., 0.));
        assert!(!swipe.sample(Changed, false, -100., 3.));
        assert!(swipe.sample(Ended, false, 0., 0.));
        assert!(!swipe.sample(Momentum, false, -200., 0.));
        assert!(!swipe.sample(Ended, false, 0., 0.));
    }

    #[test]
    fn vertical_left_small_and_cancelled_gestures_do_not_navigate() {
        for deltas in [vec![(1., 35.), (-150., 0.)], vec![(100., 0.)],
            vec![(-40., 0.)], vec![(-100., 60.)], vec![(-90., 0.), (70., 0.)]] {
            let mut swipe = BackSwipe::default();
            swipe.sample(Began, false, 0., 0.);
            for (x, y) in deltas { assert!(!swipe.sample(Changed, false, x, y)); }
            assert!(!swipe.sample(Ended, false, 0., 0.));
        }
        let mut swipe = BackSwipe::default();
        swipe.sample(Began, false, -100., 0.);
        // Cocoa cancellation is mapped to a new Began by Makepad.
        swipe.sample(Began, false, 0., 0.);
        assert!(!swipe.sample(Ended, false, 0., 0.));
    }

    #[test]
    fn mouse_wheels_and_orphaned_scrolls_do_not_navigate() {
        let mut swipe = BackSwipe::default();
        for phase in [Began, Changed, Ended] { assert!(!swipe.sample(phase, true, -200., 0.)); }
        for phase in [None, Touched, Changed, Ended, Momentum, MomentumEnded] {
            assert!(!swipe.sample(phase, false, -200., 0.));
        }
    }

    #[test]
    fn right_swipe_respects_disabled_natural_scrolling() {
        for (dx, expected) in [(100.0, true), (-100.0, false)] {
            let mut swipe = BackSwipe { flip_scroll: true, ..BackSwipe::default() };
            swipe.sample(Began, false, 0., 0.);
            swipe.sample(Changed, false, dx, 2.);
            assert_eq!(swipe.sample(Ended, false, 0., 0.), expected);
        }
    }

    #[test]
    fn left_row_swipe_respects_scroll_direction_and_rejects_vertical_or_wheel_motion() {
        for (flip, dx) in [(false, 100.0), (true, -100.0)] {
            let mut swipe = BackSwipe {flip_scroll: flip, ..Default::default()};
            swipe.sample(Began, false, 0., 0.);
            swipe.sample(Changed, false, -dx, 3.);
            assert!(swipe.sample(Ended, false, 0., 0.));
            assert!(!swipe.sample(Momentum, false, -dx, 0.));
        }
        for (mouse, dx, dy) in [(true, 100., 0.), (false, 100., 90.), (false, -100., 0.)] {
            let mut swipe = BackSwipe::default();
            swipe.sample(Began, mouse, 0., 0.);
            swipe.sample(Changed, mouse, -dx, dy);
            assert!(!swipe.sample(Ended, mouse, 0., 0.));
        }
    }
}
