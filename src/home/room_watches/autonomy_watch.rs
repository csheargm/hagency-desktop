//! Keeps a room's agent work current while it is shown (ADR 0030, GPT
//! review): the Status tab's controls and the room header's badge were read
//! only when the room opened or this desktop acted, so they stayed stale when
//! someone else paused, resumed or changed the level. Sliding sync carries
//! those custom state events in the timeline (and power levels, which move
//! who may change what, in required state too), so each one reads the room's
//! agent work again from the homeserver; the event itself is only a hint,
//! never the state. A limited timeline can skip a custom event (they are not
//! in the subscription's required state); the loop's own check of the room
//! then shows its fresh read ([`server::read`](crate::hagency::room_autonomy::server::read)).
use ::hagency::issue_watch::safeguards::room_autonomy::{ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE};
use matrix_sdk::{Client, room::Room, ruma::{events::{AnySyncStateEvent, AnySyncTimelineEvent}, serde::Raw}};
use std::sync::Mutex;

/// The room's power levels: who is owner or maintainer.
const POWER_LEVELS: &str = "m.room.power_levels";
/// The state types whose change makes a room's agent work stale.
const STALE_BY: [&str; 3] = [ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, POWER_LEVELS];

/// Whether an event of type `kind` changes who holds a role (the Members tab's tags and menus).
pub fn changes_roles(kind: &str) -> bool {
    kind == POWER_LEVELS
}

/// Whether an event of type `kind` makes a room's agent work stale.
pub fn makes_stale(kind: &str) -> bool {
    STALE_BY.contains(&kind)
}

/// The account session whose client is watched; a new session attaches again.
static WATCHED: Mutex<String> = Mutex::new(String::new());

/// Watches `client`'s sync once per account session; a room whose agent work
/// was read on this desktop is read again on a change, and a power-level
/// change tells the Members tab to read the roles again.
pub fn attach(client: &Client) {
    let account = super::account();
    let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
    if *watched == account {
        return;
    }
    *watched = account;
    client.add_event_handler(|event: Raw<AnySyncTimelineEvent>, room: Room| async move { reread_on(event.get_field::<String>("type"), &room) });
    // Power levels also come in a room's required state, without a timeline event (GPT review).
    client.add_event_handler(|event: Raw<AnySyncStateEvent>, room: Room| async move { reread_on(event.get_field::<String>("type"), &room) });
}

/// Reads `room`'s agent work again when an event of type `kind` changed it.
fn reread_on(kind: serde_json::Result<Option<String>>, room: &Room) {
    // An event without a readable type is no agent work change: nothing to do.
    let Ok(Some(kind)) = kind else { return };
    if changes_roles(&kind) {
        // The Members tab reads the roles again (it ignores other rooms).
        makepad_widgets::Cx::post_action(crate::home::mobile_chat_info::member_role::RoleChanged { room_id: room.room_id().to_owned() });
    }
    if makes_stale(&kind) && super::autonomy_store::was_read(room.room_id()) {
        super::autonomy_store::refresh(room.room_id());
    }
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
        assert!(changes_roles("m.room.power_levels"), "a role change rereads the Members tab");
        assert!(!changes_roles(ROOM_PAUSE_STATE_TYPE));
    }
}
