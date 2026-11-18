//! The "Review by a different model" room state (ADR 0024), read from the
//! homeserver itself: sliding sync does not carry custom state types, so the
//! local store never holds it (see `home::room_memory::server_state`).
use super::reviewer_pick::{DIFFERENT_MODEL, ROOM_REVIEW_STATE, different_model};
use matrix_sdk::Room;
use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::api::client::state::get_state_event_for_key::v3 as get_state;
use matrix_sdk::ruma::events::StateEventType;
use serde_json::{Value, json};

/// Whether `room` prefers a different-model reviewer (never set: ON).
pub async fn prefers_different_model(room: &Room) -> Result<bool, String> {
    let request = get_state::Request::new(room.room_id().to_owned(), StateEventType::from(ROOM_REVIEW_STATE), String::new());
    match room.client().send(request).await {
        Ok(response) => {
            let content: Value = serde_json::from_str(response.event_or_content.get()).map_err(|e| e.to_string())?;
            Ok(different_model(Some(&content)))
        }
        Err(error) if error.client_api_error_kind() == Some(&ErrorKind::NotFound) => Ok(different_model(None)),
        Err(error) => Err(error.to_string()),
    }
}

/// Whether this account may change it (the room's power levels decide).
pub async fn can_change(room: &Room) -> bool {
    let Some(user) = room.client().user_id().map(ToOwned::to_owned) else { return false };
    room.power_levels().await.is_ok_and(|levels| levels.user_can_send_state(&user, StateEventType::from(ROOM_REVIEW_STATE)))
}

/// Save the setting as room state.
pub async fn set_different_model(room: &Room, on: bool) -> Result<(), String> {
    room.send_state_event_raw(ROOM_REVIEW_STATE, "", json!({DIFFERENT_MODEL: on})).await.map(|_| ()).map_err(|e| e.to_string())
}
