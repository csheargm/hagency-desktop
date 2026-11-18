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
    assert_eq!(own_status("active", Some("host_restarting")), Some("Offline"));
    assert_eq!(own_status("active", Some("unknown")), None, "an unread service is not claimed Offline");
    assert_eq!(own_status("active", Some("elsewhere")), Some("On another device"));
    assert_eq!(own_status("active", Some("claude_sign_in")), Some("Claude sign-in needed — run `claude login`"));
    assert_eq!(own_status("active", Some("codex_sign_in")), Some("Codex sign-in needed — run `codex login`"));
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
    let own = AgentFacts { own_agent_id: Some("agt_1".into()), owner: None, runtime: Some("Claude"), status: Some("Running"), shared: None };
    assert_eq!(fact_lines(&own, Some("Anna"), false), vec!["Owner: Anna", "Runtime: Claude", "Status: Running"]);
    let theirs = AgentFacts { status: Some("Paused"), ..Default::default() };
    assert_eq!(fact_lines(&theirs, Some("Betty"), true), vec!["Agent of Betty", "Status: Paused", "Settings are visible only to Betty"]);
    assert_eq!(fact_lines(&AgentFacts::default(), None, false), vec!["Owner not shared in this room", "Settings are visible only to its owner"]);
}

/// Chris (Nov 17): the hover card says what a shared Agent does for the
/// room's members, for each of its group rooms only.
#[test]
fn an_own_agents_group_rooms_carry_its_shared_choices() {
    let status = serde_json::json!({
        "rooms": [{"roomId": "!dev:s", "name": "dev", "state": "active"}, {"roomId": "!gone:s", "name": "gone", "state": "left"}],
        "approvals": {"roomShares": [{"room": "!dev:s", "answer": true, "review": false, "fix": false}]}
    });
    let shares = super::group_room_shares(&status);
    assert_eq!(shares.len(), 1, "a room it left is not a group room any more");
    let share = shares["!dev:s"];
    assert!(share.answer && !share.review && !share.fix);
    let facts = AgentFacts { own_agent_id: Some("agt_1".into()), shared: Some(share), ..Default::default() };
    assert_eq!(fact_lines(&facts, Some("Betty"), false), vec!["Owner: Betty", "Shared in this room: answers questions; reviews, fixes need Betty's OK"]);
}
