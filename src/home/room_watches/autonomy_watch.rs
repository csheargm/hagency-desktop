//! Keeps a room's agent work current while it is shown (ADR 0030, GPT
//! review): the Status tab's controls and the room header's badge were read
//! only when the room opened or this desktop acted, so they stayed stale when
//! someone else paused, resumed or changed the level. Sliding sync carries
//! those custom state events in the timeline, so each one (and a power-level
//! change, which moves who may change what) reads the room's agent work again
//! from the homeserver; the event itself is only a hint, never the state.
use ::hagency::issue_watch::safeguards::room_autonomy::{ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE};
use matrix_sdk::{Client, room::Room, ruma::{events::AnySyncTimelineEvent, serde::Raw}};
use std::sync::Mutex;

/// The state types whose change makes a room's agent work stale.
const STALE_BY: [&str; 3] = [ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, "m.room.power_levels"];

/// Whether an event of type `kind` makes a room's agent work stale.
pub fn makes_stale(kind: &str) -> bool {
    STALE_BY.contains(&kind)
}

/// The account session whose client is watched; a new session attaches again.
static WATCHED: Mutex<String> = Mutex::new(String::new());

/// Watches `client`'s sync once per account session; a room whose agent work
/// was read on this desktop is read again on a change.
pub fn attach(client: &Client) {
    let account = super::account();
    let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
    if *watched == account {
        return;
    }
    *watched = account;
    client.add_event_handler(|event: Raw<AnySyncTimelineEvent>, room: Room| async move {
        // An event without a readable type is no agent work change: nothing to do.
        let Ok(Some(kind)) = event.get_field::<String>("type") else { return };
        if makes_stale(&kind) && super::autonomy::was_read(room.room_id()) {
            super::autonomy::refresh(room.room_id());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_resume_level_or_role_change_rereads_the_room() {
        for kind in [ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, "m.room.power_levels"] {
            assert!(makes_stale(kind), "{kind}");
        }
        assert!(!makes_stale("m.room.message"), "a message is no change to the room's agent work");
    }
}
