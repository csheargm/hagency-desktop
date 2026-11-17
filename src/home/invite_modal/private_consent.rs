//! The Add agent picker's private-Agent question. The server refuses a
//! private Agent's bind into a group room; the picker then asks the same
//! "Allow in group rooms?" the other Add agent paths ask
//! (`bring_agent::consent`), in place. Allow records the choice and binds;
//! Cancel binds nothing and says so. It never sends the owner elsewhere.
use super::{InviteModal, InviteModalState, person_label};
use crate::home::room_screen::InviteResultAction;
use makepad_widgets::*;
use ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};
use std::cell::RefCell;

thread_local! {
    /// The Agent bind the picker sent last; its refusal is answered here, not by a popup.
    static PICKER_BIND: RefCell<Option<(OwnedRoomId, OwnedUserId)>> = const { RefCell::new(None) };
}

/// Marks the bind the picker just sent (or clears it with `None`).
pub(super) fn mark_picker_bind(bind: Option<(OwnedRoomId, OwnedUserId)>) {
    PICKER_BIND.with(|cell| *cell.borrow_mut() = bind);
}

/// Whether the picker answers this bind's outcome itself.
pub(crate) fn picker_owns(room_id: &RoomId, user_id: &UserId) -> bool {
    PICKER_BIND.with(|cell| cell.borrow().as_ref().is_some_and(|(r, u)| r == room_id && u == user_id))
}

/// An Agent's name for the question: its registry name, else the ID's localpart.
pub(crate) fn agent_label(user_id: &UserId) -> String {
    let settings = crate::agent_access::current();
    let name = settings.agent_registry.agents().find(|(id, _)| id.as_str() == user_id.as_str()).and_then(|(_, e)| e.display_name.clone());
    person_label(user_id, name.as_deref())
}

impl InviteModal {
    fn agent_name(&self, user: &UserId) -> String {
        let name = self.offered.iter().chain(&self.known).find(|(id, _)| id == user).and_then(|(_, n)| n.clone());
        if name.is_some() { person_label(user, name.as_deref()) } else { agent_label(user) }
    }

    /// The refused private Agent: ask "Allow in group rooms?" in the picker.
    pub(super) fn ask_consent(&mut self, cx: &mut Cx, user: OwnedUserId) {
        let room = self.room_name_id.as_ref().map(|r| r.to_string()).unwrap_or_default();
        let text = crate::hagency::bring_agent::consent_text(&self.agent_name(&user), &room);
        self.state = InviteModalState::ConsentNeeded(user);
        self.status(cx, &text, false);
        let confirm = self.view.button(cx, ids!(confirm_button));
        confirm.set_visible(cx, true);
        confirm.set_enabled(cx, true);
        confirm.set_text(cx, crate::i18n::tr("Allow in group rooms"));
        self.view.button(cx, ids!(cancel_button)).set_visible(cx, true);
        self.view.text_input(cx, ids!(user_id_input)).set_is_read_only(cx, true);
        self.view.redraw(cx);
    }

    /// The owner's answer. Allow returns the bind to start; Cancel binds
    /// nothing, says so and leaves the picker open for another choice.
    pub(super) fn answer_consent(&mut self, cx: &mut Cx, allow: bool) -> Option<(OwnedRoomId, OwnedUserId)> {
        let InviteModalState::ConsentNeeded(user) = self.state.clone() else { return None };
        let room = self.room_name_id.clone()?;
        let confirm = self.view.button(cx, ids!(confirm_button));
        if allow {
            self.state = InviteModalState::WaitingForInvite(user.clone());
            self.status(cx, crate::i18n::tr("Adding Agent to this room…"), false);
            confirm.set_enabled(cx, false);
            return Some((room.room_id().clone(), user));
        }
        let args = [("agent", self.agent_name(&user)), ("room", room.to_string())];
        self.state = InviteModalState::WaitingForUserInput;
        mark_picker_bind(None);
        self.status(cx, &crate::i18n::format("{agent} was not added to {room}. It stays private: it only answers in your private chat with it.", &args), false);
        confirm.set_text(cx, crate::i18n::tr("Invite"));
        self.view.text_input(cx, ids!(user_id_input)).set_is_read_only(cx, false);
        self.update_invite_button(cx);
        self.view.redraw(cx);
        None
    }
}

