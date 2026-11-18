//! Settings tab, "In group rooms" card (ADR 0024): for one of the Agent's
//! group rooms at a time, what it does for the room's members without asking
//! the owner (answer questions, review PRs, fix issues). Saved through the
//! approval settings call into the owner's ledger; the kill switch and the
//! budget stay where they are.
use super::mapping;
use super::*;
use crate::hagency::room_share::{self, RoomShare};

impl AgentPanel {
    /// The group room the card edits, as `(id, name)`.
    fn share_room(&self, cx: &mut Cx) -> Option<(String, String)> {
        let rooms = mapping::watch_rooms(&self.status);
        let index = self.view.drop_down(cx, ids!(share_room)).selected_item();
        rooms.get(index).or(rooms.first()).cloned()
    }

    pub(super) fn render_sharing(&mut self, cx: &mut Cx, editable: bool) {
        let rooms = mapping::watch_rooms(&self.status);
        let labels: Vec<String> = if rooms.is_empty() {
            vec![crate::i18n::tr("Not in a group room yet").into()]
        } else {
            rooms.iter().map(|(_, name)| crate::i18n::format("In {room}", &[("room", name.clone())])).collect()
        };
        let selected = self.view.drop_down(cx, ids!(share_room)).selected_item().min(labels.len() - 1);
        self.view.drop_down(cx, ids!(share_room)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(share_room)).set_selected_item(cx, selected);
        self.view.widget(cx, ids!(share_room)).set_disabled(cx, rooms.len() < 2);
        let room = self.share_room(cx);
        let share = room.as_ref().and_then(|(id, _)| room_share::for_room(&self.status["approvals"], id));
        let shown = share.unwrap_or(room_share::DEFAULT);
        for (id, on) in [(ids!(share_answer), shown.answer), (ids!(share_review), shown.review), (ids!(share_fix), shown.fix)] {
            self.view.check_box(cx, id).set_active(cx, on, Animate::No);
            self.view.widget(cx, id).set_disabled(cx, !editable || share.is_none());
        }
        let hint = match (&room, share) {
            (None, _) => crate::i18n::tr("Add this agent to a group room to share it with the room's members.").to_owned(),
            (Some((_, name)), Some(share)) => {
                let owner = self.owner_id().map(|id| crate::profile::agent_facts::owner_name(cx, &id, None));
                room_share::summary(share, name, owner.as_deref().unwrap_or(crate::i18n::tr("its owner")))
            }
            (Some(_), None) => crate::i18n::tr("Sharing choices are kept on the device that runs this agent.").to_owned(),
        };
        self.view.label(cx, ids!(share_hint)).set_text(cx, &hint);
    }

    /// This account: the owner of every Agent the panel shows.
    fn owner_id(&self) -> Option<matrix_sdk::ruma::OwnedUserId> {
        self.agent["ownerMxid"].as_str().and_then(|id| matrix_sdk::ruma::OwnedUserId::try_from(id).ok())
            .or_else(crate::sliding_sync::current_user_id)
    }

    pub(super) fn handle_sharing_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.drop_down(cx, ids!(share_room)).selected(actions).is_some() {
            self.render(cx);
        }
        let changed = [ids!(share_answer), ids!(share_review), ids!(share_fix)].into_iter().any(|id| self.view.check_box(cx, id).changed(actions).is_some());
        if !changed {
            return;
        }
        let Some((room, _)) = self.share_room(cx) else { return };
        let share = RoomShare {
            answer: self.view.check_box(cx, ids!(share_answer)).active(cx),
            review: self.view.check_box(cx, ids!(share_review)).active(cx),
            fix: self.view.check_box(cx, ids!(share_fix)).active(cx),
        };
        self.save_approvals(cx, room_share::change(&room, share));
    }
}
