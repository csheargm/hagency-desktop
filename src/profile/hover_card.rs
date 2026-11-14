//! The small person/Agent card shown while hovering an avatar (like Buzz):
//! avatar, name, Matrix ID, server, room role, an Agent tag, and
//! "View profile" / "Message" buttons.
//!
//! One app-level instance (registered as a Cx global, like the mention
//! popup) is fed by the hooks in [`crate::shared::avatar_hover`]. It draws
//! no scrim and captures no pointer outside the card, so the timeline under
//! it keeps scrolling and hovering normally.

use makepad_widgets::*;

use crate::{
    shared::{avatar::AvatarWidgetExt, avatar_hover::{HoverEffect, HoverInput, HoverMachine}},
    sliding_sync::current_user_id,
    utils,
};
use super::{
    hover_card_route::{dispatch, request_for, room_name, CardButton},
    member_summary,
    profile_route,
    user_profile::UserProfileAndRoomId,
};

/// Fixed card width, in DIPs (must match `popup_frame.width` below).
const CARD_WIDTH: f64 = 280.0;
/// The card's usual height, used to keep it inside the window.
const CARD_EST_HEIGHT: f64 = 190.0;
/// Space between the avatar and the card, and the card and the window edge.
const CARD_GAP: f64 = 8.0;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AvatarHoverCard = #(AvatarHoverCard::register_widget(vm)) {
        width: Fill, height: Fill, flow: Overlay, align: Align{x: 0.0, y: 0.0}
        popup_frame := View {
            width: 280, height: Fit
            card := RoundedView {
                width: Fill, height: Fit, flow: Down, spacing: 10, padding: 12
                show_bg: true
                draw_bg +: {
                    color: (mod.widgets.RBX_BG_SURFACE)
                    border_radius: (mod.widgets.RBX_RADIUS_SM)
                    border_size: 1.0
                    border_color: (mod.widgets.RBX_STROKE_STRONG)
                }
                View {
                    width: Fill, height: Fit, flow: Right, spacing: 10, align: Align{y: 0.5}
                    card_avatar := Avatar { width: 40, height: 40 }
                    View {
                        width: Fill, height: Fit, flow: Down, spacing: 3
                        View {
                            width: Fill, height: Fit, flow: Right, spacing: 6, align: Align{y: 0.5}
                            presence_dot := RoundedView {
                                visible: false, width: 8, height: 8
                                draw_bg +: {color: #x2e9e5b border_radius: 4.0}
                            }
                            card_name := Label {
                                width: Fill, height: Fit, max_lines: 1, text_overflow: Ellipsis
                                draw_text +: {color: (mod.widgets.RBX_FG_PRIMARY) text_style: (mod.widgets.RBX_TEXT_BODY_STRONG)}
                                text: ""
                            }
                        }
                        card_user_id := Label {
                            width: Fill, height: Fit, max_lines: 1, text_overflow: Ellipsis
                            draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: (mod.widgets.RBX_TEXT_META)}
                            text: ""
                        }
                        agent_tag := RoundedView {
                            visible: false, width: Fit, height: Fit
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            draw_bg +: {color: mod.widgets.RINX_SELECTED border_radius: 4}
                            Label {
                                width: Fit, height: Fit
                                draw_text +: {color: mod.widgets.RINX_ACCENT text_style: theme.font_regular{font_size: (9.5 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: #(crate::i18n::tr("Agent")) i18n_text: "Agent"
                            }
                        }
                    }
                }
                card_details := Label {
                    width: Fill, height: Fit, flow: Flow.Right{wrap: true}
                    draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: (mod.widgets.RBX_TEXT_META)}
                    text: ""
                }
                View {
                    width: Fill, height: Fit, flow: Right, spacing: 8
                    view_profile_button := RobrixNeutralIconButton {
                        width: Fill, height: 32, margin: 0, align: Align{x: 0.5 y: 0.5}
                        icon_walk: Walk{width: 0 height: 0}
                        text: #(crate::i18n::tr("View profile")) i18n_text: "View profile"
                    }
                    message_button := RobrixIconButton {
                        width: Fill, height: 32, margin: 0, spacing: 6, align: Align{x: 0.5 y: 0.5}
                        draw_icon.svg: mod.widgets.ICON_DOUBLE_CHAT
                        icon_walk: Walk{width: 14 height: 14}
                        text: #(crate::i18n::tr("Message")) i18n_text: "Message"
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct AvatarHoverCard {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] machine: HoverMachine,
    #[rust] timer: Timer,
    /// The avatar widget the pointer is on (or last left), and what it shows.
    #[rust] anchor_uid: Option<WidgetUid>,
    #[rust] target: Option<UserProfileAndRoomId>,
    #[rust] anchor: Rect,
    #[rust] shown: bool,
    #[rust] over_card: bool,
}

impl Widget for AvatarHoverCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.timer.is_event(event).is_some() {
            self.step(cx, HoverInput::Timeout);
        }
        if !self.shown {
            return;
        }
        if let Event::Signal = event {
            // The room member may have just arrived from the server.
            self.populate_details(cx);
        }
        if let Event::MouseMove(e) = event {
            let inside = self.view.view(cx, ids!(card)).area().rect(cx).contains(e.abs);
            if inside != self.over_card {
                self.over_card = inside;
                self.step(cx, if inside { HoverInput::EnterCard } else { HoverInput::LeaveCard });
            }
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            self.handle_buttons(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.shown {
            return DrawStep::done();
        }
        self.position_card(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AvatarHoverCard {
    pub(crate) fn anchor_entered(&mut self, cx: &mut Cx, uid: WidgetUid, info: UserProfileAndRoomId, anchor: Rect) {
        if !crate::home::home_screen::effective_is_desktop(cx) {
            return;
        }
        let switched = self.target.as_ref().is_none_or(|t| t.user_id != info.user_id);
        self.anchor_uid = Some(uid);
        self.anchor = anchor;
        self.target = Some(info);
        if self.shown && switched {
            self.populate(cx);
        }
        self.step(cx, HoverInput::EnterAnchor);
    }

    pub(crate) fn anchor_left(&mut self, cx: &mut Cx, uid: WidgetUid) {
        // A late "left" from the previous avatar must not close the new one's card.
        if self.anchor_uid == Some(uid) {
            self.step(cx, HoverInput::LeaveAnchor);
        }
    }

    pub(crate) fn dismiss(&mut self, cx: &mut Cx) {
        self.step(cx, HoverInput::Dismiss);
    }

    fn step(&mut self, cx: &mut Cx, input: HoverInput) {
        match self.machine.step(input) {
            HoverEffect::Nothing => {}
            HoverEffect::Arm(secs) => {
                cx.stop_timer(self.timer);
                self.timer = cx.start_timeout(secs);
            }
            HoverEffect::Disarm => cx.stop_timer(self.timer),
            HoverEffect::Show => {
                self.shown = true;
                self.over_card = false;
                self.populate(cx);
            }
            HoverEffect::Hide => {
                cx.stop_timer(self.timer);
                self.shown = false;
                self.over_card = false;
                self.view.redraw(cx);
            }
        }
    }

    fn populate(&mut self, cx: &mut Cx) {
        let Some(info) = self.target.clone() else { return };
        let avatar = self.view.avatar(cx, ids!(card_avatar));
        let drew_image = info.avatar_state.image().is_some_and(|image| {
            avatar.show_image(cx, None, |cx, img| utils::load_avatar_image(&img, cx, image)).is_ok()
        });
        if !drew_image {
            avatar.show_user_text(cx, &info.user_id, info.displayable_name());
        }
        self.view.label(cx, ids!(card_name)).set_text(cx, info.displayable_name());
        self.view.label(cx, ids!(card_user_id)).set_text(cx, info.user_id.as_str());
        self.view.view(cx, ids!(agent_tag)).set_visible(cx, profile_route::is_agent(&info.user_id));
        // No presence source exists for people or Agents yet, so the dot stays hidden.
        self.view.view(cx, ids!(presence_dot)).set_visible(cx, false);
        let own = current_user_id().as_ref() == Some(&info.user_id);
        self.view.button(cx, ids!(message_button)).set_visible(cx, !own);
        for id in [ids!(view_profile_button), ids!(message_button)] {
            self.view.button(cx, id).reset_hover(cx);
        }
        self.populate_details(cx);
    }

    /// The server, then the role in this room:
    /// the same lines and role the right-hand profile shows.
    fn populate_details(&mut self, cx: &mut Cx) {
        let Some(info) = self.target.clone() else { return };
        let route = profile_route::route_for(&info.user_id);
        let mut lines = vec![profile_route::detail_line(&info.user_id, route)];
        let state = member_summary::lookup(cx, &info.user_id, &info.room_id, true);
        let role = member_summary::standing_lines(&state).1;
        if !role.is_empty() { lines.push(role.into_owned()); }
        self.view.label(cx, ids!(card_details)).set_text(cx, &lines.join("\n"));
        self.view.redraw(cx);
    }

    fn handle_buttons(&mut self, cx: &mut Cx, actions: &Actions) {
        let button = if self.view.button(cx, ids!(view_profile_button)).clicked(actions) {
            CardButton::ViewProfile
        } else if self.view.button(cx, ids!(message_button)).clicked(actions) {
            CardButton::Message
        } else {
            return;
        };
        let Some(info) = self.target.clone() else { return };
        let room_name = room_name(cx, &info);
        self.step(cx, HoverInput::Dismiss);
        dispatch(cx, request_for(button, &info, room_name));
    }

    /// Puts the card beside the avatar (right, else left), inside the window.
    fn position_card(&mut self, cx: &mut Cx2d) {
        let window = cx.current_pass_size();
        let overlay = cx.turtle().origin();
        let (left, top) = card_origin(self.anchor, window);
        if let Some(mut frame) = self.view.view(cx, ids!(popup_frame)).borrow_mut() {
            frame.walk.abs_pos = None;
            frame.walk.margin = Inset { left: left - overlay.x, top: top - overlay.y, right: 0.0, bottom: 0.0 };
        }
    }
}

/// The card's top-left for an avatar at `anchor` in a `window`-sized pass.
fn card_origin(anchor: Rect, window: DVec2) -> (f64, f64) {
    let right_side = anchor.pos.x + anchor.size.x + CARD_GAP;
    let left = if right_side + CARD_WIDTH + CARD_GAP <= window.x {
        right_side
    } else {
        (anchor.pos.x - CARD_GAP - CARD_WIDTH).max(CARD_GAP)
    };
    let top = anchor.pos.y.min(window.y - CARD_EST_HEIGHT - CARD_GAP).max(CARD_GAP);
    (left, top)
}

/// Registers the app-level card as a global on Cx.
pub fn set_global_hover_card(cx: &mut Cx, parent_ref: &WidgetRef) {
    Cx::set_global(cx, parent_ref.avatar_hover_card(cx, ids!(avatar_hover_card)));
}

/// Runs `f` on the app-level card, if the app has one (tests and embeds may not).
pub(crate) fn with_card(cx: &mut Cx, f: impl FnOnce(&mut Cx, &mut AvatarHoverCard)) {
    if !cx.has_global::<AvatarHoverCardRef>() {
        return;
    }
    let card = cx.get_global::<AvatarHoverCardRef>().clone();
    if let Some(mut inner) = card.borrow_mut() {
        f(cx, &mut inner);
    }
}

#[cfg(test)]
#[path = "hover_card_tests.rs"]
mod tests;