/// Records "Allow in group rooms" and binds; the picker shows the outcome.
pub(super) fn start_allowed_bind(room_id: OwnedRoomId, user_id: OwnedUserId, epoch: u64) {
    let (room, puppet) = (room_id.to_string(), user_id.to_string());
    crate::hagency::bring_agent::allow_into_room(puppet, room, move |result| {
        if crate::account_session::epoch() == epoch {
            Cx::post_action(InviteResultAction::Agent { room_id, user_id, epoch, result });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::group_rooms::PRIVATE_AGENT_REFUSAL;

    fn modal(cx: &mut Cx) -> InviteModal {
        let mut modal = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.InviteModal {}});
            InviteModal::script_from_value(vm, value)
        });
        modal.epoch = crate::account_session::epoch();
        let room: OwnedRoomId = "!dev:test".try_into().unwrap();
        modal.room_name_id = Some(crate::utils::RoomNameId::new(matrix_sdk::RoomDisplayName::Named("dev".into()), room));
        modal.known = vec![(agent(), Some("yue-claude".into()))];
        modal
    }
    fn agent() -> OwnedUserId { "@_hagency_agt_01m7:test".try_into().unwrap() }
    fn refused(modal: &mut InviteModal, cx: &mut Cx) {
        modal.state = InviteModalState::WaitingForInvite(agent());
        let room_id = modal.room_name_id.as_ref().unwrap().room_id().clone();
        let result = InviteResultAction::Agent { room_id, user_id: agent(), epoch: modal.epoch, result: Err(PRIVATE_AGENT_REFUSAL.into()) };
        modal.finish_invite(cx, &result);
    }
    fn status(modal: &InviteModal, cx: &mut Cx) -> String {
        modal.view.label(cx, ids!(status_label_view.status_label)).text()
    }

    /// Regression (Chris, dev room): the picker said "open Room info → Members"
    /// instead of asking. Private Agent → consent → Allow → bound and joined.
    #[test]
    fn a_private_agent_is_asked_then_allowed_and_bound() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut modal = modal(&mut cx);
        refused(&mut modal, &mut cx);
        assert_eq!(modal.state, InviteModalState::ConsentNeeded(agent()));
        let asked = status(&modal, &mut cx);
        assert!(asked.contains("yue-claude") && asked.contains("dev") && asked.contains("pool"), "{asked}");
        assert!(!asked.contains("Room info") && !asked.contains("_hagency_agt_"), "never sends the owner elsewhere: {asked}");
        assert_eq!(modal.view.button(&cx, ids!(confirm_button)).text(), "Allow in group rooms");
        let bind = modal.answer_consent(&mut cx, true);
        assert_eq!(bind, Some(("!dev:test".try_into().unwrap(), agent())));
        assert_eq!(modal.state, InviteModalState::WaitingForInvite(agent()));
        let room_id = modal.room_name_id.as_ref().unwrap().room_id().clone();
        modal.finish_invite(&mut cx, &InviteResultAction::Agent { room_id, user_id: agent(), epoch: modal.epoch, result: Ok(true) });
        assert_eq!(modal.state, InviteModalState::InviteSuccess);
        assert!(status(&modal, &mut cx).contains("Agent joined this room"));
    }

    #[test]
    fn declining_binds_nothing_and_says_so() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut modal = modal(&mut cx);
        refused(&mut modal, &mut cx);
        assert_eq!(modal.answer_consent(&mut cx, false), None, "Cancel starts no bind");
        assert_eq!(modal.state, InviteModalState::WaitingForUserInput);
        let told = status(&modal, &mut cx);
        assert!(told.contains("yue-claude was not added to dev") && told.contains("private"), "{told}");
        assert_eq!(modal.answer_consent(&mut cx, true), None, "no question is open any more");
    }

    #[test]
    fn only_the_pickers_own_bind_is_answered_in_the_picker() {
        let room: OwnedRoomId = "!dev:test".try_into().unwrap();
        mark_picker_bind(Some((room.clone(), agent())));
        assert!(picker_owns(&room, &agent()));
        assert!(!picker_owns(&room, ruma::user_id!("@_hagency_agt_other:test")));
        mark_picker_bind(None);
        assert!(!picker_owns(&room, &agent()));
    }

}
