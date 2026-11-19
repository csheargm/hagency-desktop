//! The room information panel's width: dragging its left edge resizes it
//! between [`INFO_MIN`] and [`INFO_MAX_SHARE`] of the room screen, and the
//! chosen width is kept in [`crate::app::AppState::room_info_width`], so every
//! room and the next session open it that wide.
//!
//! The edge is the same [`MiniAppsSideHandle`] the docked mini-apps region
//! uses: it reports the drag offset, this module turns it into a width.
use makepad_widgets::*;
use crate::miniapps::side::{CHAT_MIN, MiniAppsSideHandle};
use super::RoomScreen;

/// The narrowest panel: names and Matrix IDs stay readable.
pub const INFO_MIN: f64 = 300.0;
/// The panel's width until the person drags it.
pub const INFO_DEFAULT: f64 = 320.0;
/// The widest panel, as a share of the room screen, so the chat keeps room.
pub const INFO_MAX_SHARE: f64 = 0.6;

/// The narrowest room screen that shows the panel beside the chat: the
/// panel's minimum and the chat's [`CHAT_MIN`]. Narrower, the panel shows
/// over the chat instead of squeezing it (as the docked mini-apps do).
pub const INLINE_MIN: f64 = INFO_MIN + CHAT_MIN;

/// The panel width to lay out: the chosen width kept between the minimum and
/// the maximum share of `available`, and never so wide that the chat beside
/// it drops under [`CHAT_MIN`]; the saved width is unchanged and comes back
/// when there is room. A room screen narrower than the minimum gets the
/// whole width (the compact overlay case).
pub fn clamp_width(width: f64, available: f64) -> f64 {
    let max = (available * INFO_MAX_SHARE).min(available - CHAT_MIN).max(INFO_MIN);
    width.clamp(INFO_MIN, max).min(available.max(0.0))
}

