//! Keeps every client's cards current. Sliding sync does not carry the custom
//! state types in required state, so a room's memory and skills were read once
//! and never again: a card stayed "Waiting…" after someone else accepted it.
//! The state event (and the outcome notice) still arrive in the timeline, so
//! either one reloads a room whose memory or skills were already asked for.
use super::outcome::OUTCOME_KEY;
use hagency::{room_memory::MEMORY_STATE_TYPE, room_skills::SKILL_STATE_TYPE};
use matrix_sdk::{Client, room::Room, ruma::{events::AnySyncTimelineEvent, serde::Raw}};
use serde_json::Value;

/// What one timeline event makes stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stale {
    pub memory: bool,
    pub skills: bool,
}
pub fn stale_by(event: &Value) -> Stale {
    match event["type"].as_str() {
        Some(MEMORY_STATE_TYPE) => Stale { memory: true, skills: false },
        Some(SKILL_STATE_TYPE) => Stale { memory: false, skills: true },
        Some("m.room.message") if event["content"].get(OUTCOME_KEY).is_some() => Stale { memory: true, skills: true },
        _ => Stale::default(),
    }
}

/// Watches this client's sync for memory and skill changes.
pub fn attach(client: &Client) {
    client.add_event_handler(|event: Raw<AnySyncTimelineEvent>, room: Room| async move {
        // An event without a readable type is no memory change: nothing to do.
        let Ok(Some(kind)) = event.get_field::<String>("type") else { return };
        if kind != MEMORY_STATE_TYPE && kind != SKILL_STATE_TYPE && kind != "m.room.message" {
            return;
        }
        let Ok(value) = event.deserialize_as::<Value>() else { return };
        let stale = stale_by(&value);
        let room_id = room.room_id();
        if stale.memory && super::store::lock().was_requested(room_id) {
            super::ops::refresh(room_id);
        }
        if stale.skills && crate::home::room_skills::ops::lock().requested.contains(room_id) {
            crate::home::room_skills::ops::refresh(room_id);
        }
    });
}
