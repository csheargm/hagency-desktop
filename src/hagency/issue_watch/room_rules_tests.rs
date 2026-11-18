//! Only the room's own rules event, from a person the power levels let send
//! it, becomes the snapshot the merge gate judges by (ADR 0029).
use super::*;
use ::hagency::issue_watch::safeguards::RoomRulesState;

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const RULES: &str = "low_risk_paths: [\"docs/**\"]\nrequire_ci: false\n";

fn event(sender: &str) -> Value {
    json!({"type": MERGE_RULES_STATE_TYPE, "state_key": "", "room_id": ROOM, "sender": sender, "event_id": "$rules1", "origin_server_ts": 1, "content": content_of(RULES)})
}
/// Joined members of the room, as the server's state lists them.
fn members(ids: &[&str]) -> Vec<Value> {
    ids.iter().map(|id| json!({"type": "m.room.member", "state_key": id, "sender": id, "event_id": "$m", "content": {"membership": "join"}})).collect()
}
fn with(rules: Value) -> Vec<Value> {
    let mut all = members(&["@admin:localhost:8450", "@member:localhost:8450", "@_hagency_agt_01m7:localhost:8450", "@_hagency_csheargm_anna-claude:localhost:8450"]);
    all.push(rules);
    all
}
fn admins(sender: &str) -> bool {
    sender == "@admin:localhost:8450"
}

#[test]
fn an_admins_rules_are_accepted_with_their_event_id() {
    let read = accept(&with(event("@admin:localhost:8450")), ROOM, admins);
    assert!(matches!(&read, RoomRead::Rules { event_id, .. } if event_id == "$rules1"));
    assert!(matches!(read.state(), RoomRulesState::Read(p) if p.low_risk_paths == ["docs/**"]));
    let snap = snapshot("csheargm/hagency-demo-sandbox", ROOM, &read);
    assert_eq!((snap.event_id.as_deref(), snap.sender.as_deref(), snap.policy.as_deref()), (Some("$rules1"), Some("@admin:localhost:8450"), Some(RULES)));
    let wire = snapshots_json(&[snap.clone()]);
    let back: Vec<RoomRulesSnapshot> = serde_json::from_value(wire).unwrap();
    assert_eq!(back, [snap], "the owner API reads what the desktop sends");
}

#[test]
fn member_agent_and_other_room_rules_are_ignored() {
    assert_eq!(accept(&with(event("@member:localhost:8450")), ROOM, admins), RoomRead::Refused(REFUSED_SENDER), "a member below the level");
    // Even where the power levels would allow it, an Agent never sets merge rules.
    assert_eq!(accept(&with(event("@_hagency_agt_01m7:localhost:8450")), ROOM, |_| true), RoomRead::Refused(REFUSED_AGENT));
    assert_eq!(accept(&with(event("@_hagency_csheargm_anna-claude:localhost:8450")), ROOM, |_| true), RoomRead::Refused(REFUSED_AGENT));
    let mut elsewhere = event("@admin:localhost:8450");
    elsewhere["room_id"] = json!("!other:localhost:8450");
    assert_eq!(accept(&with(elsewhere), ROOM, admins), RoomRead::Refused(REFUSED_SHAPE), "another room's state");
    let mut keyed = event("@admin:localhost:8450");
    keyed["state_key"] = json!("@admin:localhost:8450");
    assert_eq!(accept(&with(keyed), ROOM, admins), RoomRead::None, "only state key \"\"");
    let mut no_id = event("@admin:localhost:8450");
    no_id["event_id"] = json!("local-echo");
    assert_eq!(accept(&with(no_id), ROOM, admins), RoomRead::Refused(REFUSED_SHAPE), "no server event id");
    let mut banned = with(event("@admin:localhost:8450"));
    banned[0]["content"]["membership"] = json!("ban");
    assert_eq!(accept(&banned, ROOM, admins), RoomRead::Refused(REFUSED_LEFT), "a banned admin's power level does not keep their rules");
    for read in [RoomRead::None, RoomRead::Refused(REFUSED_SENDER)] {
        let snap = snapshot("a/b", ROOM, &read);
        assert_eq!((snap.policy, snap.event_id), (None, None), "recorded as no rules: a person merges");
    }
}

#[test]
fn every_watch_room_of_the_saved_watch_is_synced() {
    let saved = json!({"issueWatch": {"mode": "all", "label": "", "repo": "csheargm/hagency-demo-sandbox", "roomId": ROOM, "pushTrusted": true, "enabledAt": 1},
        "work": [{"repo": "acme/app", "roomId": "!w:x"}]});
    assert_eq!(watched_rooms(&saved), [("csheargm/hagency-demo-sandbox".to_owned(), ROOM.to_owned()), ("acme/app".to_owned(), "!w:x".to_owned())]);
    let mut paused = saved.clone();
    paused["issueWatch"]["mode"] = json!("off");
    assert_eq!(watched_rooms(&paused), [("acme/app".to_owned(), "!w:x".to_owned())], "a paused watch is not synced");
    assert!(watched_rooms(&json!({})).is_empty());
}
