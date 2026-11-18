//! Let every member of a private room the desktop creates join an Element Call
//! and publish their own Agents' issue status (ADR 0023).
//! MatrixRTC (Element Call v0.16) publishes each participant as an
//! `org.matrix.msc3401.call.member` state event; under the private_chat preset
//! members have PL 0 while state needs 50, so non-moderators got 403.
//!
//! This is a read-modify-write of the room's power levels right after creation,
//! not `power_level_content_override`: servers replace the whole `events` map
//! from an override, which would drop server defaults (v12 tombstone at 150)
//! and any operator-configured restrictions. Here only the call key changes.
//!
//! Rooms made before the issue status existed lack its entry; a moderator or
//! admin's desktop adds that one entry when they link a repository
//! ([`allow_member_state_in`]): an explicit action, never a background load.
//!
//! The merge rules (ADR 0029) decide what Agents may merge, so they are for
//! room admins only: both paths add `events[MERGE_RULES_STATE_TYPE] = 100`
//! when the room has no entry. An entry already there is an admin's choice
//! and is never lowered or overwritten.
use matrix_sdk::{
    Room,
    ruma::{Int, UserId, api::client::state::get_state_event_for_key::v3 as get_state, events::{StateEventType, TimelineEventType, room::power_levels::RoomPowerLevels}},
};
use serde_json::Value;

/// MatrixRTC membership state event used by Element Call v0.16.
pub const CALL_MEMBER_EVENT: &str = "org.matrix.msc3401.call.member";
const MEMBER_LEVEL: i64 = 0;
/// The Matrix presets' admin level.
const ADMIN_LEVEL: i64 = 100;
/// State every member may write. The issue status (ADR 0023) is keyed by its
/// sender's own Matrix ID, which the homeserver lets nobody else write, so
/// opening it lets each owner publish only their own Agents' states.
const MEMBER_STATE_EVENTS: [&str; 2] = [CALL_MEMBER_EVENT, ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE];
/// State only room admins may write; added at [`ADMIN_LEVEL`] only where missing.
const ADMIN_STATE_EVENTS: [&str; 1] = [::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE];
/// What an existing room gets (GPT review): only Hagency's own state, each
/// entry only when missing; call membership stays as that room's admins set it.
const EXISTING_ROOM_EVENTS: [&str; 2] = [::hagency::repo_status::ISSUE_STATUS_STATE_TYPE, ADMIN_STATE_EVENTS[0]];
/// Bound on the follow-up so a stalled server cannot leave creation at "Creating…".
const FOLLOW_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The level Hagency gives `key`: admins for admin-only state, else members.
fn target_level(key: &str) -> i64 {
    if ADMIN_STATE_EVENTS.contains(&key) { ADMIN_LEVEL } else { MEMBER_LEVEL }
}

/// For a room just created: call membership and issue status opened to PL 0,
/// and the merge rules at PL 100 when they have no entry; `None` when nothing
/// changes (nothing to send). Every other field is kept verbatim.
pub fn open_call_membership(levels: Value) -> anyhow::Result<Option<Value>> {
    let keys: Vec<&'static str> = MEMBER_STATE_EVENTS.into_iter().chain(ADMIN_STATE_EVENTS).collect();
    set_where(levels, &keys, |key, level| match level {
        None => true,
        Some(level) => !ADMIN_STATE_EVENTS.contains(&key) && level.as_i64() != Some(MEMBER_LEVEL),
    })
}

