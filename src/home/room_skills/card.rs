//! The proposal card an Agent's `skill_propose` posts: the skill's name,
//! description, the start of its body, why, and Accept / Dismiss. Accept is
//! offered only to members who may write skills; Dismiss is local, sharing
//! memory's per-account dismissals.
use super::model::{self, Proposal};
use super::ops;
use crate::home::room_memory::model::{CardState, can_accept, can_dismiss, card_state};
use crate::home::room_memory::store as memory_store;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.SkillProposalCard = #(SkillProposalCard::register_widget(vm)) {
        ..mod.widgets.RoundedView
        visible: false width: Fill height: Fit flow: Down spacing: 6
        margin: Inset{top: 8 right: 10} padding: 12
        draw_bg +: {color: (mod.widgets.RBX_BG_SURFACE) border_radius: 4}
        skill_title := Label {
            width: Fill height: Fit
            draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        skill_summary := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_PRIMARY)}
        skill_preview := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_SECONDARY)}
        skill_status := Label {visible: false width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_SECONDARY)}
        skill_buttons := View {width: Fill height: Fit flow: Right spacing: 8
            skill_accept := RobrixPositiveIconButton {
                width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("Accept")) i18n_text: "Accept"
            }
            skill_dismiss := RobrixNeutralIconButton {
                width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("Dismiss")) i18n_text: "Dismiss"
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SkillProposalCard {
    #[deref]
    pub view: View,
    #[rust]
    pub context: Option<(OwnedRoomId, Proposal)>,
}

/// Dismissals share memory's local store under their own namespace.
fn dismissal_key(proposal: &Proposal) -> String {
    format!("skill:{}", proposal.id)
}
const PREVIEW_CHARS: usize = 280;

/// The card's state and whether this person may accept it.
pub fn state_of(room: &RoomId, me: Option<&str>, proposal: &Proposal) -> (CardState, bool) {
    let (applied, can_write, working, failure) = {
        let store = ops::lock();
        let loaded = store.current(room, me);
        let working = store.working.contains(&(room.to_owned(), proposal.id.clone()));
        (loaded.is_some_and(|l| model::is_applied(proposal, &l.skills)), loaded.is_some_and(|l| l.can_write()), working, store.errors.get(room).cloned())
    };
    let dismissed = memory_store::lock().is_dismissed(room, me, &dismissal_key(proposal));
    (card_state(applied, dismissed, working, failure.as_deref()), can_write)
}

fn status_text(state: &CardState, can_write: bool) -> Option<String> {
    let tr = crate::i18n::tr;
    match state {
        CardState::Added => Some(tr("Saved as a team skill").to_owned()),
        CardState::Dismissed => Some(tr("Dismissed").to_owned()),
        CardState::Adding => Some(tr("Saving team skill…").to_owned()),
        CardState::Failed(message) => Some(message.clone()),
        CardState::Pending if !can_write => Some(tr("Waiting for a member with permission to accept.").to_owned()),
        CardState::Pending => None,
    }
}

impl SkillProposalCard {
    fn sync(&mut self, cx: &mut Cx) {
        let Some((room, proposal)) = self.context.as_ref() else {
            return self.view.set_visible(cx, false);
        };
        let (state, can_write) = state_of(room, memory_store::me().as_deref(), proposal);
        let skill = &proposal.skill;
        self.view.set_visible(cx, true);
        self.view.label(cx, ids!(skill_title)).set_text(cx, &crate::i18n::format("Team skill suggestion: {name}", &[("name", skill.name.clone())]));
        self.view.label(cx, ids!(skill_summary)).set_text(cx, &skill.description);
        let mut preview: String = skill.body.chars().take(PREVIEW_CHARS).collect();
        if skill.body.chars().count() > PREVIEW_CHARS {
            preview.push('…');
        }
        let reason = crate::i18n::format("Reason: {reason}", &[("reason", proposal.reason.clone())]);
        self.view.label(cx, ids!(skill_preview)).set_text(cx, &format!("{preview}\n{reason}"));
        let status = status_text(&state, can_write);
        self.view.label(cx, ids!(skill_status)).set_visible(cx, status.is_some());
        self.view.label(cx, ids!(skill_status)).set_text(cx, status.as_deref().unwrap_or_default());
        self.view.button(cx, ids!(skill_accept)).set_visible(cx, can_accept(&state, can_write));
        self.view.view(cx, ids!(skill_buttons)).set_visible(cx, can_dismiss(&state));
        self.view.redraw(cx);
    }
    fn accept(&self) {
        let Some((room, proposal)) = self.context.clone() else { return };
        let id = proposal.id.clone();
        ops::submit(&room, &id, Box::new(move |loaded| {
            let existing = loaded.skills.iter().find(|s| s.name == proposal.skill.name);
            model::accept_plan(&proposal, existing, &loaded.me, &loaded.levels, loaded.storage)
        }));
    }
}

impl Widget for SkillProposalCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some((room, proposal)) = self.context.clone() else { return };
        let ours = |a: &Action| {
            a.downcast_ref::<ops::SkillsChanged>().is_some_and(|c| c.room_id == room)
                || a.downcast_ref::<memory_store::MemoryChanged>().is_some_and(|c| c.room_id == room)
        };
        if actions.iter().any(ours) {
            return self.sync(cx);
        }
        if self.view.button(cx, ids!(skill_accept)).clicked(actions) {
            self.accept();
        } else if self.view.button(cx, ids!(skill_dismiss)).clicked(actions) {
            crate::home::room_memory::ops::dismiss(&room, &dismissal_key(&proposal));
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SkillProposalCardRef {
    /// Shows the card for `proposal`, or hides it (`None`: not a skill card).
    pub fn show(&self, cx: &mut Cx, room: &RoomId, proposal: Option<Proposal>) {
        let Some(mut card) = self.borrow_mut() else { return };
        if proposal.is_some() {
            ops::ensure_loaded(room);
        }
        card.context = proposal.map(|p| (room.to_owned(), p));
        card.sync(cx);
    }
}
