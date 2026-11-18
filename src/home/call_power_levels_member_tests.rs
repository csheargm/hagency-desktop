//! Opening member-written Hagency state in rooms made before it existed:
//! only missing entries are added, and only by a moderator or admin.
use super::*;
use ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE;
use matrix_sdk::ruma::{events::room::power_levels::{RoomPowerLevelsEventContent, RoomPowerLevelsSource}, room_version_rules::AuthorizationRules, user_id};
use serde_json::json;

fn typed(levels: Value, rules: &AuthorizationRules, creators: &[&str]) -> RoomPowerLevels {
    let content: RoomPowerLevelsEventContent = serde_json::from_value(levels).unwrap();
    let creators: Vec<_> = creators.iter().map(|c| UserId::parse(*c).unwrap()).collect();
    RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), rules, creators)
}

/// A pre-status room: calls already open, issue status absent.
fn old_room() -> Value {
    json!({"users":{"@admin:x":100,"@mod:x":50},"users_default":0,"state_default":50,
        "events":{"m.room.power_levels":100,"m.room.tombstone":150,CALL_MEMBER_EVENT:0,"custom.locked":75}})
}

#[test]
fn adds_only_the_missing_entry_and_keeps_every_other_field() {
    let updated = add_missing_member_state(old_room()).unwrap().unwrap();
    let mut expected = old_room();
    expected["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    assert_eq!(updated, expected);
    assert!(add_missing_member_state(updated).unwrap().is_none(), "nothing to send once present");
}

#[test]
fn an_entry_an_admin_set_is_never_changed_even_a_raised_one() {
    let mut levels = old_room();
    levels["events"][CALL_MEMBER_EVENT] = json!(100);
    levels["events"][ISSUE_STATUS_STATE_TYPE] = json!(50);
    assert!(add_missing_member_state(levels.clone()).unwrap().is_none());
    levels["events"].as_object_mut().unwrap().remove(ISSUE_STATUS_STATE_TYPE);
    let updated = add_missing_member_state(levels).unwrap().unwrap();
    assert_eq!((updated["events"][CALL_MEMBER_EVENT].as_i64(), updated["events"][ISSUE_STATUS_STATE_TYPE].as_i64()), (Some(100), Some(0)));
    let bare = add_missing_member_state(json!({"users_default":0})).unwrap().unwrap();
    assert_eq!(bare, json!({"users_default":0,"events":{CALL_MEMBER_EVENT:0,ISSUE_STATUS_STATE_TYPE:0}}));
    assert!(add_missing_member_state(json!({"events":[]})).is_err(), "malformed is an error, not 'present'");
}

#[test]
fn only_an_account_that_may_change_power_levels_adds_entries() {
    let levels = typed(old_room(), &AuthorizationRules::V10, &[]);
    assert_eq!(missing_member_state(&levels), vec![ISSUE_STATUS_STATE_TYPE]);
    assert!(may_add_member_state(&levels, user_id!("@admin:x")));
    assert!(!may_add_member_state(&levels, user_id!("@member:x")), "a plain member is skipped");
    assert!(!may_add_member_state(&levels, user_id!("@mod:x")), "power levels need 100 here");
    let mut moderated = old_room();
    moderated["events"]["m.room.power_levels"] = json!(50);
    moderated["state_default"] = json!(75);
    let levels = typed(moderated, &AuthorizationRules::V10, &[]);
    assert!(!may_add_member_state(&levels, user_id!("@mod:x")), "below the missing event's current level");
    let creator = typed(old_room(), &AuthorizationRules::V12, &["@owner:x"]);
    assert!(may_add_member_state(&creator, user_id!("@owner:x")), "a room v12 creator outranks every level");
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

/// User flow: an admin opens (or links a repository in) a room made before
/// the issue status existed; the server gets the old levels plus one entry.
#[tokio::test]
async fn an_admin_opens_issue_status_in_an_old_room_and_keeps_the_rest() {
    let server = wiremock::MockServer::start().await;
    let room = synced_as(&server, "@admin:x", &old_room()).await;
    mount_levels(&server, &old_room()).await;
    assert!(allow_member_state_in(&room).await.unwrap());
    let mut expected = old_room();
    expected["events"][ISSUE_STATUS_STATE_TYPE] = json!(0);
    assert_eq!(puts(&server).await, vec![expected]);
}

/// A plain member's desktop never tries: no read, no write.
#[tokio::test]
async fn a_member_without_permission_sends_nothing() {
    let server = wiremock::MockServer::start().await;
    let room = synced_as(&server, "@member:x", &old_room()).await;
    mount_levels(&server, &old_room()).await;
    assert!(!allow_member_state_in(&room).await.unwrap());
    let touched = server.received_requests().await.unwrap().into_iter().any(|r| r.url.path().contains("power_levels"));
    assert!(!touched);
}

/// Already present (locally): nothing is read or sent.
#[tokio::test]
async fn a_room_with_the_entry_is_left_alone() {
    let server = wiremock::MockServer::start().await;
    let mut levels = old_room();
    levels["events"][ISSUE_STATUS_STATE_TYPE] = json!(50);
    let room = synced_as(&server, "@admin:x", &levels).await;
    mount_levels(&server, &levels).await;
    assert!(!allow_member_state_in(&room).await.unwrap());
    assert!(puts(&server).await.is_empty());
}