/// For an existing room: each of `keys` with no entry at all gets Hagency's
/// level for it, or `None` when every one has an entry. An entry an admin
/// set, even a raised or lowered one, is never changed.
pub fn add_missing_member_state(levels: Value, keys: &[&'static str]) -> anyhow::Result<Option<Value>> {
    set_where(levels, keys, |_, level| level.is_none())
}

/// Sets each of `keys` whose current entry `reset` selects to [`target_level`].
fn set_where(mut levels: Value, keys: &[&'static str], reset: impl Fn(&str, Option<&Value>) -> bool) -> anyhow::Result<Option<Value>> {
    let object = levels.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels are not an object"))?;
    let events = object.entry("events").or_insert_with(|| Value::Object(Default::default()));
    let events = events.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels events is not an object"))?;
    let changed: Vec<&str> = keys.iter().copied().filter(|key| reset(key, events.get(*key))).collect();
    if changed.is_empty() {
        return Ok(None);
    }
    for key in changed {
        events.insert(key.into(), target_level(key).into());
    }
    Ok(Some(levels))
}

/// The existing-room Hagency state events these power levels give no entry.
pub fn missing_member_state(levels: &RoomPowerLevels) -> Vec<&'static str> {
    EXISTING_ROOM_EVENTS.into_iter().filter(|key| !levels.events.contains_key(&TimelineEventType::from(*key))).collect()
}

/// The missing entries `me` may add: they may change power levels, already
/// reach the event's current level and reach the level it gets, as the
/// homeserver's power-level rule requires of a changed entry. So a moderator
/// allowed to change power levels opens issue status, but only an admin adds
/// the admin-only merge-rules entry; a plain member adds nothing.
pub fn addable_member_state(levels: &RoomPowerLevels, me: &UserId) -> Vec<&'static str> {
    if !levels.user_can_send_state(me, StateEventType::RoomPowerLevels) {
        return Vec::new();
    }
    let mine = levels.for_user(me);
    missing_member_state(levels).into_iter()
        .filter(|key| levels.user_can_send_state(me, StateEventType::from(*key)) && mine >= Int::new_saturating(target_level(key)))
        .collect()
}

async fn server_levels(room: &Room) -> anyhow::Result<Value> {
    let request = get_state::Request::new(room.room_id().to_owned(), StateEventType::RoomPowerLevels, String::new());
    Ok(serde_json::from_str(room.client().send(request).await?.event_or_content.get())?)
}

/// Adds the missing Hagency entries this account may add to an existing room;
/// `Ok(false)` when nothing was missing or it may add none (nothing sent).
/// Matrix has no conditional state write: a power-level change another admin
/// makes between this read and write would be lost, so this runs only on an
/// admin's explicit link, never in the background (GPT review).
pub async fn allow_member_state_in(room: &Room) -> anyhow::Result<bool> {
    let me = room.client().user_id().ok_or_else(|| anyhow::anyhow!("signed out"))?.to_owned();
    let addable = addable_member_state(&room.power_levels().await?, &me);
    if addable.is_empty() {
        return Ok(false);
    }
    let send = async {
        // Re-read from the server: the PUT replaces the whole content, so it is
        // built from the newest copy; only a change landing in the moment
        // between this read and the write could be lost.
        let Some(updated) = add_missing_member_state(server_levels(room).await?, &addable)? else { return Ok(false) };
        room.send_state_event_raw("m.room.power_levels", "", updated).await?;
        anyhow::Ok(true)
    };
    tokio::time::timeout(FOLLOW_UP_TIMEOUT, send).await.map_err(|_| anyhow::anyhow!("timed out"))?
}

/// Apply [`open_call_membership`] to a room the user just created (as creator
/// they may edit power levels).
pub async fn allow_call_membership(room: &Room) -> anyhow::Result<()> {
    let current = server_levels(room).await?;
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
        expected["events"][::hagency::repo_status::ISSUE_STATUS_STATE_TYPE] = json!(0);
        expected["events"][ADMIN_STATE_EVENTS[0]] = json!(ADMIN_LEVEL);
        assert_eq!(updated, expected);
    }

    /// A new room's merge rules are admin-only, unless the server already set them.
    #[test]
    fn created_room_gets_admin_only_merge_rules_but_keeps_an_explicit_level() {
        let rules = ::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE;
        let updated = open_call_membership(json!({"state_default":50})).unwrap().unwrap();
        assert_eq!(updated["events"][rules], 100);
        let explicit = json!({"events":{rules:75, CALL_MEMBER_EVENT:0, ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE:0}});
        assert!(open_call_membership(explicit.clone()).unwrap().is_none(), "an explicit level is never overwritten");
        let mut closed = explicit;
        closed["events"][CALL_MEMBER_EVENT] = json!(50);
        let reopened = open_call_membership(closed).unwrap().unwrap();
        assert_eq!((reopened["events"][CALL_MEMBER_EVENT].as_i64(), reopened["events"][rules].as_i64()), (Some(0), Some(75)));
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
        expected["events"][::hagency::repo_status::ISSUE_STATUS_STATE_TYPE] = json!(0);
        expected["events"][ADMIN_STATE_EVENTS[0]] = json!(ADMIN_LEVEL);
        assert_eq!(serde_json::from_slice::<Value>(&put.body).unwrap(), expected);
    }
}

#[cfg(test)]
#[path = "call_power_levels_member_tests.rs"]
mod member_tests;
