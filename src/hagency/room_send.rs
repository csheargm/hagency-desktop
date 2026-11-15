//! A background send that needs no open timeline, so a background task's
//! message gets the composer's Agent routing.
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId};

/// Sends `message` into a joined room with the composer's Agent routing and
/// waits for the homeserver to accept it. Every failure comes back as text.
/// The caller-chosen transaction id makes a retry with the same id
/// deduplicated by the homeserver instead of posted twice. `extra` adds
/// content fields beside the message (e.g. a sender's marker).
pub async fn send_routed_with(room_id: OwnedRoomId, message: RoomMessageEventContent, extra: serde_json::Map<String, serde_json::Value>, txn_id: OwnedTransactionId) -> Result<(), String> {
    let owner = crate::sliding_sync::current_user_id().ok_or("Signed out")?;
    let room = crate::sliding_sync::get_client()
        .and_then(|client| client.get_room(&room_id))
        .ok_or_else(|| format!("This desktop has not joined room {room_id}"))?;
    let is_direct = room.is_direct().await.map_err(|e| format!("Could not read room {room_id}: {e}"))?;
    let settings = crate::agent_access::current();
    let routing = crate::agent_access::routing::directives(&settings, Some(&owner), &room_id, is_direct, &message, None)?;
    let mut content = serde_json::to_value(&message).map_err(|e| format!("Could not encode the message: {e}"))?;
    let object = content.as_object_mut().ok_or("The message did not encode as an object")?;
    object.extend(extra);
    if let Some(routing) = routing {
        object.extend(routing);
    }
    room.send_raw("m.room.message", content)
        .with_transaction_id(&txn_id)
        .await
        .map_err(|e| format!("Could not send to {room_id}: {e}"))?;
    Ok(())
}
