//! Custom room state read from the homeserver itself. Sliding sync does not
//! carry custom state types (`io.hagency.memory`, `io.hagency.skill`) in its
//! required state, so the local store never holds them and a local read showed
//! an empty list right after a successful write. One full state read per load
//! answers for every custom type kept as plaintext state.
use matrix_sdk::{Client, ruma::{RoomId, api::client::state::get_state_events}};
use serde_json::Value;

/// The room's current state events of `event_type`, as the server holds them.
pub(crate) async fn events_of_type(client: &Client, room_id: &RoomId, event_type: &str) -> Result<Vec<Value>, String> {
    let response = client
        .send(get_state_events::v3::Request::new(room_id.to_owned()))
        .await
        .map_err(|error| crate::i18n::format("Room memory is unavailable right now: {reason}", &[("reason", error.to_string())]))?;
    Ok(of_type(response.room_state.iter().filter_map(|raw| serde_json::from_str(raw.json().get()).ok()), event_type))
}

/// The events of one type from a full state listing.
pub(crate) fn of_type(events: impl Iterator<Item = Value>, event_type: &str) -> Vec<Value> {
    events.filter(|event| event["type"] == event_type).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path_regex}};

    /// Regression: memory written to the server (state present there) but absent from the
    /// sliding-sync store must still be listed.
    #[tokio::test]
    async fn state_on_the_server_is_read_even_when_the_sync_store_lacks_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path_regex(r"^/_matrix/client/v3/rooms/.*/state$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"type":"io.hagency.memory","state_key":"m1","sender":"@o:x","event_id":"$1","origin_server_ts":1,"content":{"text":"demo Monday"}},
                {"type":"m.room.name","state_key":"","sender":"@o:x","event_id":"$2","origin_server_ts":1,"content":{"name":"r"}}])))
            .mount(&server).await;
        let client = Client::builder().homeserver_url(server.uri()).server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
        client.matrix_auth().restore_session(matrix_sdk::authentication::matrix::MatrixSession {
            meta: matrix_sdk::SessionMeta { user_id: matrix_sdk::ruma::owned_user_id!("@o:x"), device_id: "D".into() },
            tokens: matrix_sdk::SessionTokens { access_token: "t".into(), refresh_token: None },
        }, matrix_sdk::store::RoomLoadSettings::default()).await.unwrap();
        let room = matrix_sdk::ruma::room_id!("!r:x");
        let events = events_of_type(&client, room, "io.hagency.memory").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["content"]["text"], "demo Monday");
        assert!(events_of_type(&client, room, "io.hagency.skill").await.unwrap().is_empty());
    }
}
