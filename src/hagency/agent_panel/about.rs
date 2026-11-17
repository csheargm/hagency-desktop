//! Identity tab, About card (top): read-only facts about the Agent — Matrix
//! ID (with Copy), name, owner, where and on what it runs, state, created
//! date and how many rooms it is in (opens the Context tab).
use super::*;

/// The About card's rows, as `(label, value)`; unknown facts are left out.
pub(super) fn about_rows(agent: &Value, status: &Value, model: &Value, owner: Option<&str>) -> Vec<(&'static str, String)> {
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let mut rows = Vec::new();
    let mut push = |label, value: Option<String>| if let Some(value) = value { rows.push((label, value)); };
    push("Name", text(&agent["displayName"]));
    push("Owner", text(&agent["ownerMxid"]).or_else(|| owner.map(str::to_owned)));
    if !status.is_null() {
        push("Runs on", Some(mapping::runs_on(status["here"] == true, status["deviceName"].as_str().unwrap_or(""), None)));
    }
    let credential = model["modelProfile"]["credential_ref"].as_str().or_else(|| agent["credentialRef"].as_str());
    push("Runtime", credential.map(|c| mapping::harness_label(mapping::runtime_of(c)).to_owned()));
    push("Status", text(&agent["state"]).map(|s| state_label(&s)));
    push("Created", agent["createdAtMs"].as_i64().and_then(created_date));
    rows
}

fn state_label(state: &str) -> String {
    let label = match state { "active" => "Active", "suspended" => "Paused", "creating" => "Being created", "retired" => "Retired", other => other };
    crate::i18n::tr(label).to_owned()
}

/// `createdAtMs` as a local calendar date, e.g. "2026-11-17".
fn created_date(ms: i64) -> Option<String> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|at| at.format("%Y-%m-%d").to_string())
}

impl AgentPanel {
    pub(super) fn render_about(&mut self, cx: &mut Cx) {
        let puppet = self.agent["puppetMxid"].as_str().unwrap_or("");
        self.view.label(cx, ids!(matrix_id)).set_text(cx, puppet);
        self.view.button(cx, ids!(copy_matrix_id)).set_enabled(cx, !puppet.is_empty());
        let owner = crate::sliding_sync::current_user_id().map(|id| id.to_string());
        let lines: Vec<String> = about_rows(&self.agent, &self.status, &self.model, owner.as_deref()).into_iter()
            .map(|(label, value)| format!("{}: {value}", crate::i18n::tr(label))).collect();
        self.view.label(cx, ids!(about)).set_text(cx, &lines.join("\n"));
        let rooms = rooms::joined_rooms(&self.status).len();
        let text = crate::i18n::format("In {count} rooms · View", &[("count", rooms.to_string())]);
        self.view.button(cx, ids!(about_rooms)).set_text(cx, &text);
    }

    pub(super) fn handle_about_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(copy_matrix_id)).clicked(actions)
            && let Some(puppet) = self.agent["puppetMxid"].as_str().filter(|p| !p.is_empty()) {
            cx.copy_to_clipboard(puppet);
            self.note(cx, "Matrix ID copied.");
        }
        if self.view.button(cx, ids!(about_rooms)).clicked(actions) { self.set_tab(cx, Tab::Context); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Live finding: the Identity tab showed no identity facts at all.
    #[test]
    fn about_lists_the_agents_identity_facts() {
        let agent = json!({"displayName": "yue-claude", "state": "active", "createdAtMs": 1_763_337_600_000_i64});
        let status = json!({"here": true, "deviceName": "Mac"});
        let model = json!({"modelProfile": {"credential_ref": "claude-managed:x"}});
        let rows = about_rows(&agent, &status, &model, Some("@chris:s"));
        let labels: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(labels, vec!["Name", "Owner", "Runs on", "Runtime", "Status", "Created"]);
        assert_eq!(rows[1].1, "@chris:s");
        assert_eq!(rows[2].1, "Runs on this Mac");
        assert_eq!(rows[3].1, "Claude");
        assert_eq!(rows[4].1, "Active");
        assert!(rows[5].1.starts_with("2025-11-1"), "{}", rows[5].1);
        let bare = about_rows(&json!({"ownerMxid": "@o:s"}), &Value::Null, &Value::Null, Some("@me:s"));
        assert_eq!(bare, vec![("Owner", "@o:s".to_owned())], "unknown facts are left out; the listing's owner wins");
    }
}
