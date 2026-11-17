//! The room information panel's width: dragging its left edge resizes it
//! between [`INFO_MIN`] and [`INFO_MAX_SHARE`] of the room screen, and the
//! chosen width is kept in [`crate::app::AppState::room_info_width`], so every
//! room and the next session open it that wide.
//!
//! The edge is the same [`MiniAppsSideHandle`] the docked mini-apps region
//! uses: it reports the drag offset, this module turns it into a width.
use makepad_widgets::*;
use crate::miniapps::side::MiniAppsSideHandle;
use super::RoomScreen;

/// The narrowest panel: names and Matrix IDs stay readable.
pub const INFO_MIN: f64 = 300.0;
/// The panel's width until the person drags it.
pub const INFO_DEFAULT: f64 = 320.0;
/// The widest panel, as a share of the room screen, so the chat keeps room.
pub const INFO_MAX_SHARE: f64 = 0.6;

/// The panel width to lay out: the chosen width kept between the minimum and
/// the maximum share of `available`. A room screen narrower than the minimum
/// gets the whole width (the compact overlay case).
pub fn clamp_width(width: f64, available: f64) -> f64 {
    let max = (available * INFO_MAX_SHARE).max(INFO_MIN);
    width.clamp(INFO_MIN, max).min(available.max(0.0))
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
        assert_eq!(clamp_width(2000.0, 600.0), 360.0);
        assert_eq!(clamp_width(INFO_DEFAULT, 280.0), 280.0, "a tiny screen gives the overlay all it has");
    }

    /// GPT review: a saved 700 in a 600-point room shows 360; dragging
    /// starts from there.
    #[test]
    fn a_drag_starts_from_the_clamped_width_on_screen() {
        let from = clamp_width(700.0, 600.0);
        assert_eq!(from, 360.0);
        assert_eq!(dragged_width(from, 20.0, 600.0), 340.0);
    }

    #[test]
    fn dragging_the_left_edge_left_widens_the_panel() {
        assert_eq!(dragged_width(320.0, -100.0, 1280.0), 420.0);
        assert_eq!(dragged_width(320.0, 50.0, 1280.0), INFO_MIN);
        assert_eq!(dragged_width(320.0, -2000.0, 1280.0), 768.0);
    }
}
