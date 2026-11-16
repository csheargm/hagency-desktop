//! The proposal card an Agent's `memory_propose` posts: the proposed text, why,
//! and Accept / Dismiss. Accept is offered only to members who may write room
//! memory, under a "Your decision" line so they read as the person's choice,
//! not the Agent's; a message that does not parse stays an ordinary notice.
use super::model::{self, CardState, Proposal};
use super::outcome::{Announce, Kind, Origin};
use super::{auto, ops, store};
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.MemoryProposalCard = #(MemoryProposalCard::register_widget(vm)) {
        ..mod.widgets.RoundedView
        visible: false width: Fill height: Fit flow: Down spacing: 6
        margin: Inset{top: 8 right: 10} padding: 12
        draw_bg +: {color: (mod.widgets.RBX_BG_SURFACE) border_radius: 4}
        memory_title := Label {
            width: Fill height: Fit
            text: #(crate::i18n::tr("Memory suggestion")) i18n_text: "Memory suggestion"
            draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        memory_text := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_PRIMARY)}
        memory_reason := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_SECONDARY)}
        memory_status := Label {visible: false width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_SECONDARY)}
        memory_decision := Label {
            visible: false width: Fill height: Fit
            text: #(crate::i18n::tr("Your decision (room moderators):")) i18n_text: "Your decision (room moderators):"
            draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: theme.font_bold{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        memory_buttons := View {width: Fill height: Fit flow: Right spacing: 8
            memory_accept := RobrixPositiveIconButton {
                width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("Accept")) i18n_text: "Accept"
            }
            memory_dismiss := RobrixNeutralIconButton {
                width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("Dismiss")) i18n_text: "Dismiss"
            }
        }
        memory_auto_on := RobrixNeutralIconButton {
            visible: false width: Fit height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
            text: #(crate::i18n::tr("Always accept from my Agents")) i18n_text: "Always accept from my Agents"
        }
        memory_auto_is_on := Label {
            visible: false width: Fill height: Fit
            text: #(crate::i18n::tr("Auto-accept is on")) i18n_text: "Auto-accept is on"
            draw_text.color: (mod.widgets.RBX_FG_SECONDARY)
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct MemoryProposalCard {
    #[deref]
    pub view: View,
    #[rust]
    pub context: Option<(OwnedRoomId, Proposal)>,
    /// The proposal message, for the outcome reply posted on Accept.
    #[rust]
    pub origin: Option<Origin>,
}

/// What the card says under the text, given its state and the viewer's power.
pub fn status_text(state: &CardState, can_write: bool) -> Option<String> {
    let tr = crate::i18n::tr;
    match state {
        CardState::Added => Some(tr("Added to room memory").to_owned()),
        CardState::Dismissed => Some(tr("Dismissed").to_owned()),
        CardState::Adding => Some(tr("Adding to room memory…").to_owned()),
        CardState::Failed(message) => Some(message.clone()),
        CardState::Pending if !can_write => Some(tr("Waiting for a member with permission to accept.").to_owned()),
        CardState::Pending => None,
    }
}

impl MemoryProposalCard {
    pub(super) fn sync(&mut self, cx: &mut Cx) {
        let Some((room, proposal)) = self.context.as_ref() else {
            return self.view.set_visible(cx, false);
        };
        let me = store::me();
        let (state, can_write) = {
            let mut store = store::lock();
            let state = store.card_state(room, me.as_deref(), &proposal.entry.id);
            (state, store.loaded(room, me.as_deref()).is_some_and(|l| l.can_write()))
        };
        if auto::claim(room, proposal, &state, can_write) {
            return self.accept();
        }
        self.view.set_visible(cx, true);
        self.view.label(cx, ids!(memory_text)).set_text(cx, &proposal.entry.text);
        self.view.label(cx, ids!(memory_reason)).set_visible(cx, !proposal.reason.is_empty());
        self.view.label(cx, ids!(memory_reason)).set_text(cx, &crate::i18n::format("Reason: {reason}", &[("reason", proposal.reason.clone())]));
        let status = match state {
            CardState::Added if auto::was_auto(room, &proposal.entry.id) => Some(crate::i18n::tr("Added to room memory automatically").to_owned()),
            _ => status_text(&state, can_write),
        };
        let my_agent = auto::is_my_agent(room, &proposal.entry.author);
        let offer = auto::offer(can_write, my_agent, auto::is_on(room, me.as_deref()), state == CardState::Pending);
        self.show_controls(cx, &state, can_write, status, offer);
    }
    fn show_controls(&mut self, cx: &mut Cx, state: &CardState, can_write: bool, status: Option<String>, offer: auto::Offer) {
        self.view.label(cx, ids!(memory_status)).set_visible(cx, status.is_some());
        self.view.label(cx, ids!(memory_status)).set_text(cx, status.as_deref().unwrap_or_default());
        self.view.label(cx, ids!(memory_decision)).set_visible(cx, model::can_accept(state, can_write));
        self.view.button(cx, ids!(memory_accept)).set_visible(cx, model::can_accept(state, can_write));
        self.view.button(cx, ids!(memory_dismiss)).set_visible(cx, model::can_dismiss(state));
        self.view.view(cx, ids!(memory_buttons)).set_visible(cx, model::can_dismiss(state));
        self.view.button(cx, ids!(memory_auto_on)).set_visible(cx, offer == auto::Offer::TurnOn);
        self.view.label(cx, ids!(memory_auto_is_on)).set_visible(cx, offer == auto::Offer::IsOn);
        self.view.redraw(cx);
    }
    fn accept(&self) {
        let Some((room, proposal)) = self.context.clone() else { return };
        let id = proposal.entry.id.clone();
        let announce = self.origin.clone().map(|origin| Announce { origin, kind: Kind::Memory, proposal: id.clone() });
        ops::submit_announced(&room, &id, Box::new(move |loaded| {
            let existing = loaded.entries.iter().find(|e| e.id == proposal.entry.id);
            model::accept_plan(&proposal, existing, &loaded.me, &loaded.levels, loaded.storage)
        }), announce);
    }
}

impl Widget for MemoryProposalCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some((room, proposal)) = self.context.clone() else { return };
        if actions.iter().any(|a| a.downcast_ref::<store::MemoryChanged>().is_some_and(|c| c.room_id == room)) {
            return self.sync(cx);
        }
        if self.view.button(cx, ids!(memory_accept)).clicked(actions) {
            self.accept();
        } else if self.view.button(cx, ids!(memory_dismiss)).clicked(actions) {
            ops::dismiss(&room, &proposal.entry.id);
        } else if self.view.button(cx, ids!(memory_auto_on)).clicked(actions) {
            // The next sync accepts this card too, like the panel's switch.
            auto::switch(&room, true);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl MemoryProposalCardRef {
    /// Shows the card for `proposal`, or hides it (`None`: a plain notice).
    pub fn show(&self, cx: &mut Cx, room: &matrix_sdk::ruma::RoomId, proposal: Option<Proposal>, origin: Option<Origin>) {
        let Some(mut card) = self.borrow_mut() else { return };
        if proposal.is_some() {
            ops::ensure_loaded(room);
        }
        card.origin = origin;
        card.context = proposal.map(|p| (room.to_owned(), p));
        card.sync(cx);
    }
}
