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

/// What room info is laid out as for a room screen `width` wide with the
/// person's panel width `panel`: beside the chat or not, and how wide.
pub fn layout_of(width: f64, panel: f64) -> (bool, f64) {
    (width >= INLINE_MIN, clamp_width(panel, width))
}

/// The room screen widths room info is laid out for. Beside a docked side
/// region (the Agents panel) the room screen is a deferred Fill: drawn at a
/// provisional full-row width and shrunk after the region is placed, so the
/// peeked width is wrong, and at draw start the Dock has just re-recorded its
/// draw list, so the screen's area is no longer readable (GPT review). The real
/// width is read on the frame after a draw, outside any draw, and laid out from.
#[derive(Debug, Default)]
pub struct InfoLayout {
    /// The redraw this layout was made in, and the width it used.
    used: Option<(u64, f64)>,
    /// The width the room screen really took, read after its last draw.
    drawn: Option<f64>,
    /// The drawn width a corrective redraw was asked for: one per distinct width, never a loop.
    corrected: Option<f64>,
    check: NextFrame,
}

impl InfoLayout {
    /// The width to lay out this redraw: the real width once one was read,
    /// else the peek. Fixed for the redraw: a stepped draw resumes later.
    pub fn width(&mut self, redraw: u64, peeked: f64) -> (f64, bool) {
        match self.used {
            Some((at, width)) if at == redraw => (width, false),
            _ => {
                let width = self.drawn.unwrap_or(peeked);
                self.used = Some((redraw, width));
                (width, true)
            }
        }
    }

    /// Records the width the room screen really took; whether one corrective
    /// redraw is due: only when that width changes the layout (beside or
    /// not, or the clamped panel width), and once per distinct drawn width.
    pub fn settle(&mut self, drawn: f64, panel: f64) -> bool {
        // No area yet (never drawn, or a hidden tab): keep the last real width.
        if drawn <= 0.0 { return false }
        self.drawn = Some(drawn);
        let Some((_, used)) = self.used else { return false };
        if layout_of(used, panel) == layout_of(drawn, panel) || self.corrected == Some(drawn) {
            return false;
        }
        self.corrected = Some(drawn);
        true
    }
}

/// The width after dragging the left edge `dx` points from `from`: moving
/// the edge left (negative `dx`) widens the panel.
pub fn dragged_width(from: f64, dx: f64, available: f64) -> f64 {
    clamp_width(from - dx, available)
}

impl RoomScreen {
    /// The width this frame lays room info out in ([`InfoLayout::width`]);
    /// a new layout asks for the frame after the draw to read the real width.
    pub(super) fn info_layout_width(&mut self, cx: &mut Cx, peeked: f64) -> f64 {
        let (width, fresh) = self.info_layout.width(cx.redraw_id, peeked);
        if fresh {
            self.info_layout.check = cx.new_next_frame();
        }
        width
    }

    /// The frame after a draw, when the room screen's area is valid: reads
    /// the width it really took, and redraws once when that changes the layout.
    pub(super) fn check_info_layout_width(&mut self, cx: &mut Cx, event: &Event) {
        if self.info_layout.check.is_event(event).is_none() { return }
        let drawn = self.view.area().rect(cx).size.x;
        if self.info_layout.settle(drawn, self.info_width()) {
            self.view.redraw(cx);
        }
    }

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

    /// GPT review: a 0.5-point tolerance missed 700 → 699.5; and a width that
    /// does not change the layout, or one already corrected, never redraws again.
    #[test]
    fn only_a_changed_layout_asks_for_one_corrective_redraw() {
        let mut layout = InfoLayout::default();
        assert_eq!(layout.width(1, 1090.0), (1090.0, true), "before a first frame only the peek is known");
        assert_eq!(layout.width(1, 1.0), (1090.0, false), "fixed for the redraw");
        assert!(layout.settle(650.0, INFO_DEFAULT), "the Agents panel docked: inline becomes overlay");
        assert!(!layout.settle(650.0, INFO_DEFAULT), "at most one corrective redraw per drawn width");
        assert_eq!(layout.width(2, 1090.0), (650.0, true), "laid out from the real width, not the provisional peek");
        assert!(!layout.settle(650.0, INFO_DEFAULT), "stable: no further redraw");
        assert!(!layout.settle(660.0, INFO_DEFAULT), "a width that keeps the layout redraws nothing");
        let mut edge = InfoLayout::default();
        edge.width(1, INLINE_MIN);
        assert!(edge.settle(INLINE_MIN - 0.5, INFO_DEFAULT), "700 → 699.5 switches to the overlay");
        assert_eq!(edge.width(2, INLINE_MIN).0, INLINE_MIN - 0.5);
        assert!(!edge.settle(INLINE_MIN - 0.5, INFO_DEFAULT));
        assert!(!InfoLayout::default().settle(0.0, INFO_DEFAULT), "an unreadable area changes nothing");
    }

