//! The small person/Agent card shown while hovering an avatar or a sender
//! name (like Buzz): avatar, name, a small Matrix ID with Copy, the server
//! and room role, and "View profile" / "Message" buttons. An Agent shows its
//! owner, runtime and status ([`super::agent_facts`]) instead; one of this
//! account's own Agents swaps "View profile" for "Agent details".
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
    agent_facts::{self, AgentFacts, AgentFactsChanged},
    member_summary,
    own_agents::OwnAgentsChanged,
    profile_route,
    user_profile::UserProfileAndRoomId,
};

/// Fixed card width, in DIPs (must match `popup_frame.width` below).
const CARD_WIDTH: f64 = 280.0;
/// The card's usual height, used to keep it inside the window.
const CARD_EST_HEIGHT: f64 = 230.0;
/// Space between the avatar and the card, and the card and the window edge.
const CARD_GAP: f64 = 8.0;

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
    /// Set while the card shows an Agent.
    #[rust] facts: Option<AgentFacts>,
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
        if let Event::Actions(actions) = event
            && actions.iter().any(|a| a.downcast_ref::<AgentFactsChanged>().is_some() || a.downcast_ref::<OwnAgentsChanged>().is_some())
        {
            self.populate(cx);
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
        // An Agent's owner and status are per room: another room is a switch too.
        let switched = self.target.as_ref().is_none_or(|t| t.user_id != info.user_id || t.room_id != info.room_id);
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
        let agent = profile_route::is_agent(&info.user_id);
        self.view.view(cx, ids!(agent_tag)).set_visible(cx, agent);
        // No presence source exists for people yet, so the dot stays hidden.
        self.view.view(cx, ids!(presence_dot)).set_visible(cx, false);
        self.facts = agent.then(|| agent_facts::facts(&info.user_id, &info.room_id));
        self.show_facts(cx);
    }

    /// The buttons and detail lines for what is known; an own Agent swaps
    /// "View profile" for "Agent details".
    fn show_facts(&mut self, cx: &mut Cx) {
        let Some(info) = self.target.clone() else { return };
        let own_agent = self.own_agent().is_some();
        self.view.button(cx, ids!(view_profile_button)).set_visible(cx, !own_agent);
        self.view.button(cx, ids!(agent_details_button)).set_visible(cx, own_agent);
        let own = current_user_id().as_ref() == Some(&info.user_id);
        self.view.button(cx, ids!(message_button)).set_visible(cx, !own);
        for id in [ids!(view_profile_button), ids!(agent_details_button), ids!(message_button), ids!(copy_id_button)] {
            self.view.button(cx, id).reset_hover(cx);
        }
        self.populate_details(cx);
    }

    fn own_agent(&self) -> Option<&str> {
        self.facts.as_ref()?.own_agent_id.as_deref()
    }

    /// A person's server, an Agent's owner/runtime/status, then any room
    /// role: the same lines the right-hand profile shows.
    fn populate_details(&mut self, cx: &mut Cx) {
        let Some(info) = self.target.clone() else { return };
        let route = profile_route::route_for(&info.user_id);
        let mut lines = match self.facts.as_ref() {
            Some(facts) => {
                let owner = facts.owner.as_ref().map(|owner| agent_facts::owner_name(cx, owner, Some(&info.room_id)));
                agent_facts::fact_lines(facts, owner.as_deref(), false)
            }
            None => vec![profile_route::detail_line(&info.user_id, route)],
        };
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
        } else if self.view.button(cx, ids!(agent_details_button)).clicked(actions) {
            CardButton::AgentDetails
        } else if self.view.button(cx, ids!(copy_id_button)).clicked(actions) {
            CardButton::CopyId
        } else {
            return;
        };
        let Some(info) = self.target.clone() else { return };
        let room_name = room_name(cx, &info);
        // Re-check ownership now: the account may have changed since the card opened.
        let own_agent = super::own_agents::agent_for(&info.user_id).filter(|id| self.own_agent() == Some(id.as_str()));
        let Some(request) = request_for(button, &info, room_name, own_agent.as_deref()) else { return };
        // Copying keeps the card open; every other button leaves it.
        if button != CardButton::CopyId { self.step(cx, HoverInput::Dismiss); }
        dispatch(cx, request);
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
