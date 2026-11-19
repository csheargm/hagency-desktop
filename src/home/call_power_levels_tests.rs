//! Power levels Hagency sets on rooms (moved out of call_power_levels.rs for R4).
use super::*;
use serde_json::json;

#[test]
fn opens_only_the_call_key_and_keeps_server_defaults() {
    let server = json!({"users":{"@me:x":100},"state_default":50,"invite":0,
        "events":{"m.room.tombstone":150,"m.room.power_levels":100,"custom.locked":75}});
    let updated = open_call_membership(server.clone()).unwrap().unwrap();
    let mut expected = server;
    expected["events"][CALL_MEMBER_EVENT] = json!(0);
    expected["events"][::hagency::repo_status::ISSUE_STATUS_STATE_TYPE] = json!(0);
    expected["events"][ADMIN_STATE_EVENTS[0]] = json!(ADMIN_LEVEL);
    expected["events"][MAINTAINER_STATE_EVENTS[0]] = json!(50);
    expected["events"][PAUSE_EVENT] = json!(0);
    assert_eq!(updated, expected);
}

/// A new room's merge rules are admin-only, unless the server already set them.
#[test]
fn created_room_gets_admin_only_merge_rules_but_keeps_an_explicit_level() {
    let rules = ::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE;
    let updated = open_call_membership(json!({"state_default":50})).unwrap().unwrap();
    assert_eq!(updated["events"][rules], 100);
    let explicit = json!({"events":{rules:75, CALL_MEMBER_EVENT:0, ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE:0, MAINTAINER_STATE_EVENTS[0]:100, PAUSE_EVENT:0}});
    assert!(open_call_membership(explicit.clone()).unwrap().is_none(), "an explicit level is never overwritten");
    let mut closed = explicit;
    closed["events"][CALL_MEMBER_EVENT] = json!(50);
    let reopened = open_call_membership(closed).unwrap().unwrap();
    assert_eq!((reopened["events"][CALL_MEMBER_EVENT].as_i64(), reopened["events"][rules].as_i64()), (Some(0), Some(75)));
    assert_eq!(reopened["events"][MAINTAINER_STATE_EVENTS[0]], 100, "a stricter autonomy entry is never lowered");
}

/// ADR 0030: the autonomy level is for owner and maintainers (50, never below a
/// stricter `state_default`), a pause for every member.
#[test]
fn autonomy_is_for_maintainers_and_pause_for_members() {
    let updated = open_call_membership(json!({"state_default":50})).unwrap().unwrap();
    assert_eq!((updated["events"][MAINTAINER_STATE_EVENTS[0]].as_i64(), updated["events"][PAUSE_EVENT].as_i64()), (Some(50), Some(0)));
    let strict = add_missing_member_state(json!({"state_default":100}), &EXISTING_ROOM_EVENTS).unwrap().unwrap();
    assert_eq!(strict["events"][MAINTAINER_STATE_EVENTS[0]], 100, "a stricter default is not loosened");
}

#[test]
fn adds_events_map_when_missing_and_skips_when_already_open() {
    let updated = open_call_membership(json!({"users_default":0})).unwrap().unwrap();
    assert_eq!(updated["events"][CALL_MEMBER_EVENT], 0);
    assert_eq!(updated["events"][::hagency::repo_status::ISSUE_STATUS_STATE_TYPE], 0);
    assert!(open_call_membership(updated).unwrap().is_none());
    let call_only = json!({"events": {CALL_MEMBER_EVENT: 0, ADMIN_STATE_EVENTS[0]: 100}});
    assert!(open_call_membership(call_only).unwrap().is_some(), "a room with calls open still opens issue status");
    assert!(open_call_membership(json!([])).is_err());
    assert!(open_call_membership(json!({"events":[]})).is_err(), "malformed is an error, not 'already open'");
}

async fn signed_in(server: &wiremock::MockServer) -> matrix_sdk::Client {
    let client = matrix_sdk::Client::builder().homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
    client.matrix_auth().restore_session(matrix_sdk::authentication::matrix::MatrixSession {
        meta: matrix_sdk::SessionMeta { user_id: matrix_sdk::ruma::owned_user_id!("@o:x"), device_id: "D".into() },
        tokens: matrix_sdk::SessionTokens { access_token: "t".into(), refresh_token: None },
    }, matrix_sdk::store::RoomLoadSettings::default()).await.unwrap();
    client
}

/// A server refusing the follow-up must not fail or hang room creation.
#[tokio::test]
async fn follow_up_failure_is_bounded_and_never_a_creation_error() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/_matrix/client/v3/createRoom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id":"!r:x"})))
        .mount(&server).await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(403)
        .set_body_json(json!({"errcode":"M_FORBIDDEN","error":"no"}))).mount(&server).await;
    let client = signed_in(&server).await;
    let room = client.create_room(Default::default()).await.unwrap();
    tokio::time::timeout(FOLLOW_UP_TIMEOUT * 2, allow_call_membership_after_create(&room, true)).await.unwrap();
    assert!(allow_call_membership(&room, true).await.is_err());
    assert!(server.received_requests().await.unwrap().iter().all(|r| r.method.as_str() != "PUT"));
}

