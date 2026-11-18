//! Opening member-written Hagency state in rooms made before it existed:
//! only missing entries are added, and only by a moderator or admin; the
//! admin-only merge rules (ADR 0029) only by an admin.
use super::*;
use ::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE as RULES;
use ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE;
use ::hagency::issue_watch::safeguards::room_autonomy::{ROOM_AUTONOMY_STATE_TYPE as AUTONOMY, ROOM_PAUSE_STATE_TYPE as PAUSE};
use matrix_sdk::ruma::{events::room::power_levels::{RoomPowerLevelsEventContent, RoomPowerLevelsSource}, room_version_rules::AuthorizationRules, user_id};
use serde_json::json;

fn typed(levels: Value, rules: &AuthorizationRules, creators: &[&str]) -> RoomPowerLevels {
    let content: RoomPowerLevelsEventContent = serde_json::from_value(levels).unwrap();
    let creators: Vec<_> = creators.iter().map(|c| UserId::parse(*c).unwrap()).collect();
    RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), rules, creators)
}

/// A pre-status room: calls already open, issue status absent (its agent
/// work entries, ADR 0030, already set, so these tests keep to the rules and status).
fn old_room() -> Value {
    json!({"users":{"@admin:x":100,"@mod:x":50},"users_default":0,"state_default":50,
        "events":{"m.room.power_levels":100,"m.room.tombstone":150,CALL_MEMBER_EVENT:0,"custom.locked":75,AUTONOMY:50,PAUSE:0}})
}

