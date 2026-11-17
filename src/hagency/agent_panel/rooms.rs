//! Settings tab, Rooms card: "Join rooms I join". On, this Agent follows its
//! owner into group rooms they join or create; off, the owner gets a
//! one-click "Bring <agent> into <room>?" instead. Kept on this desktop per
//! account and per Agent (`bring_agent` prefs); on unless turned off.
//! Context tab, Rooms card: a ticked row per room the Agent is in; unticking
//! one, or "Remove my agent from this room", makes the Agent leave it.
use super::*;

impl AgentPanel {
    /// Shows this Agent's saved setting; called when the panel opens.
    pub(super) fn fill_room_follow(&mut self, cx: &mut Cx) {
        let on = crate::hagency::bring_agent::joins_rooms(&self.agent_id());
        self.view.check_box(cx, ids!(join_rooms)).set_active(cx, on, Animate::No);
        if let Some(notice) = crate::hagency::bring_agent::catch_up_notice(&self.agent_id()) {
            self.view.label(cx, ids!(status)).set_text(cx, &notice);
        }
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

/// The room checklist rows in the Context tab's DSL, top to bottom.
const ROOM_TICKS: [LiveId; 8] = [
    live_id!(room_tick_0), live_id!(room_tick_1), live_id!(room_tick_2), live_id!(room_tick_3),
    live_id!(room_tick_4), live_id!(room_tick_5), live_id!(room_tick_6), live_id!(room_tick_7),
];

/// A room the Agent is in, as `(binding id, room id, name)`.
pub(crate) fn joined_rooms(status: &Value) -> Vec<(String, String, String)> {
    status["rooms"].as_array().into_iter().flatten()
        .filter(|room| room["state"] == "active")
        .filter_map(|room| {
            let binding = room["id"].as_str().filter(|id| !id.is_empty())?;
            let id = room["roomId"].as_str().filter(|id| id.starts_with('!'))?;
            let name = room["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
            Some((binding.to_owned(), id.to_owned(), name.to_owned()))
        })
        .collect()
}

/// The binding that holds the open room, when the Agent is in it.
pub(crate) fn open_room_binding(rooms: &[(String, String, String)], open: Option<&str>) -> Option<String> {
    rooms.iter().find(|(_, room, _)| Some(room.as_str()) == open).map(|(binding, _, _)| binding.clone())
}

fn open_room_id() -> Option<String> {
    crate::home::sidebar_action_menu::plus_menu::open_room().map(|room| room.room_id().to_string())
}

impl AgentPanel {
    /// One ticked row per room the Agent is in, and the open room's Remove.
    pub(super) fn render_room_ticks(&mut self, cx: &mut Cx) {
        let rooms = joined_rooms(&self.status);
        for (index, id) in ROOM_TICKS.iter().enumerate() {
            let row = self.view.widget(cx, &[*id]);
            row.set_visible(cx, index < rooms.len());
            if let Some((_, _, name)) = rooms.get(index) {
                row.set_text(cx, name);
                self.view.check_box(cx, &[*id]).set_active(cx, true, Animate::No);
            }
        }
        let open = open_room_binding(&rooms, open_room_id().as_deref());
        let leave = self.view.button(cx, ids!(leave_open_room));
        leave.set_visible(cx, open.is_some());
        leave.set_enabled(cx, !self.busy);
        self.render_add_room(cx);
    }

    /// Unticking a room, or Remove for the open room, makes the Agent leave it.
    pub(super) fn handle_room_leave_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let rooms = joined_rooms(&self.status);
        let mut binding = ROOM_TICKS.iter().enumerate().find_map(|(index, id)| {
            (self.view.check_box(cx, &[*id]).changed(actions) == Some(false)).then(|| rooms.get(index).map(|r| r.0.clone())).flatten()
        });
        if binding.is_none() && self.view.button(cx, ids!(leave_open_room)).clicked(actions) {
            binding = open_room_binding(&rooms, open_room_id().as_deref());
        }
        let Some(binding_id) = binding else { return };
        self.note(cx, "Removing the agent from the room…");
        let command = crate::hagency::model::Command::Binding {
            agent_id: self.agent_id(), binding_id, action: crate::hagency::model::BindingAction::Leave,
        };
        self.command(Load::Left, true, command);
        self.render(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live finding: an owner could add an Agent to a room but not take it out.
    #[test]
    fn the_checklist_lists_active_rooms_and_finds_the_open_rooms_binding() {
        let status = serde_json::json!({"rooms": [
            {"id": "b1", "roomId": "!a:s", "name": "Ops", "state": "active"},
            {"id": "b2", "roomId": "!b:s", "name": "", "state": "active"},
            {"id": "b3", "roomId": "!c:s", "name": "Old", "state": "left"},
        ]});
        let rooms = joined_rooms(&status);
        assert_eq!(rooms, vec![("b1".into(), "!a:s".into(), "Ops".into()), ("b2".into(), "!b:s".into(), "!b:s".into())]);
        assert_eq!(open_room_binding(&rooms, Some("!b:s")).as_deref(), Some("b2"));
        assert_eq!(open_room_binding(&rooms, Some("!c:s")), None, "a room it left offers no Remove");
        assert_eq!(open_room_binding(&rooms, None), None);
    }
}
