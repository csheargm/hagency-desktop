//! Context tab, Rooms card: "Add to a room…". Lists the owner's joined group
//! rooms (no DMs, no spaces) the Agent is not in; picking one binds it through
//! the Add agent path (`bring_agent::bind_into_rooms_then` →
//! `Service::invite_agent_to_room`), so a private Agent still gets its
//! "Allow in group rooms?" prompt and every outcome its popup.
use super::*;

/// The owner's rooms the Agent can be added to: not bound in any state but
/// left, and not its private chat. `owner_rooms` are `(room id, name)`.
pub(super) fn rooms_to_add(owner_rooms: &[(String, String)], status: &Value) -> Vec<(String, String)> {
    let bound: Vec<&str> = status["rooms"].as_array().into_iter().flatten()
        .filter(|room| room["state"] != "left")
        .filter_map(|room| room["roomId"].as_str())
        .chain(status["chat"]["roomId"].as_str())
        .collect();
    let mut rooms: Vec<(String, String)> = owner_rooms.iter().filter(|(id, _)| !bound.contains(&id.as_str())).cloned().collect();
    rooms.sort_by_cached_key(|(id, name)| (name.to_lowercase(), id.clone()));
    rooms.dedup_by(|a, b| a.0 == b.0);
    rooms
}

impl AgentPanel {
    /// Reads the owner's joined group rooms for this opening.
    pub(super) fn load_owner_rooms(&mut self) {
        self.spawn(Load::OwnerRooms, false, async {
            let rooms = crate::hagency::create_agent::joined_group_rooms().await;
            Ok(serde_json::to_value(rooms).unwrap_or_default())
        });
    }

    pub(super) fn apply_owner_rooms(&mut self, value: &Value) {
        self.owner_rooms = serde_json::from_value(value.clone()).unwrap_or_default();
    }

    /// The dropdown: a prompt row, then one row per room it can join.
    pub(super) fn render_add_room(&mut self, cx: &mut Cx) {
        let rooms = rooms_to_add(&self.owner_rooms, &self.status);
        let prompt = if rooms.is_empty() { "No other rooms to add it to" } else { "Add to a room…" };
        let labels = std::iter::once(crate::i18n::tr(prompt).to_owned()).chain(rooms.iter().map(|(_, name)| name.clone())).collect();
        let select = self.view.drop_down(cx, ids!(add_room));
        select.set_labels(cx, labels);
        select.set_selected_item(cx, 0);
        let puppet = self.agent["puppetMxid"].as_str().unwrap_or("");
        self.view.widget(cx, ids!(add_room)).set_disabled(cx, self.busy || rooms.is_empty() || puppet.is_empty() || self.status.is_null());
    }

    pub(super) fn handle_add_room_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(index) = self.view.drop_down(cx, ids!(add_room)).selected(actions) else { return };
        let rooms = rooms_to_add(&self.owner_rooms, &self.status);
        let (Some(room), Some(puppet)) = (index.checked_sub(1).and_then(|i| rooms.get(i)), self.agent["puppetMxid"].as_str().filter(|p| !p.is_empty())) else {
            return self.render(cx);
        };
        let name = self.agent["displayName"].as_str().unwrap_or("Agent").to_owned();
        self.note(cx, "Adding the agent to the room…");
        self.busy = true;
        let request = self.request;
        crate::hagency::bring_agent::bind_into_rooms_then(name, puppet.to_owned(), vec![room.clone()], move |result| {
            let result = result.clone().map(Value::Bool);
            Cx::post_action(Loaded { request, load: Load::Added, mutation: true, result });
        });
        self.render(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Live finding: the Context tab could remove an Agent from a room but not add one.
    #[test]
    fn only_rooms_the_agent_is_not_in_are_offered() {
        let owner = vec![
            ("!ops:s".to_owned(), "Ops".to_owned()), ("!dm:s".to_owned(), "DM".to_owned()),
            ("!old:s".to_owned(), "old".to_owned()), ("!new:s".to_owned(), "Beta".to_owned()),
            ("!wait:s".to_owned(), "Wait".to_owned()), ("!new:s".to_owned(), "Beta".to_owned()),
        ];
        let status = json!({"chat": {"roomId": "!dm:s"}, "rooms": [
            {"roomId": "!ops:s", "state": "active"}, {"roomId": "!old:s", "state": "left"}, {"roomId": "!wait:s", "state": "pending"},
        ]});
        let names: Vec<String> = rooms_to_add(&owner, &status).into_iter().map(|r| r.1).collect();
        assert_eq!(names, vec!["Beta", "old"], "bound, pending and the private chat are left out; a left room can be re-added");
        assert_eq!(rooms_to_add(&[], &status), vec![]);
    }
}
