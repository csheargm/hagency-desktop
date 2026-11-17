//! The Watches tab flow: an Agents roster plus each Agent's saved watch
//! become the rows for one room only, with status and owner, never limits.
use super::*;
use serde_json::json;

const DEV: &str = "!dev:example.org";

fn roster() -> Value {
    json!({"ownerMxid": "@betty:example.org", "agents": [
        {"id": "agt_bird", "displayName": "bird-claude", "state": "active"},
        {"id": "agt_off", "displayName": "sleepy", "state": "off"},
        {"id": "agt_none", "displayName": "plain", "state": "active"},
    ]})
}
fn saved() -> Vec<(String, Value)> {
    let off = json!({"mode": "off", "label": "", "repo": "", "roomId": "", "pushTrusted": false, "enabledAt": 0});
    vec![
        ("agt_bird".into(), json!({"issueWatch": off, "work": [
            {"repo": "csheargm/hagency-demo-sandbox", "roomId": DEV, "issuesPerDay": 9},
            {"repo": "acme/private", "roomId": "!other:example.org"}]})),
        ("agt_off".into(), json!({"issueWatch": off, "work": [{"repo": "acme/web", "roomId": DEV}]})),
        ("agt_none".into(), json!({"issueWatch": off, "work": []})),
    ]
}

#[test]
fn rows_list_this_rooms_watches_with_status_and_owner() {
    let rows = rows(DEV, &roster(), &saved()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].agent.as_str(), rows[0].repo.as_str(), rows[0].active), ("bird-claude", "csheargm/hagency-demo-sandbox", true));
    assert_eq!(rows[0].owner, "@betty:example.org");
    assert!(!rows[1].active, "an Agent that is off has its watch paused");
    let (title, detail) = panel::row_text(&rows[0]);
    assert_eq!(title, "bird-claude · csheargm/hagency-demo-sandbox");
    assert!(detail.contains("Active") && detail.contains("@betty:example.org") && !detail.contains('9'));
}

#[test]
fn unreadable_roster_is_an_error_and_empty_says_so() {
    assert!(rows(DEV, &json!({}), &[]).is_err());
    let empty = rows("!quiet:example.org", &roster(), &saved()).unwrap();
    assert!(empty.is_empty());
    assert!(panel::notice(Some(&Ok(empty))).unwrap().contains("No Agent watches"));
    assert!(panel::notice(Some(&Err("down".into()))).unwrap().contains("down"));
}

#[test]
fn a_superseded_refresh_is_dropped_and_accounts_do_not_share_rows() {
    let room: OwnedRoomId = "!cache:example.org".try_into().unwrap();
    let key = ("@a:example.org".to_owned(), room.clone());
    let (old, new) = (begin(&key), begin(&key));
    assert!(finish(key.clone(), new, Ok(Vec::new())));
    assert!(!finish(key.clone(), old, Err("stale".into())), "an older reply never overwrites a newer one");
    assert!(store().loaded.get(&("@b:example.org".to_owned(), room)).is_none());
}
