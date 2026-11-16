//! Settings tab, Rooms card: "Join rooms I join". On, this Agent follows its
//! owner into group rooms they join or create; off, the owner gets a
//! one-click "Bring <agent> into <room>?" instead. Kept on this desktop per
//! account and per Agent (`bring_agent` prefs); on unless turned off.
use super::*;

impl AgentPanel {
    /// Shows this Agent's saved setting; called when the panel opens.
    pub(super) fn fill_room_follow(&mut self, cx: &mut Cx) {
        let on = crate::hagency::bring_agent::joins_rooms(&self.agent_id());
        self.view.check_box(cx, ids!(join_rooms)).set_active(cx, on, Animate::No);
    }

    pub(super) fn handle_room_follow_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(on) = self.view.check_box(cx, ids!(join_rooms)).changed(actions) else { return };
        match crate::hagency::bring_agent::save_joins_rooms(&self.agent_id(), on) {
            Ok(()) => self.note(cx, "Saved."),
            Err(code) => {
                self.view.label(cx, ids!(status)).set_text(cx, &crate::i18n::format("Not saved: {code}", &[("code", code)]));
                self.fill_room_follow(cx);
            }
        }
    }
}
