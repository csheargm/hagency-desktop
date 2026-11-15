//! Reads and writes a room's team skills through the signed-in Matrix client,
//! with memory's storage rules: state in a plaintext room, encrypted timeline
//! events in an encrypted one (state is never read there). Failures come back
//! as text to show.
use crate::home::room_memory::io::{levels, open, scan};
use crate::home::room_memory::model::{Delivery, Write};
use hagency::room_memory::{PowerLevels, Storage};
use hagency::room_skills::{self as skills, SKILL_STATE_TYPE, Skill};
use matrix_sdk::{Client, ruma::{OwnedRoomId, RoomId}};

#[derive(Clone, Debug)]
pub struct Loaded {
    pub room_id: OwnedRoomId,
    pub me: String,
    pub storage: Storage,
    pub skills: Vec<Skill>,
    pub levels: PowerLevels,
    pub unreadable: usize,
    pub truncated: bool,
}
impl Loaded {
    pub fn can_write(&self) -> bool {
        self.levels.can_write_type(&self.me, SKILL_STATE_TYPE)
    }
}

fn unavailable() -> String {
    crate::i18n::tr("Team skills are unavailable right now. Try again in a moment.").to_owned()
}

pub async fn load(client: &Client, room_id: &RoomId) -> Result<Loaded, String> {
    let me = client.user_id().ok_or_else(unavailable)?.to_string();
    let (room, storage) = open(client, room_id).await?;
    let mut loaded = Loaded {
        room_id: room_id.to_owned(), me, storage, skills: Vec::new(), levels: PowerLevels::default(), unreadable: 0, truncated: false,
    };
    match storage {
        Storage::State => {
            // From the server: the sync store does not hold custom state types.
            let events = crate::home::room_memory::server_state::events_of_type(client, room_id, SKILL_STATE_TYPE).await?;
            loaded.skills = skills::reduce_state(&events);
            loaded.levels = levels(&room, std::iter::once(loaded.me.clone())).await?;
        }
        Storage::Timeline => {
            let scan = scan(&room, SKILL_STATE_TYPE).await?;
            let senders = scan.events.iter().filter_map(|e| e["sender"].as_str().map(String::from)).chain([loaded.me.clone()]);
            loaded.levels = levels(&room, senders).await?;
            loaded.skills = skills::reduce_timeline(&scan.events, &loaded.levels).0;
            (loaded.unreadable, loaded.truncated) = (scan.unreadable, scan.truncated);
        }
    }
    Ok(loaded)
}

/// Sends one write under the signed-in person's own user.
pub async fn send(client: &Client, room_id: &RoomId, storage: Storage, write: &Write) -> Result<(), String> {
    let (room, current) = open(client, room_id).await?;
    if current != storage {
        return Err(unavailable());
    }
    let sent = match &write.delivery {
        Delivery::State { state_key } => room.send_state_event_raw(SKILL_STATE_TYPE, state_key, write.content.clone()).await.map(|_| ()),
        Delivery::Timeline => room.send_raw(SKILL_STATE_TYPE, write.content.clone()).await.map(|_| ()),
    };
    sent.map_err(|error| crate::i18n::format("Could not save the team skill: {reason}", &[("reason", error.to_string())]))
}
