use super::*;
use serde_json::json;

/// Regression: a new room got the leftover "ann2" because it came first.
#[test]
fn active_agents_are_newest_first_with_an_identity() {
    let listing = json!({"agents": [
        {"id": "agt_01m7aaaaaaaaaaaaaaaaaaaaaa", "state": "active", "puppetMxid": "@_hagency_agt_ann2:s", "displayName": "ann2"},
        {"id": "agt_01m9zzzzzzzzzzzzzzzzzzzzzz", "state": "retired", "puppetMxid": "@_hagency_agt_gone:s", "displayName": "gone"},
        {"id": "agt_01m8bbbbbbbbbbbbbbbbbbbbbb", "state": "active", "puppetMxid": "@_hagency_agt_new:s", "displayName": "maintainer-agent", "ownerDirectRoomId": "!dm:s"},
        {"id": "agt_01m9yyyyyyyyyyyyyyyyyyyyyy", "state": "active", "puppetMxid": "", "displayName": "no identity"},
    ]});
    let pick = active_agents(&listing).into_iter().next().unwrap();
    assert_eq!(pick, AgentPick { id: "agt_01m8bbbbbbbbbbbbbbbbbbbbbb".into(), name: "maintainer-agent".into(), puppet: "@_hagency_agt_new:s".into(), direct_room: Some("!dm:s".into()) });
    let order: Vec<&str> = active_agents_newest_first(&listing).iter().map(|r| r["displayName"].as_str().unwrap()).collect();
    assert_eq!(order, ["maintainer-agent", "ann2"]);
    assert_eq!(active_agents(&json!({"agents": []})).into_iter().next(), None);
    assert_eq!(active_agents(&json!({})).into_iter().next(), None);
    let unnamed = json!({"agents": [{"id": "a", "state": "active", "puppetMxid": "@p:s"}]});
    assert_eq!(active_agents(&unnamed).into_iter().next().unwrap().name, "Agent");
}

#[test]
fn a_created_time_wins_over_id_order() {
    let listing = json!({"agents": [
        {"id": "agt_b", "state": "active", "puppetMxid": "@b:s", "createdAtMs": 100},
        {"id": "agt_a", "state": "active", "puppetMxid": "@a:s", "createdAtMs": 200},
        {"id": "agt_c", "state": "archived", "puppetMxid": "@c:s", "createdAtMs": 300},
    ]});
    assert_eq!(active_agents(&listing).into_iter().next().unwrap().id, "agt_a");
}

#[test]
fn a_new_group_room_gets_the_agent_but_never_a_space_dm_or_room_it_is_in() {
    let room = RoomFacts { can_invite: true, ..RoomFacts::default() };
    assert!(should_auto_join(true, room));
    assert!(!should_auto_join(false, room), "New room honours \"Join rooms I join\" off");
    for facts in [RoomFacts { space: true, ..room }, RoomFacts { direct: true, ..room }, RoomFacts { agent_present: true, ..room }, RoomFacts { can_invite: false, ..room }] {
        assert!(!should_auto_join(true, facts), "{facts:?}");
    }
}

#[test]
fn room_choices_put_the_open_room_first_and_ticked_then_by_name() {
    let rooms = vec![("!b:s".to_owned(), "beta".to_owned()), ("!c:s".to_owned(), "Charlie".to_owned()), ("!a:s".to_owned(), "alpha".to_owned()), ("!c:s".to_owned(), "Charlie".to_owned())];
    let choices = room_choices(rooms.clone(), Some("!c:s"), MAX_ROOM_CHOICES);
    let order: Vec<_> = choices.iter().map(|c| (c.room_id.as_str(), c.ticked)).collect();
    assert_eq!(order, vec![("!c:s", true), ("!a:s", false), ("!b:s", false)]);
    assert_eq!(ticked_rooms(&choices), vec![("!c:s".to_owned(), "Charlie".to_owned())]);
    let none_open = room_choices(rooms.clone(), None, MAX_ROOM_CHOICES);
    assert!(none_open.iter().all(|c| !c.ticked) && none_open[0].room_id == "!a:s");
    let capped = room_choices(rooms, Some("!c:s"), 1);
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].room_id, "!c:s", "the open room always fits");
}

#[test]
fn outcome_lines_name_the_agent_and_room() {
    assert_eq!(bind_result_text("ann-claude", "Team", &Ok(true)), "ann-claude joined Team.");
    assert_eq!(bind_result_text("ann-claude", "Team", &Ok(false)), "ann-claude is still joining Team.");
    let failed = bind_result_text("ann-claude", "Team", &Err("agent_retired".into()));
    assert!(failed.starts_with("Could not add ann-claude to Team: ") && !failed.ends_with("agent_retired"), "{failed}");
}

#[cfg(test)]
#[path = "tests_follow.rs"]
mod follow;

/// Regression (Anna, dev room): her Agent was created after she made the room,
/// and only later joins and New room brought Agents along, so it never joined.
#[test]
fn a_new_agent_catches_up_only_on_group_rooms_the_owner_created() {
    let group = RoomFacts { can_invite: true, ..RoomFacts::default() };
    assert!(should_catch_up(true, false, true, group));
    assert!(!should_catch_up(true, false, false, group), "a room the owner only joined is left to its prompt");
    assert!(!should_catch_up(false, false, true, group), "the setting is honored");
    assert!(!should_catch_up(true, true, true, group), "a pair already handled (e.g. removed) is not re-added");
    for facts in [RoomFacts { direct: true, ..group }, RoomFacts { space: true, ..group }, RoomFacts { agent_present: true, ..group }, RoomFacts::default()] {
        assert!(!should_catch_up(true, false, true, facts), "{facts:?}");
    }
}
