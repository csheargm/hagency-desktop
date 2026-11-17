//! Identity tab: the owner renames the Agent. The server sets the stored name
//! and the Agent's Matrix display name in one owner call.
use super::*;
use crate::hagency::model::AgentAction;

/// The name to send, or the note explaining why nothing is sent. Mirrors the
/// server policy: 1-64 characters after trimming, no control characters.
pub(super) fn rename_input(raw: &str, current: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("Use 1 to 64 characters for the name.");
    }
    if name == current {
        return Err("That is already its name.");
    }
    Ok(name.to_owned())
}

impl AgentPanel {
    pub(super) fn fill_rename(&mut self, cx: &mut Cx) {
        let name = self.agent["displayName"].as_str().unwrap_or("").to_owned();
        self.view.text_input(cx, ids!(rename_input)).set_text(cx, &name);
    }
    pub(super) fn render_rename(&mut self, cx: &mut Cx) {
        self.view.button(cx, ids!(rename_save)).set_enabled(cx, !self.busy);
    }
    pub(super) fn handle_rename_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let input = self.view.text_input(cx, ids!(rename_input));
        if !self.view.button(cx, ids!(rename_save)).clicked(actions) && input.returned(actions).is_none() {
            return;
        }
        match rename_input(&input.text(), self.agent["displayName"].as_str().unwrap_or("")) {
            Ok(name) => {
                self.note(cx, "Saving…");
                let command = Command::Agent { agent_id: self.agent_id(), action: AgentAction::Rename(name) };
                self.command(Load::Renamed, true, command);
            }
            Err(note) => self.note(cx, note),
        }
        self.render(cx);
    }
    /// The reply's Agent (or the bare projection) carries the saved name.
    pub(super) fn apply_rename(&mut self, cx: &mut Cx, value: &Value) {
        let agent = if value["agent"].is_object() { &value["agent"] } else { value };
        if let Some(name) = agent["displayName"].as_str() {
            self.agent["displayName"] = Value::from(name);
            self.fill_rename(cx);
            cx.action(AgentPanelAction::Renamed { agent_id: self.agent_id(), display_name: name.to_owned() });
        }
        self.note(cx, "Saved.");
    }
}
