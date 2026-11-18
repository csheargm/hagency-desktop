//! Identity tab, About card (top): read-only facts about the Agent — Matrix
//! ID (with Copy), name, owner, where and on what it runs, state, created
//! date and how many rooms it is in (opens the Context tab), then one line per
//! group room saying what it does there for the room's members.
use super::*;

/// The About card's rows, as `(label, value)`; unknown facts are left out.
/// `owner` is the owner's shown name, put before their Matrix ID.
pub(super) fn about_rows(agent: &Value, status: &Value, model: &Value, owner: Option<&str>) -> Vec<(&'static str, String)> {
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let mut rows = Vec::new();
    let mut push = |label, value: Option<String>| if let Some(value) = value { rows.push((label, value)); };
    push("Name", text(&agent["displayName"]));
    push("Owner", owner_line(text(&agent["ownerMxid"]), owner));
    if !status.is_null() {
        push("Runs on", Some(device_line(status["here"] == true, status["deviceName"].as_str().unwrap_or(""))));
    }
    let credential = model["modelProfile"]["credential_ref"].as_str().or_else(|| agent["credentialRef"].as_str());
    push("Runtime", credential.map(|c| mapping::harness_label(mapping::runtime_of(c)).to_owned()));
    push("Status", text(&agent["state"]).map(|s| state_label(&s)));
    push("Created", agent["createdAtMs"].as_i64().and_then(created_date));
    rows
}

/// "Anna (@anna:s)": the name first, the Matrix ID secondary.
fn owner_line(mxid: Option<String>, name: Option<&str>) -> Option<String> {
    match (mxid, name.filter(|n| !n.trim().is_empty())) {
        (Some(mxid), Some(name)) if name != mxid => Some(format!("{name} ({mxid})")),
        (mxid, name) => mxid.or_else(|| name.map(str::to_owned)),
    }
}

/// The "Runs on" value alone ("This Mac", a device), not a sentence.
fn device_line(here: bool, device: &str) -> String {
    match (here, device.is_empty()) {
        (true, _) => crate::i18n::tr("This Mac").to_owned(),
        (false, false) => device.to_owned(),
        (false, true) => crate::i18n::tr("Not assigned to a device yet").to_owned(),
    }
}

/// How many group rooms the Agent is in, from the same rooms as the card's
/// "Shared in …" lines; `None` until the status has read its Rooms (still
/// loading, or the read failed): never a made-up 0.
fn rooms_count(status: &Value) -> Option<usize> {
    status["rooms"].is_array().then(|| mapping::watch_rooms(status).len())
}

/// "In 1 room · View" / "In 3 rooms · View"; "Rooms · View" while the status
/// loads, and "Couldn't load rooms · Retry" once it came back without its
/// Rooms (opening Context reads them again).
fn rooms_line(status: &Value) -> String {
    let Some(count) = rooms_count(status) else {
        let key = if status.is_null() { "Rooms · View" } else { ROOMS_FAILED_LINK };
        return crate::i18n::tr(key).to_owned();
    };
    let key = if count == 1 { "In {count} room · View" } else { "In {count} rooms · View" };
    crate::i18n::format(key, &[("count", count.to_string())])
}

