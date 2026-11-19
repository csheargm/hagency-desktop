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

/// GPT integration review: a failed read withdraws the room's last rules
/// (the owner API marks them unread) and the status line says merges need a person.
#[test]
fn a_failed_read_records_the_room_as_unread_and_says_so() {
    let (snap, error) = recorded("a/b", ROOM, Err("timed out".into()));
    assert!(snap.unread && snap.policy.is_none() && snap.event_id.is_none());
    assert!(error.as_deref().is_some_and(|e| e.contains("need a person") && e.contains("timed out")), "{error:?}");
    let sent = snapshots_json(&[snap]);
    assert_eq!(sent[0]["unread"], true, "the flag reaches the owner API");
    let parsed: RoomRulesSnapshot = serde_json::from_value(sent[0].clone()).unwrap();
    assert!(parsed.unread);
    let (read, none) = recorded("a/b", ROOM, Ok(accept(&with(event("@admin:localhost:8450")), ROOM, admins)));
    assert!(!read.unread && read.policy.is_some() && none.is_none());
}

/// GPT integration review: rules over the gate's 8 KiB limit are refused
/// before saving instead of looking saved and counting as invalid.
#[test]
fn rules_over_the_gates_size_limit_are_refused_before_saving() {
    use ::hagency::issue_watch::safeguards::{MAX_POLICY_BYTES, suggested_policy};
    assert!(rules_text(&suggested_policy()).is_ok());
    let mut long = suggested_policy();
    long.low_risk_paths = (0..MAX_POLICY_BYTES / 8).map(|i| format!("docs/{i}/**")).collect();
    assert!(rules_text(&long).is_err_and(|e| e.contains("too long")));
}

const ADMIN: &str = "@admin:localhost:8450";
const MODERATOR: &str = "@member:localhost:8450";

fn power_levels(rules_level: Option<i64>) -> Value {
    let mut content = json!({"users": {ADMIN: 100, MODERATOR: 50}, "state_default": 50, "events": {}});
    if let Some(level) = rules_level {
        content["events"][MERGE_RULES_STATE_TYPE] = json!(level);
    }
    json!({"type": "m.room.power_levels", "state_key": "", "sender": ADMIN, "event_id": "$pl", "origin_server_ts": 1, "content": content})
}

/// Admin-only rules (PL 100): a moderator's rules stop counting, an admin's still do.
#[test]
fn the_rules_level_comes_from_the_servers_power_levels() {
    let judged = |sender: &str, rules_level| {
        let mut events = with(event(sender));
        events.push(power_levels(rules_level));
        let levels = levels_of(&events, &AuthorizationRules::V10, Vec::new()).expect("levels in the state");
        accept(&events, ROOM, |s| UserId::parse(s).is_ok_and(|u| may_set_rules(&levels, &u)))
    };
    assert_eq!(judged(MODERATOR, Some(100)), RoomRead::Refused(REFUSED_SENDER));
    assert!(matches!(judged(ADMIN, Some(100)), RoomRead::Rules { .. }));
    assert!(matches!(judged(MODERATOR, None), RoomRead::Rules { .. }), "without the entry, state_default (50) applies");
    assert!(levels_of(&with(event(ADMIN)), &AuthorizationRules::V10, Vec::new()).is_none(), "no levels: nobody's rules count");
}

/// User flow (GPT review): the sync store still has the old levels (no entry,
/// moderator may send), the server already has 100: the moderator's rules
/// are refused, because `read` judges by the server's levels.
#[tokio::test]
async fn read_judges_by_the_servers_levels_not_a_stale_sync_store() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path, path_regex}};
    let server = MockServer::start().await;
    let state = |kind: &str, content: Value| json!({"type": kind, "state_key": "", "sender": ADMIN, "content": content, "event_id": format!("${kind}"), "origin_server_ts": 1});
    let mut stale = vec![state("m.room.create", json!({"room_version": "10", "creator": ADMIN})), power_levels(None)];
    stale.extend(members(&[ADMIN, MODERATOR]));
    let sync = json!({"next_batch": "s1", "rooms": {"join": {ROOM: {"state": {"events": stale}, "timeline": {"events": [], "limited": false}}}}});
    Mock::given(method("GET")).and(path("/_matrix/client/v3/sync")).respond_with(ResponseTemplate::new(200).set_body_json(sync)).mount(&server).await;
    let mut now = with(event(MODERATOR));
    now.push(power_levels(Some(100)));
    Mock::given(method("GET")).and(path_regex(r"^/_matrix/client/v3/rooms/.*/state/?$")).respond_with(ResponseTemplate::new(200).set_body_json(now)).mount(&server).await;
    let client = matrix_sdk::Client::builder().homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
    client.matrix_auth().restore_session(matrix_sdk::authentication::matrix::MatrixSession {
        meta: matrix_sdk::SessionMeta { user_id: UserId::parse(ADMIN).unwrap(), device_id: "D".into() },
        tokens: matrix_sdk::SessionTokens { access_token: "t".into(), refresh_token: None },
    }, matrix_sdk::store::RoomLoadSettings::default()).await.unwrap();
    client.sync_once(matrix_sdk::config::SyncSettings::default().timeout(std::time::Duration::from_secs(0))).await.unwrap();
    let room = client.get_room(&matrix_sdk::ruma::RoomId::parse(ROOM).unwrap()).expect("room from sync");
    let stale_levels = room.power_levels().await.unwrap();
    assert!(may_set_rules(&stale_levels, &UserId::parse(MODERATOR).unwrap()), "the store is stale on purpose");
    assert_eq!(read(&room).await.unwrap(), RoomRead::Refused(REFUSED_SENDER));
}

/// GPT integration review: every room's snapshot went in one save, which the
/// owner API refused past 16 KiB, so every room kept superseded rules. Saves
/// now stay under the limit, keep every room in order, and a snapshot too big
/// alone is sent withdrawn (fail closed).
#[test]
fn room_rule_saves_are_batched_under_the_owner_api_body_limit() {
    let policy = format!("low_risk_paths: [{}]\n", (0..300).map(|i| format!("\"docs/{i:04}/**\"")).collect::<Vec<_>>().join(", "));
    assert!(policy.len() > 4 * 1024 && policy.len() <= MAX_POLICY_BYTES);
    let snap = |i: usize, policy: String| RoomRulesSnapshot {
        repo: "a/b".into(), room_id: format!("!r{i}:x"), event_id: Some("$e".into()), sender: Some("@admin:x".into()), policy: Some(policy), unread: false,
    };
    let mut all: Vec<RoomRulesSnapshot> = (0..12).map(|i| snap(i, policy.clone())).collect();
    all.push(snap(12, "x".repeat(OWNER_API_MAX_BODY_BYTES)));
    let one = serde_json::to_vec(&save_body(&all)).unwrap().len();
    assert!(one > OWNER_API_MAX_BODY_BYTES, "the old single save was refused: {one}");
    let batches = batches(all.clone());
    assert!(batches.len() > 1);
    for batch in &batches {
        let size = serde_json::to_vec(&save_body(batch)).unwrap().len();
        assert!(size <= OWNER_API_MAX_BODY_BYTES, "{size}");
    }
    let sent: Vec<RoomRulesSnapshot> = batches.into_iter().flatten().collect();
    assert_eq!(sent.iter().map(|s| &s.room_id).collect::<Vec<_>>(), all.iter().map(|s| &s.room_id).collect::<Vec<_>>());
    assert_eq!(&sent[..12], &all[..12], "the ones that fit go unchanged");
    assert!(sent[12].unread && sent[12].policy.is_none(), "too big alone: withdrawn");
}