#[test]
fn adds_only_the_missing_entries_and_keeps_every_other_field() {
    let updated = add_missing_member_state(old_room(), &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    let mut expected = old_room();
    expected["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    expected["events"][RULES] = json!(100);
    assert_eq!(updated, expected);
    assert!(add_missing_member_state(updated, &EXISTING_ROOM_EVENTS).unwrap().is_none(), "nothing to send once present");
    let status_only = add_missing_member_state(old_room(), &[ISSUE_STATUS_STATE_TYPE]).unwrap().unwrap();
    assert!(status_only["events"].get(RULES).is_none(), "only the keys asked for are added");
}

#[test]
fn an_entry_an_admin_set_is_never_changed_even_a_raised_one() {
    let mut levels = old_room();
    levels["events"][CALL_MEMBER_EVENT] = json!(100);
    levels["events"][ISSUE_STATUS_STATE_TYPE] = json!(50);
    levels["events"][RULES] = json!(50);
    assert!(add_missing_member_state(levels.clone(), &EXISTING_ROOM_EVENTS).unwrap().is_none(), "an explicit 50 for the rules is kept");
    levels["events"].as_object_mut().unwrap().remove(ISSUE_STATUS_STATE_TYPE);
    let updated = add_missing_member_state(levels, &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    let got = |key: &str| updated["events"][key].as_i64();
    assert_eq!((got(CALL_MEMBER_EVENT), got(ISSUE_STATUS_STATE_TYPE), got(RULES)), (Some(100), Some(0), Some(50)));
    let bare = add_missing_member_state(json!({"users_default":0}), &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    assert_eq!(bare, json!({"users_default":0,"events":{ISSUE_STATUS_STATE_TYPE:0, RULES:100, AUTONOMY:50, PAUSE:0}}), "call membership is not opened in an existing room");
    assert!(add_missing_member_state(json!({"events":[]}), &EXISTING_ROOM_EVENTS).is_err(), "malformed is an error, not 'present'");
}

#[test]
fn only_an_account_that_may_change_power_levels_adds_entries() {
    let levels = typed(old_room(), &AuthorizationRules::V10, &[]);
    assert_eq!(missing_member_state(&levels), vec![ISSUE_STATUS_STATE_TYPE, RULES]);
    assert_eq!(addable_member_state(&levels, user_id!("@admin:x")), vec![ISSUE_STATUS_STATE_TYPE, RULES]);
    assert!(addable_member_state(&levels, user_id!("@member:x")).is_empty(), "a plain member is skipped");
    assert!(addable_member_state(&levels, user_id!("@mod:x")).is_empty(), "power levels need 100 here");
    let mut moderated = old_room();
    moderated["events"]["m.room.power_levels"] = json!(50);
    let levels = typed(moderated.clone(), &AuthorizationRules::V10, &[]);
    assert_eq!(addable_member_state(&levels, user_id!("@mod:x")), vec![ISSUE_STATUS_STATE_TYPE], "a moderator cannot add the admin-only rules entry");
    moderated["state_default"] = json!(75);
    let levels = typed(moderated, &AuthorizationRules::V10, &[]);
    assert!(addable_member_state(&levels, user_id!("@mod:x")).is_empty(), "below the missing event's current level");
    let creator = typed(old_room(), &AuthorizationRules::V12, &["@owner:x"]);
    assert_eq!(addable_member_state(&creator, user_id!("@owner:x")).len(), 2, "a room v12 creator outranks every level");
}

fn sync_body(levels: &Value) -> Value {
    let state = |kind: &str, content: Value, n: u32| json!({"type":kind,"state_key":"","sender":"@admin:x","content":content,
        "event_id":format!("$s{n}"),"origin_server_ts":1});
    let member = |who: &str, n: u32| json!({"type":"m.room.member","state_key":who,"sender":who,"content":{"membership":"join"},
        "event_id":format!("$m{n}"),"origin_server_ts":1});
    json!({"next_batch":"s1","rooms":{"join":{"!r:x":{
        "state":{"events":[state("m.room.create", json!({"room_version":"10","creator":"@admin:x"}), 1), state("m.room.power_levels", levels.clone(), 2), member("@admin:x", 1), member("@member:x", 2)]},
        "timeline":{"events":[],"limited":false}}}}})
}

async fn synced_as(server: &wiremock::MockServer, me: &str, levels: &Value) -> Room {
    use wiremock::{Mock, ResponseTemplate, matchers::{method, path}};
    Mock::given(method("GET")).and(path("/_matrix/client/v3/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_body(levels))).mount(server).await;
    let client = matrix_sdk::Client::builder().homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
    client.matrix_auth().restore_session(matrix_sdk::authentication::matrix::MatrixSession {
        meta: matrix_sdk::SessionMeta { user_id: UserId::parse(me).unwrap(), device_id: "D".into() },
        tokens: matrix_sdk::SessionTokens { access_token: "t".into(), refresh_token: None },
    }, matrix_sdk::store::RoomLoadSettings::default()).await.unwrap();
    client.sync_once(matrix_sdk::config::SyncSettings::default().timeout(std::time::Duration::from_secs(0))).await.unwrap();
    client.get_room(matrix_sdk::ruma::room_id!("!r:x")).expect("room from sync")
}

async fn mount_levels(server: &wiremock::MockServer, levels: &Value) {
    use wiremock::{Mock, ResponseTemplate, matchers::{method, path_regex}};
    let at = r"^/_matrix/client/v3/rooms/.*/state/m\.room\.power_levels/?$";
    Mock::given(method("GET")).and(path_regex(at)).respond_with(ResponseTemplate::new(200).set_body_json(levels.clone())).mount(server).await;
    Mock::given(method("PUT")).and(path_regex(at)).respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id":"$pl"}))).mount(server).await;
}

async fn puts(server: &wiremock::MockServer) -> Vec<Value> {
    server.received_requests().await.unwrap().into_iter().filter(|r| r.method.as_str() == "PUT")
        .map(|r| serde_json::from_slice(&r.body).unwrap()).collect()
}

/// User flow: an admin links a repository in a room made before
/// the issue status existed; the server gets the old levels plus one entry.
#[tokio::test]
async fn an_admin_opens_issue_status_in_an_old_room_and_keeps_the_rest() {
    let server = wiremock::MockServer::start().await;
    let room = synced_as(&server, "@admin:x", &old_room()).await;
    mount_levels(&server, &old_room()).await;
    assert!(allow_member_state_in(&room).await.unwrap().is_empty(), "nothing left for someone else");
    let mut expected = old_room();
    expected["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    expected["events"][RULES] = json!(100);
    assert_eq!(puts(&server).await, vec![expected]);
}

/// A plain member's desktop judges by the server's levels and never writes.
#[tokio::test]
async fn a_member_without_permission_sends_nothing() {
    let server = wiremock::MockServer::start().await;
    let room = synced_as(&server, "@member:x", &old_room()).await;
    mount_levels(&server, &old_room()).await;
    assert_eq!(allow_member_state_in(&room).await.unwrap(), [ISSUE_STATUS_STATE_TYPE, RULES], "left for an admin, so the link can say so");
    assert!(puts(&server).await.is_empty());
}

/// GPT review: the sync store still shows an explicit rules entry the server
/// no longer has; linking judges by the server and adds it.
#[tokio::test]
async fn a_stale_sync_store_does_not_skip_the_rules_entry() {
    let server = wiremock::MockServer::start().await;
    let mut cached = old_room();
    cached["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    let now = cached.clone();
    cached["events"][RULES] = json!(50);
    let room = synced_as(&server, "@admin:x", &cached).await;
    mount_levels(&server, &now).await;
    assert!(allow_member_state_in(&room).await.unwrap().is_empty());
    let mut expected = now;
    expected["events"][RULES] = json!(100);
    assert_eq!(puts(&server).await, vec![expected]);
}

/// GPT review: filling the missing entry never loosens a stricter state_default.
#[test]
fn a_stricter_state_default_is_kept_for_the_rules_entry() {
    let strict = json!({"users":{"@owner:x":200},"state_default":150});
    let updated = add_missing_member_state(strict.clone(), &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    assert_eq!((updated["events"][RULES].as_i64(), updated["events"][ISSUE_STATUS_STATE_TYPE].as_i64()), (Some(150), Some(0)));
    assert_eq!(open_call_membership(strict.clone()).unwrap().unwrap()["events"][RULES], 150);
    let levels = typed(strict, &AuthorizationRules::V10, &[]);
    assert_eq!(addable_member_state(&levels, user_id!("@owner:x")).len(), 4);
    assert_eq!(add_missing_member_state(json!({"state_default":150}), &EXISTING_ROOM_EVENTS).unwrap().unwrap()["events"][AUTONOMY], 150, "the autonomy entry is not loosened either");
    let legacy = json!({"users":{"@owner:x":200},"state_default":"150"});
    let updated = add_missing_member_state(legacy, &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    assert_eq!(updated["events"][RULES], 150, "a room v9 string level is kept too");
}

/// Already present (locally): nothing is read or sent.
#[tokio::test]
async fn a_room_with_the_entry_is_left_alone() {
    let server = wiremock::MockServer::start().await;
    let mut levels = old_room();
    levels["events"][ISSUE_STATUS_STATE_TYPE] = json!(50);
    levels["events"][RULES] = json!(50);
    let room = synced_as(&server, "@admin:x", &levels).await;
    mount_levels(&server, &levels).await;
    assert!(allow_member_state_in(&room).await.unwrap().is_empty(), "nothing missing");
    assert!(puts(&server).await.is_empty());
}

/// User flow: a moderator allowed to change power levels links a repository;
/// members get issue status, but the merge rules stay for an admin to close.
#[tokio::test]
async fn a_moderator_opens_issue_status_but_never_sets_the_rules_level() {
    let server = wiremock::MockServer::start().await;
    let mut levels = old_room();
    levels["events"]["m.room.power_levels"] = json!(50);
    let room = synced_as(&server, "@mod:x", &levels).await;
    mount_levels(&server, &levels).await;
    assert_eq!(allow_member_state_in(&room).await.unwrap(), [RULES], "the admin-only rules entry is left for an admin");
    let mut expected = levels;
    expected["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    assert_eq!(puts(&server).await, vec![expected]);
}
