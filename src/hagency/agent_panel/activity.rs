//! Context and Activity tabs, read-only. Context lists the Agent's rooms
//! (its bindings, named from this client's rooms); Activity lists waiting
//! approvals and the last requests from this device's ledger.
use super::*;

/// One line per room: "Name · state", the private chat first.
pub(super) fn room_lines(status: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if status["chat"]["roomId"].is_string() {
        lines.push(crate::i18n::tr("Private chat with you").to_owned());
    }
    for room in status["rooms"].as_array().into_iter().flatten() {
        let name = room["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(crate::i18n::tr("Unnamed room"));
        let state = room["state"].as_str().unwrap_or("");
        lines.push(if state.is_empty() || state == "active" { name.to_owned() } else { format!("{name} · {state}") });
    }
    lines
}

/// The Context tab's text: nothing while the status loads, an error (not "Not
/// in any room yet") when its Rooms read failed, else one line per room.
pub(super) fn context_text(status: &Value) -> String {
    let lines = room_lines(status);
    if status.is_null() {
        String::new()
    } else if !status["rooms"].is_array() {
        crate::i18n::tr("Couldn't load this Agent's rooms. Open this tab again to retry.").to_owned()
    } else if lines.is_empty() {
        crate::i18n::tr("Not in any room yet.").to_owned()
    } else {
        lines.join("\n")
    }
}

impl AgentPanel {
    pub(super) fn render_context(&mut self, cx: &mut Cx) {
        let text = context_text(&self.status);
        self.view.label(cx, ids!(rooms)).set_text(cx, &text);
        self.render_room_ticks(cx);
    }

    /// Reloads Activity when its tab is showing; the ledger is on this device only.
    pub(super) fn refresh_activity(&mut self) {
        if self.tab != Tab::Activity || !self.here() { return; }
        let agent_id = self.agent_id();
        match Service::for_current() {
            Ok(service) => self.spawn(Load::Activity, false, activity_load::load(service, agent_id)),
            Err(code) => self.spawn(Load::Activity, false, async move { Err(code) }),
        }
    }
    pub(super) fn render_activity(&mut self, cx: &mut Cx) {
        let lines = activity_rows::activity_lines(&self.activity, activity_rows::local_time);
        let text = if self.status.is_null() {
            String::new()
        } else if !self.here() {
            crate::i18n::tr("Activity is kept on the device that runs this agent.").to_owned()
        } else if self.activity.is_null() {
            crate::i18n::tr("Loading…").to_owned()
        } else if lines.is_empty() {
            crate::i18n::tr("No requests yet.").to_owned()
        } else {
            lines.join("\n")
        };
        self.view.label(cx, ids!(activity)).set_text(cx, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// GPT integration review: when the Agent's Rooms read failed, Context
    /// said "Not in any room yet." (or listed only the private chat).
    #[test]
    fn a_failed_rooms_read_is_shown_as_an_error_not_an_empty_list() {
        let failed = json!({"here": true, "chat": {"roomId": "!dm:s"}, "rooms": null});
        assert_eq!(context_text(&failed), "Couldn't load this Agent's rooms. Open this tab again to retry.");
        assert_eq!(context_text(&json!({"here": true, "rooms": []})), "Not in any room yet.");
        let both = json!({"chat": {"roomId": "!dm:s"}, "rooms": [{"name": "dev room", "state": "active"}]});
        assert_eq!(context_text(&both), "Private chat with you\ndev room");
        assert_eq!(context_text(&Value::Null), "", "still loading");
    }
}
