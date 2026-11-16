//! The one-click prompt for an Agent whose "Join rooms I join" is off:
//! "Bring <agent> into <room>?" [Not now] [Bring]. The pair was recorded
//! before the prompt opened, so either answer is final for that room.
use super::{AgentPick, bind_into_rooms, offer_text};
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;
use std::collections::VecDeque;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Action = RobrixNeutralIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Primary = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}

    mod.widgets.BringAgentPrompt = #(BringAgentPrompt::register_widget(vm)) {
        ..mod.widgets.SmallModal
        width: Fill{max: 420} spacing: 14
        question := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
        ModalButtonsRow {
            not_now := Action {text: #(crate::i18n::tr("Not now")) i18n_text: "Not now"}
            bring := Primary {text: #(crate::i18n::tr("Bring")) i18n_text: "Bring"}
        }
    }
}

/// Shows or closes the prompt; the app shell owns its modal.
#[derive(Clone, Debug)]
pub enum BringAgentAction {
    Offer { room_id: OwnedRoomId, room_name: String, agent: AgentPick },
    Close,
}

type Offer = (OwnedRoomId, String, AgentPick);

#[derive(Script, ScriptHook, Widget)]
pub struct BringAgentPrompt {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    /// The offer on screen first, then those waiting their turn.
    #[rust] offers: VecDeque<Offer>,
}
impl Widget for BringAgentPrompt {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if self.view.button(cx, ids!(bring)).clicked(actions) { self.answer(cx, true); }
        if self.view.button(cx, ids!(not_now)).clicked(actions) { self.answer(cx, false); }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl BringAgentPrompt {
    /// Bring binds the Agent; either answer moves on to the next offer.
    fn answer(&mut self, cx: &mut Cx, bring: bool) {
        let Some((room_id, room_name, agent)) = self.offers.pop_front() else { return };
        if bring {
            bind_into_rooms(agent.name, agent.puppet, vec![(room_id.to_string(), room_name)]);
        }
        if self.show_front(cx) { self.view.redraw(cx); } else { cx.action(BringAgentAction::Close); }
    }
    /// Puts the first waiting offer on screen; false when none is left.
    fn show_front(&mut self, cx: &mut Cx) -> bool {
        let Some((_, room_name, agent)) = self.offers.front() else { return false };
        let text = offer_text(&agent.name, room_name);
        self.view.label(cx, ids!(question)).set_text(cx, &text);
        true
    }
}

impl BringAgentPromptRef {
    pub fn action(&self, cx: &mut Cx, modal: ModalRef, action: &BringAgentAction) {
        let Some(mut inner) = self.borrow_mut() else { return };
        match action {
            BringAgentAction::Offer { room_id, room_name, agent } => {
                inner.offers.push_back((room_id.clone(), room_name.clone(), agent.clone()));
                if inner.offers.len() == 1 && inner.show_front(cx) { modal.open(cx); }
            }
            BringAgentAction::Close => {
                inner.offers.clear();
                modal.close(cx);
            }
        }
    }
}

/// The app shell's one hook: sends `action` to the prompt in `bring_agent_modal`.
pub fn route(cx: &mut Cx, ui: &WidgetRef, action: &BringAgentAction) {
    let modal = ui.modal(cx, ids!(bring_agent_modal));
    ui.bring_agent_prompt(cx, ids!(bring_agent_modal.content)).action(cx, modal, action);
}