/// The About link when the Agent's Rooms could not be read.
const ROOMS_FAILED_LINK: &str = "Couldn't load rooms · Retry";

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
        // A row without `ownerMxid` is this account's own.
        let owner = self.agent["ownerMxid"].as_str().and_then(|id| matrix_sdk::ruma::OwnedUserId::try_from(id).ok())
            .or_else(crate::sliding_sync::current_user_id);
        let name = owner.as_ref().map(|owner| crate::profile::agent_facts::owner_name(cx, owner, None));
        let mut agent = self.agent.clone();
        if let (Some(owner), Some(row)) = (owner.as_ref(), agent.as_object_mut()) { row.insert("ownerMxid".into(), owner.to_string().into()); }
        let mut lines: Vec<String> = about_rows(&agent, &self.status, &self.model, name.as_deref()).into_iter()
            .map(|(label, value)| format!("{}: {value}", crate::i18n::tr(label))).collect();
        // What it does for each group room's members (ADR 0025).
        lines.extend(crate::hagency::room_share::summaries(&self.status, name.as_deref().unwrap_or(crate::i18n::tr("its owner"))));
        self.view.label(cx, ids!(about)).set_text(cx, &lines.join("\n"));
        let text = rooms_line(&self.status);
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
        let agent = json!({"displayName": "yue-claude", "ownerMxid": "@chris:s", "state": "active", "createdAtMs": 1_763_337_600_000_i64});
        let status = json!({"here": true, "deviceName": "Mac"});
        let model = json!({"modelProfile": {"credential_ref": "claude-managed:x"}});
        let rows = about_rows(&agent, &status, &model, Some("Chris"));
        let labels: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(labels, vec!["Name", "Owner", "Runs on", "Runtime", "Status", "Created"]);
        assert_eq!(rows[1].1, "Chris (@chris:s)", "the owner's name first, the Matrix ID secondary");
        assert_eq!(rows[2].1, "This Mac", "not \"Runs on: Runs on this Mac\"");
        assert_eq!(rows[3].1, "Claude");
        assert_eq!(rows[4].1, "Active");
        assert!(rows[5].1.starts_with("2025-11-1"), "{}", rows[5].1);
        let bare = about_rows(&json!({"ownerMxid": "@o:s"}), &Value::Null, &Value::Null, None);
        assert_eq!(bare, vec![("Owner", "@o:s".to_owned())], "unknown facts are left out");
        let elsewhere = about_rows(&json!({}), &json!({"here": false, "deviceName": "Studio"}), &Value::Null, None);
        assert_eq!(elsewhere, vec![("Runs on", "Studio".to_owned())]);
    }

    /// Live finding: "In 1 rooms".
    #[test]
    fn the_rooms_count_is_singular_for_one_room() {
        let rooms = |n: usize| json!({"rooms": vec![json!({"roomId": "!r:s", "state": "active"}); n]});
        assert_eq!(rooms_line(&rooms(1)), "In 1 room · View");
        assert_eq!(rooms_line(&rooms(0)), "In 0 rooms · View");
    }

    /// Live finding (2026-11-18, bird-claude in dev + e2e room on alpha, owner
    /// on beta): "In 0 rooms · View" while the status was still loading (it
    /// took ~20 s), and a failed Rooms read would also have said 0.
    #[test]
    fn the_rooms_count_waits_for_the_agents_rooms() {
        // The beta server's bindings rows for bird-claude, as the status builds them.
        let rows = [
            json!({"id": "bnd_01m7k84mkv45aajspr87rzjdcd", "agentId": "agt_01m7k84kgg5a5mtm2epqhbpjb5", "projectId": null, "scopeKind": "owner_direct", "roomId": "!UGKDID7ea14wLtXxlh:localhost:8452", "state": "active", "generation": 1}),
            json!({"id": "bnd_01m7k85dxq758k08z530vjh7bc", "agentId": "agt_01m7k84kgg5a5mtm2epqhbpjb5", "projectId": "prj_01m7k6ws2rg5qha2vgffp9s85e", "scopeKind": "project", "roomId": "!a3cQXHR8GZ4RP5n71s:localhost:8450", "state": "active", "generation": 1}),
            json!({"id": "bnd_01m7pfc1axdner3h83f4wa63sj", "agentId": "agt_01m7k84kgg5a5mtm2epqhbpjb5", "projectId": "prj_01m7k6ws2rg5qha2vgffp9s85e", "scopeKind": "project", "roomId": "!VBvsrVXbTdOC6fOclj:localhost:8450", "state": "active", "generation": 1}),
        ];
        let rooms: Vec<Value> = crate::host::hagency::local::group_room_entries(&rows, |_| String::new());
        let loaded = json!({"here": true, "rooms": rooms});
        assert_eq!(rooms_count(&loaded), Some(2), "the dev room and the e2e room; the private chat is not counted");
        assert_eq!(rooms_line(&loaded), "In 2 rooms · View");
        assert_eq!(rooms_count(&Value::Null), None, "not loaded yet: no count");
        assert_eq!(rooms_count(&json!({"here": true, "rooms": null})), None, "the Rooms read failed: no count");
        assert_eq!(rooms_line(&Value::Null), "Rooms · View", "still loading");
        assert_eq!(rooms_line(&json!({"here": true, "rooms": null})), "Couldn't load rooms · Retry",
            "GPT integration review: a failed read is not a neutral \"Rooms\" link");
        // Live finding (anna-claude): "Shared in dev room" listed, count 0. The
        // count reads the rooms those lines read, whatever else a row lacks.
        let shared = json!({"rooms": [{"roomId": "!dev:s", "name": "dev room", "state": "active"}], "approvals": {"roomShares": []}});
        assert_eq!(rooms_count(&shared), Some(crate::hagency::room_share::summaries(&shared, "Anna").len()));
        assert_eq!(rooms_count(&shared), Some(1));
    }
}
