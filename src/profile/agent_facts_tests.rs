use super::*;
use matrix_sdk::ruma::user_id;
use serde_json::json;

#[test]
fn own_status_reads_running_offline_and_paused() {
    assert_eq!(own_status("active", None), None, "unread service: nothing claimed yet");
    assert_eq!(own_status("active", Some("running")), Some("Running"));
    assert_eq!(own_status("active", Some("processing")), Some("Running"));
    assert_eq!(own_status("active", Some("stopped")), Some("Paused"));
    assert_eq!(own_status("active", Some("paused")), Some("Paused"));
    assert_eq!(own_status("active", Some("no_host")), Some("Offline"));
    assert_eq!(own_status("active", Some("unknown")), Some("Offline"));
    assert_eq!(own_status("active", Some("elsewhere")), Some("On another device"));
    assert_eq!(own_status("suspended", Some("running")), Some("Paused"), "a suspended Agent is paused whatever its service says");
}

#[test]
fn the_roster_names_owners_and_pauses_only() {
    let rows = roster_rows(&json!([
        {"puppetMxid": "@_hagency_agt_1:s", "ownerMxid": "@betty:s", "bindingState": "active"},
        {"puppetMxid": "@_hagency_agt_2:s", "ownerMxid": "@betty:s", "bindingState": "suspended"},
        {"puppetMxid": "@_hagency_agt_3:s"},
        {"puppetMxid": "bad", "ownerMxid": "@betty:s"}
    ]));
    assert_eq!(rows.len(), 2, "rows without an owner or with a malformed ID are left out");
    let first = &rows[user_id!("@_hagency_agt_1:s")];
    assert_eq!(first.owner, user_id!("@betty:s"));
    assert_eq!(roster_status(&first.binding_state), Some("Active in this room"));
    assert_eq!(roster_status(&rows[user_id!("@_hagency_agt_2:s")].binding_state), Some("Paused"));
    assert_eq!(roster_status("left"), None);
}

#[test]
fn own_agents_show_config_facts_and_others_say_who_sees_settings() {
    let own = AgentFacts { own_agent_id: Some("agt_1".into()), owner: None, runtime: Some("Claude"), status: Some("Running") };
    assert_eq!(fact_lines(&own, Some("Anna"), false), vec!["Owner: Anna", "Runtime: Claude", "Status: Running"]);
    let theirs = AgentFacts { status: Some("Paused"), ..Default::default() };
    assert_eq!(fact_lines(&theirs, Some("Betty"), true), vec!["Agent of Betty", "Status: Paused", "Settings are visible only to Betty"]);
    assert_eq!(fact_lines(&AgentFacts::default(), None, false), vec!["Owner not shared in this room", "Settings are visible only to its owner"]);
}
