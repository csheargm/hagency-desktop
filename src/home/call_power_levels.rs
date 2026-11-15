//! Let every member of a private room the desktop creates join an Element Call.
//! MatrixRTC (Element Call v0.16) publishes each participant as an
//! `org.matrix.msc3401.call.member` state event; under the private_chat preset
//! members have PL 0 while state needs 50, so non-moderators got 403.
//!
//! This is a read-modify-write of the room's power levels right after creation,
//! not `power_level_content_override`: servers replace the whole `events` map
//! from an override, which would drop server defaults (v12 tombstone at 150)
//! and any operator-configured restrictions. Here only the call key changes.
use matrix_sdk::{
    Room,
    ruma::{api::client::state::get_state_event_for_key::v3 as get_state, events::StateEventType},
};
use serde_json::Value;

/// MatrixRTC membership state event used by Element Call v0.16.
pub const CALL_MEMBER_EVENT: &str = "org.matrix.msc3401.call.member";
const MEMBER_LEVEL: i64 = 0;
/// Bound on the follow-up so a stalled server cannot leave creation at "Creating…".
const FOLLOW_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The power levels with call membership opened to PL 0, or `None` when they
/// already allow it (nothing to send). Every other field is kept verbatim.
pub fn open_call_membership(mut levels: Value) -> anyhow::Result<Option<Value>> {
    let object = levels.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels are not an object"))?;
    let events = object.entry("events").or_insert_with(|| Value::Object(Default::default()));
    let events = events.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels events is not an object"))?;
    if events.get(CALL_MEMBER_EVENT).and_then(Value::as_i64) == Some(MEMBER_LEVEL) {
        return Ok(None);
    }
    events.insert(CALL_MEMBER_EVENT.into(), MEMBER_LEVEL.into());
    Ok(Some(levels))
}

/// Apply [`open_call_membership`] to a room the user just created (as creator
/// they may edit power levels).
pub async fn allow_call_membership(room: &Room) -> anyhow::Result<()> {
    let request = get_state::Request::new(
        room.room_id().to_owned(),
        StateEventType::RoomPowerLevels,
        String::new(),
    );
    let current: Value = serde_json::from_str(room.client().send(request).await?.event_or_content.get())?;
    // The room was created a moment ago by this user with no other members, so no
    // concurrent power-level change can be overwritten by this full-content PUT.
    if let Some(updated) = open_call_membership(current)? {
        room.send_state_event_raw("m.room.power_levels", "", updated).await?;
    }
    Ok(())
}

/// Creation flows call this once the room exists. A failure is deliberately
/// not a creation failure: the room is already made, so reporting an error
/// would invite a duplicate; the room then behaves as before this fix (calls
/// need a moderator) and the warning records why.
pub async fn allow_call_membership_after_create(room: &Room) {
    let outcome = tokio::time::timeout(FOLLOW_UP_TIMEOUT, allow_call_membership(room)).await;
    let error = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(error)) => format!("{error:#}"),
        Err(_) => "timed out".to_owned(),
    };
    makepad_widgets::warning!("room {} created but call access not opened: {error}", room.room_id());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opens_only_the_call_key_and_keeps_server_defaults() {
        let server = json!({"users":{"@me:x":100},"state_default":50,"invite":0,
            "events":{"m.room.tombstone":150,"m.room.power_levels":100,"custom.locked":75}});
        let updated = open_call_membership(server.clone()).unwrap().unwrap();
        let mut expected = server;
        expected["events"][CALL_MEMBER_EVENT] = json!(0);
        assert_eq!(updated, expected);
    }

    #[test]
    fn adds_events_map_when_missing_and_skips_when_already_open() {
        let updated = open_call_membership(json!({"users_default":0})).unwrap().unwrap();
        assert_eq!(updated["events"][CALL_MEMBER_EVENT], 0);
        assert!(open_call_membership(updated).unwrap().is_none());
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
        tokio::time::timeout(FOLLOW_UP_TIMEOUT * 2, allow_call_membership_after_create(&room)).await.unwrap();
        assert!(allow_call_membership(&room).await.is_err());
        assert!(server.received_requests().await.unwrap().iter().all(|r| r.method.as_str() != "PUT"));
    }

    /// User flow: create a private room, then the follow-up keeps the server's
    /// power levels and opens only call membership.
    #[tokio::test]
    async fn created_room_gets_call_membership_opened_on_the_server() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path, path_regex}};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/_matrix/client/v3/createRoom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id":"!r:x"})))
            .mount(&server).await;
        let defaults = json!({"users":{"@o:x":100},"state_default":50,
            "events":{"m.room.tombstone":150,"m.room.power_levels":100}});
        Mock::given(method("GET")).and(path_regex(r"^/_matrix/client/v3/rooms/.*/state/m\.room\.power_levels/?$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(defaults.clone()))
            .mount(&server).await;
        Mock::given(method("PUT")).and(path_regex(r"^/_matrix/client/v3/rooms/.*/state/m\.room\.power_levels/?$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id":"$pl"})))
            .mount(&server).await;
        let client = signed_in(&server).await;
        let config = crate::home::space_management::CreateRoomConfig { name: "Team".into(), ..Default::default() };
        let room = client.create_room(config.request().unwrap()).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), allow_call_membership(&room))
            .await.unwrap().unwrap();
        let put = server.received_requests().await.unwrap().into_iter()
            .find(|r| r.method.as_str() == "PUT").expect("power levels were sent");
        let mut expected = defaults;
        expected["events"][CALL_MEMBER_EVENT] = json!(0);
        assert_eq!(serde_json::from_slice::<Value>(&put.body).unwrap(), expected);
    }
}