    /// Live, Nov 19 05:52 (1400-point window): room info open, then the Agents
    /// panel docks and the room screen really gets 650. Each frame re-records
    /// the same draw list before the layout is read, as the Dock does, so the
    /// previous frame's area is gone at draw start (GPT review).
    #[test]
    fn opening_and_closing_the_agents_panel_lays_room_info_out_again() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        use super::super::desktop_information_columns_tests::screen;
        let (mut cx, mut screen) = screen();
        screen.chat_info_requested = true;
        screen.room_name_id = Some(crate::utils::RoomNameId::empty(matrix_sdk::ruma::room_id!("!a:server").to_owned()));
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, dvec2(1280.0, 900.0));
        let mut list = DrawList2d::new(&mut cx);
        let overlay = Overlay {draw_list: DrawList::new(&mut cx)};
        let mut redraws = 0;
        let mut frame = |cx: &mut Cx, screen: &mut RoomScreen, real: f64| {
            cx.redraw_id += 1;
            cx.redraw_all();
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None); list.begin_always(&mut cx2d); overlay.begin(&mut cx2d);
            cx2d.begin_root_turtle(dvec2(1280.0, 900.0), Layout::flow_overlay());
            // Read inside the re-recorded list; the peek is the provisional full row.
            let width = screen.info_layout_width(&mut cx2d, 1090.0);
            screen.layout_desktop_chat_info(&mut cx2d, width);
            screen.view.draw_walk_all(&mut cx2d, &mut Scope::with_data(&mut crate::app::AppState::default()),
                Walk {width: Size::Fixed(real), height: Size::Fixed(760.0), ..Walk::default()});
            overlay.end(&mut cx2d); cx2d.end_pass_sized_turtle(); list.end(&mut cx2d); cx2d.end_pass(&pass);
        };
        let mut check = |cx: &mut Cx, screen: &mut RoomScreen| {
            let next = Event::NextFrame(NextFrameEvent { frame: 1, time: 0.0, set: [screen.info_layout.check].into() });
            let before = screen.info_layout.corrected;
            screen.check_info_layout_width(cx, &next);
            if screen.info_layout.corrected != before { redraws += 1; }
        };
        let inline = |cx: &mut Cx, screen: &RoomScreen| screen.view.view(cx, ids!(desktop_chat_info_panel)).visible();
        let chat = |cx: &mut Cx, screen: &RoomScreen| screen.view.view(cx, ids!(chat_center)).area().rect(cx).size.x;
        frame(&mut cx, &mut screen, 1090.0);
        check(&mut cx, &mut screen);
        assert!(inline(&mut cx, &screen), "room info beside the chat");
        // The Agents panel docks: this frame still lays out for the old width.
        frame(&mut cx, &mut screen, 650.0);
        check(&mut cx, &mut screen);
        frame(&mut cx, &mut screen, 650.0);
        check(&mut cx, &mut screen);
        // Too narrow for both: room info gives way (the existing resize-down rule).
        assert!(!inline(&mut cx, &screen), "room info no longer sits beside the chat");
        assert_eq!(chat(&mut cx, &screen), 650.0, "the chat keeps the room screen's whole width");
        for _ in 0..3 {
            frame(&mut cx, &mut screen, 650.0);
            check(&mut cx, &mut screen);
        }
        // The Agents panel closes: the chat gets the row back; room info opens beside it again.
        frame(&mut cx, &mut screen, 1090.0);
        check(&mut cx, &mut screen);
        frame(&mut cx, &mut screen, 1090.0);
        check(&mut cx, &mut screen);
        assert_eq!(chat(&mut cx, &screen), 1090.0);
        screen.chat_info_requested = true;
        frame(&mut cx, &mut screen, 1090.0);
        check(&mut cx, &mut screen);
        assert!(inline(&mut cx, &screen) && chat(&mut cx, &screen) >= CHAT_MIN);
        drop(frame);
        drop(check);
        assert_eq!(redraws, 2, "one corrective redraw when the panel docks, one when it closes; a stable width asks for none");
    }

    #[test]
    fn dragging_the_left_edge_left_widens_the_panel() {
        assert_eq!(dragged_width(320.0, -100.0, 1280.0), 420.0);
        assert_eq!(dragged_width(320.0, 50.0, 1280.0), INFO_MIN);
        assert_eq!(dragged_width(320.0, -2000.0, 1280.0), 768.0);
    }
}