/// Creates a room against a mock server and returns the server's levels and the levels sent back.
async fn created_room_put(open_calls: bool) -> (Value, Value) {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/_matrix/client/v3/createRoom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id":"!r:x"})))
        .mount(&server).await;
    let defaults = json!({"users":{"@o:x":100},"state_default":50,
        "events":{"m.room.tombstone":150,"m.room.power_levels":100}});
    mount_server_levels(&server, &defaults).await;
    let client = signed_in(&server).await;
    let config = crate::home::space_management::CreateRoomConfig { name: "Team".into(), public: !open_calls, ..Default::default() };
    let room = client.create_room(config.request().unwrap()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), allow_call_membership(&room, open_calls))
        .await.unwrap().unwrap();
    let put = server.received_requests().await.unwrap().into_iter()
        .find(|r| r.method.as_str() == "PUT").expect("power levels were sent");
    (defaults, serde_json::from_slice(&put.body).unwrap())
}

/// User flow: create a private room, then the follow-up keeps the server's
/// power levels, opens call membership and makes the merge rules admin-only.
#[tokio::test]
async fn created_room_gets_call_membership_opened_on_the_server() {
    let (mut expected, sent) = created_room_put(true).await;
    expected["events"][CALL_MEMBER_EVENT] = json!(0);
    expected["events"][::hagency::repo_status::ISSUE_STATUS_STATE_TYPE] = json!(0);
    expected["events"][ADMIN_STATE_EVENTS[0]] = json!(ADMIN_LEVEL);
    expected["events"][MAINTAINER_STATE_EVENTS[0]] = json!(50);
    expected["events"][PAUSE_EVENT] = json!(0);
    assert_eq!(sent, expected);
}

/// User flow: a public room keeps calls as the server set them but still
/// gets admin-only merge rules (GPT review).
#[tokio::test]
async fn created_public_room_gets_only_admin_only_merge_rules() {
    let (mut expected, sent) = created_room_put(false).await;
    expected["events"][ADMIN_STATE_EVENTS[0]] = json!(ADMIN_LEVEL);
    expected["events"][MAINTAINER_STATE_EVENTS[0]] = json!(50);
    expected["events"][PAUSE_EVENT] = json!(0);
    assert_eq!(sent, expected);
}

/// The room's power levels on a mock homeserver: a GET answers what the last
/// PUT wrote, as a real server does; `landing` is another client's write that
/// lands right after this desktop's next PUT (the race the re-read catches).
#[derive(Clone, Default)]
pub(super) struct ServerLevels {
    pub now: std::sync::Arc<std::sync::Mutex<Value>>,
    pub landing: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
}

impl wiremock::Respond for ServerLevels {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let mut now = self.now.lock().unwrap();
        if request.method.as_str() == "PUT" {
            let written: Value = serde_json::from_slice(&request.body).unwrap();
            *now = self.landing.lock().unwrap().take().unwrap_or(written);
            return wiremock::ResponseTemplate::new(200).set_body_json(json!({"event_id":"$pl"}));
        }
        wiremock::ResponseTemplate::new(200).set_body_json(now.clone())
    }
}

/// Serves `levels` as the room's power levels, kept up to date by every PUT.
pub(super) async fn mount_server_levels(server: &wiremock::MockServer, levels: &Value) -> ServerLevels {
    use wiremock::{Mock, matchers::path_regex};
    let state = ServerLevels { now: std::sync::Arc::new(std::sync::Mutex::new(levels.clone())), ..Default::default() };
    let at = r"^/_matrix/client/v3/rooms/.*/state/m\.room\.power_levels/?$";
    Mock::given(path_regex(at)).respond_with(state.clone()).mount(server).await;
    state
}

/// GPT review (P2): a role edit holding another room's power-level lock (a
/// slow server) never makes this room's change wait for it; the same room's waits.
#[tokio::test]
async fn a_change_in_another_room_never_holds_up_this_one() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/_matrix/client/v3/createRoom"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id":"!busy-elsewhere:x"})))
        .mount(&server).await;
    mount_server_levels(&server, &json!({"users":{"@o:x":100},"events":{}})).await;
    let room = signed_in(&server).await.create_room(Default::default()).await.unwrap();
    let other = room_lock(matrix_sdk::ruma::room_id!("!role-edit:x"));
    let _held = other.lock().await;
    tokio::time::timeout(std::time::Duration::from_secs(3), allow_call_membership(&room, false))
        .await.expect("another room's lock must not block this room").unwrap();
    let same = room_lock(room.room_id());
    let _this = same.lock().await;
    assert!(room_lock(room.room_id()).try_lock().is_err(), "the same room still changes one at a time");
}

/// GPT review (P2): one domain's locks are per room, so a repository change
/// waiting on a slow room never holds up another room's.
#[test]
fn room_locks_serialise_one_room_only() {
    static LOCKS: RoomLocks = RoomLocks::new();
    let a = LOCKS.of(matrix_sdk::ruma::room_id!("!a:x"));
    let _held = a.try_lock().unwrap();
    assert!(LOCKS.of(matrix_sdk::ruma::room_id!("!b:x")).try_lock().is_ok(), "another room is free");
    assert!(LOCKS.of(matrix_sdk::ruma::room_id!("!a:x")).try_lock().is_err(), "the same room waits");
}