thread_local! {
    /// The room screen's width as the home screen lays it out (`None` outside the desktop layout).
    static ROOM_WIDTH: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Called by the home screen each frame with the room screen's width beside
/// the rooms list and a docked side region (the Agents panel): the window's
/// content width minus both. The room screen's own peek can't tell: beside the
/// docked region it is a deferred Fill, measured at the whole row's width
/// (live, Nov 19: room info stayed inline beside a 330-point chat). `sizes`
/// (window width, docked region width) are logged when the region docks or
/// undocks or room info's place changes, not on every drag step.
pub fn set_room_width(width: Option<f64>, sizes: (f64, f64)) {
    let shape = |w: Option<f64>| w.map(|w| w >= INLINE_MIN);
    let before = ROOM_WIDTH.with(|w| w.replace(width));
    thread_local! { static DOCKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    let docked = sizes.1 > 0.0;
    if shape(before) != shape(width) || DOCKED.with(|d| d.replace(docked)) != docked {
        log!("Room info layout: room screen {width:?} (window {}, docked side region {})", sizes.0, sizes.1);
    }
}

/// The width room info lays out in: the room screen's peek, never more than
/// the width the home screen gave it.
pub fn room_width(peeked: f64) -> f64 {
    ROOM_WIDTH.with(|w| w.get()).map_or(peeked, |room| peeked.min(room))
}

/// The width after dragging the left edge `dx` points from `from`: moving
/// the edge left (negative `dx`) widens the panel.
pub fn dragged_width(from: f64, dx: f64, available: f64) -> f64 {
    clamp_width(from - dx, available)
}

impl RoomScreen {
    /// The width the panel is drawn at, before clamping to the room screen.
    pub(super) fn info_width(&self) -> f64 {
        self.info_width.unwrap_or(INFO_DEFAULT)
    }

    /// Picks up the person's saved width while no drag is under way.
    pub(super) fn restore_info_width(&mut self, scope: &mut Scope) {
        if self.info_drag_from.is_none()
            && let Some(saved) = scope.data.get::<crate::app::AppState>().and_then(|state| state.room_info_width) {
            self.info_width = Some(saved);
        }
    }

    /// Follows a drag of the panel's edge; on release, saves the width.
    pub(super) fn handle_info_panel_drag(&mut self, cx: &mut Cx, scope: &mut Scope) {
        let (drag, released) = self.view.widget(cx, ids!(desktop_chat_info_handle))
            .borrow_mut::<MiniAppsSideHandle>()
            .map(|mut handle| handle.take_drag())
            .unwrap_or_default();
        let available = self.view.area().rect(cx).size.x;
        if let Some(dx) = drag {
            // Start from the width on screen, which a narrow window clamps.
            let from = *self.info_drag_from.get_or_insert(clamp_width(self.info_width(), available));
            if dx != 0.0 {
                self.info_dragged = true;
                let width = dragged_width(from, dx, available);
                if Some(width) != self.info_width {
                    self.info_width = Some(width);
                    self.view.redraw(cx);
                }
            }
        }
        if released {
            self.info_drag_from = None;
            // A press without a move keeps the saved preference.
            if std::mem::take(&mut self.info_dragged)
                && let Some(state) = scope.data.get_mut::<crate::app::AppState>() {
                state.room_info_width = Some(self.info_width());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_stays_between_the_minimum_and_sixty_percent() {
        assert_eq!(clamp_width(INFO_DEFAULT, 1280.0), INFO_DEFAULT);
        assert_eq!(clamp_width(100.0, 1280.0), INFO_MIN, "never narrower than the minimum");
        assert_eq!(clamp_width(2000.0, 1000.0), 600.0, "never wider than 60% of the room screen");
        assert_eq!(clamp_width(2000.0, 600.0), INFO_MIN, "a narrow room screen keeps the panel at its minimum");
        assert_eq!(clamp_width(INFO_DEFAULT, 280.0), 280.0, "a tiny screen gives the overlay all it has");
    }

    /// GPT review: a saved 700 in an 800-point room shows 400; dragging
    /// starts from there.
    #[test]
    fn a_drag_starts_from_the_clamped_width_on_screen() {
        let from = clamp_width(700.0, 800.0);
        assert_eq!(from, 400.0);
        assert_eq!(dragged_width(from, 20.0, 800.0), 380.0);
    }

    /// Nov 19: a widened panel beside the open Agents panel left the chat
    /// ~170 points wide. The chat keeps [`CHAT_MIN`]; the saved width stays.
    #[test]
    fn a_wide_saved_panel_never_squeezes_the_chat_below_its_minimum() {
        for available in [INLINE_MIN, 760.0, 900.0, 1000.0, 1280.0] {
            let width = clamp_width(2000.0, available);
            assert!(available - width >= CHAT_MIN, "{available}: chat {}", available - width);
            assert!(width >= INFO_MIN);
        }
        assert_eq!(clamp_width(700.0, 900.0), 500.0, "the panel shrinks first");
        assert_eq!(clamp_width(700.0, 1280.0), 700.0, "with room again, the saved width comes back");
        assert_eq!(dragged_width(400.0, -500.0, 900.0), 500.0, "a drag stops where the chat's minimum begins");
    }

    /// Live, Nov 19 05:52: the room screen peeked 1090 beside a docked Agents
    /// panel while it really had 650; the home screen's width wins.
    #[test]
    fn the_home_screens_room_width_caps_the_peek() {
        set_room_width(Some(650.0), (1400.0, 444.0));
        assert_eq!(room_width(1090.0), 650.0);
        assert!(room_width(1090.0) < INLINE_MIN, "so room info does not sit beside a squeezed chat");
        assert_eq!(room_width(500.0), 500.0, "a narrower peek (a thread pane) still counts");
        set_room_width(None, (0.0, 0.0));
        assert_eq!(room_width(1090.0), 1090.0, "outside the desktop layout only the peek is known");
    }

    #[test]
    fn dragging_the_left_edge_left_widens_the_panel() {
        assert_eq!(dragged_width(320.0, -100.0, 1280.0), 420.0);
        assert_eq!(dragged_width(320.0, 50.0, 1280.0), INFO_MIN);
        assert_eq!(dragged_width(320.0, -2000.0, 1280.0), 768.0);
    }
}
