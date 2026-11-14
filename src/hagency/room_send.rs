//! The composer's message for a mention plus text, and a background send of
//! it that needs no open timeline, so a background task posts exactly what a
//! person would type (and gets the composer's Agent routing).
use matrix_sdk::ruma::events::Mentions;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, UserId};

/// The composer's Markdown for a mention pill followed by the text: `[@Name](matrix.to) text`.
pub fn mention_markdown(label: &str, user: &UserId, text: &str) -> String {
    let label = format!("@{}", label.trim_start_matches('@'));
    format!("{} {}", crate::shared::mention_link(&label, user), text.trim())
}

/// The message the composer would send for `text`, mentioning `user` (shown as `label`) first.
pub fn message_content(text: &str, user: &UserId, label: &str) -> RoomMessageEventContent {
    RoomMessageEventContent::text_markdown(mention_markdown(label, user, text))
        .add_mentions(Mentions::with_user_ids([user.to_owned()]))
}

/// Sends `message` into a joined room with the composer's Agent routing and
/// waits for the homeserver to accept it. Every failure comes back as text.
/// The caller-chosen transaction id makes a retry with the same id
/// deduplicated by the homeserver instead of posted twice.
pub async fn send_routed_with(room_id: OwnedRoomId, message: RoomMessageEventContent, txn_id: OwnedTransactionId) -> Result<(), String> {
    let owner = crate::sliding_sync::current_user_id().ok_or("Signed out")?;
    let room = crate::sliding_sync::get_client()
        .and_then(|client| client.get_room(&room_id))
        .ok_or_else(|| format!("This desktop has not joined room {room_id}"))?;
    let is_direct = room.is_direct().await.map_err(|e| format!("Could not read room {room_id}: {e}"))?;
    let settings = crate::agent_access::current();
    let routing = crate::agent_access::routing::directives(&settings, Some(&owner), &room_id, is_direct, &message, None)?;
    let mut content = serde_json::to_value(&message).map_err(|e| format!("Could not encode the message: {e}"))?;
    if let (Some(object), Some(extra)) = (content.as_object_mut(), routing) {
        object.extend(extra);
    }
    room.send_raw("m.room.message", content)
        .with_transaction_id(&txn_id)
        .await
        .map_err(|e| format!("Could not send to {room_id}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mention_is_a_real_pill_with_escaped_label() {
        let user: &UserId = "@bot:example.org".try_into().unwrap();
        assert_eq!(mention_markdown("Fix*Bot", user, " please fix x "), "[\\@Fix\\*Bot](https://matrix.to/#/@bot:example.org) please fix x");
        let content = message_content("please fix x", user, "Bot");
        assert_eq!(content.mentions.unwrap().user_ids.into_iter().collect::<Vec<_>>(), vec![user.to_owned()]);
    }
}
