//! Keeps every client's cards current. Sliding sync does not carry the custom
//! state types in required state, so a room's memory and skills were read once
//! and never again: a card stayed "Waiting…" after someone else accepted it.
//! The state event (and the outcome notice) still arrive in the timeline, so
//! either one reloads a room whose memory or skills were already asked for.
use super::outcome::OUTCOME_KEY;
use hagency::{room_memory::MEMORY_STATE_TYPE, room_skills::SKILL_STATE_TYPE};
use matrix_sdk::{Client, room::Room, ruma::{events::AnySyncTimelineEvent, serde::Raw}};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex, time::{Duration, Instant}};

/// At most one outcome-notice reload per room in this window. Any member can
/// post a notice carrying the marker, so it is only a "reload" hint and is
/// rate limited; the state event itself (which only members with power can
/// send) is never debounced, so a genuine change is never missed.
pub const OUTCOME_RELOAD_GAP: Duration = Duration::from_secs(5);

/// The last outcome-notice reload per room.
#[derive(Debug, Default)]
pub struct OutcomeDebounce(BTreeMap<String, Instant>);

impl OutcomeDebounce {
    /// Whether a notice in `room` at `now` may reload; records it when so.
    pub fn allow(&mut self, room: &str, now: Instant) -> bool {
        if self.0.get(room).is_some_and(|last| now.duration_since(*last) < OUTCOME_RELOAD_GAP) {
            return false;
        }
        self.0.insert(room.to_owned(), now);
        true
    }
}

static OUTCOMES: Mutex<OutcomeDebounce> = Mutex::new(OutcomeDebounce(BTreeMap::new()));

/// What one timeline event makes stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stale {
    pub memory: bool,
    pub skills: bool,
    /// Came from an outcome notice (any sender), not a state event.
    pub from_notice: bool,
}
pub fn stale_by(event: &Value) -> Stale {
    match event["type"].as_str() {
        Some(MEMORY_STATE_TYPE) => Stale { memory: true, skills: false, from_notice: false },
        Some(SKILL_STATE_TYPE) => Stale { memory: false, skills: true, from_notice: false },
        Some("m.room.message") if event["content"].get(OUTCOME_KEY).is_some() => Stale { memory: true, skills: true, from_notice: true },
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
        if stale.from_notice && !OUTCOMES.lock().unwrap_or_else(|p| p.into_inner()).allow(room_id.as_str(), Instant::now()) {
            return;
        }
        if stale.memory && super::store::lock().was_requested(room_id, super::store::me().as_deref()) {
            super::ops::refresh(room_id);
        }
        if stale.skills && crate::home::room_skills::ops::lock().was_requested(room_id, super::store::me().as_deref()) {
            crate::home::room_skills::ops::refresh(room_id);
        }
    });
}
