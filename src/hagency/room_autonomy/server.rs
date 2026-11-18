//! The room's work loop controls on the homeserver (ADR 0030): one full
//! state read (never the sync store or a local echo) for the level, pauses,
//! linked repositories, membership and power levels, and the writes the
//! Status tab makes. The homeserver refuses anyone below an event's level;
//! the checks here only spare a refused request.
use super::lines;
use super::state::{Rights, RoomState, state_of};
use crate::hagency::issue_watch::room_rules::{MEMBER_EVENT, POWER_LEVELS_EVENT, joined, power_levels_event};
use ::hagency::issue_watch::safeguards::room_autonomy::{
    Level, MAINTAINER_LEVEL, ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, level_content, pause_content, resume_point,
};
use ::hagency::repo_status::linked::ROOM_REPOS_STATE_TYPE;
use matrix_sdk::Room;
use matrix_sdk::ruma::events::{StateEventType, room::message::RoomMessageEventContent, room::power_levels::RoomPowerLevels};
use matrix_sdk::ruma::{Int, UserId};
use serde_json::Value;

const TYPES: [&str; 5] = [ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, ROOM_REPOS_STATE_TYPE, MEMBER_EVENT, POWER_LEVELS_EVENT];

/// `me`'s rights under `levels`.
pub fn rights_of(levels: &RoomPowerLevels, me: &UserId) -> Rights {
    let may = |kind: &str| levels.user_can_send_state(me, StateEventType::from(kind));
    Rights {
        change_level: may(ROOM_AUTONOMY_STATE_TYPE),
        pause: may(ROOM_PAUSE_STATE_TYPE),
        link: may(ROOM_REPOS_STATE_TYPE),
        maintainer: levels.for_user(me) >= Int::new_saturating(MAINTAINER_LEVEL),
    }
}

/// `room`'s loop state and the raw events it came from, read from the
/// homeserver now. This account must be joined; unreadable power levels
/// make nothing count (fail closed: the level is refused, so paused).
pub async fn read_events(room: &Room) -> Result<(RoomState, Vec<Value>), String> {
    let events = crate::home::room_memory::server_state::events_of_types(&room.client(), room.room_id(), &TYPES).await?;
    let me = room.client().user_id().map(ToOwned::to_owned).ok_or_else(|| crate::i18n::tr("Signed out").to_owned())?;
    if room.state() != matrix_sdk::RoomState::Joined || !joined(&events, me.as_str()) {
        return Err(crate::i18n::tr("this desktop has not joined the room").to_owned());
    }
    let levels = power_levels_event(&events).and_then(|content| crate::home::call_power_levels::typed_levels(room, content));
    let may_send = |sender: &str, kind: &str| {
        let user = UserId::parse(sender).ok();
        levels.as_ref().zip(user).is_some_and(|(l, u)| l.user_can_send_state(&u, StateEventType::from(kind)))
    };
    let rights = levels.as_ref().map(|l| rights_of(l, &me)).unwrap_or_default();
    Ok((state_of(&events, room.room_id().as_str(), may_send, rights), events))
}

/// `room`'s loop state, read from the homeserver now.
pub async fn read(room: &Room) -> Result<RoomState, String> {
    read_events(room).await.map(|(state, _)| state)
}

/// This account's name for room lines, and the local time now.
async fn who_and_when(room: &Room) -> (String, String) {
    let me = room.client().user_id().map(|u| u.to_string()).unwrap_or_default();
    let name = room.client().account().get_display_name().await.ok().flatten().filter(|n| !n.trim().is_empty());
    (name.unwrap_or(me), chrono::Local::now().format("%H:%M").to_string())
}

async fn post(room: &Room, text: String) -> Result<(), String> {
    room.send(RoomMessageEventContent::text_plain(text)).await.map(|_| ()).map_err(|e| crate::i18n::format("Could not post to the room: {error}", &[("error", e.to_string())]))
}

/// Set the level (owner and maintainers), keeping the Resume point; one room line.
pub async fn set_level(room: &Room, level: Level) -> Result<(), String> {
    let state = read(room).await?;
    if !state.can_change {
        return Err(crate::i18n::tr("Only the room's owner and maintainers can change agent work here.").to_owned());
    }
    room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, state.resumed_at)).await
        .map_err(|e| crate::i18n::format("Could not change agent work: {error}", &[("error", e.to_string())]))?;
    let (name, _) = who_and_when(room).await;
    post(room, lines::level_line(&name, level)).await
}

/// Pause the room (any member): this account's own pause event, then one room line.
pub async fn pause(room: &Room) -> Result<(), String> {
    let me = room.client().user_id().map(|u| u.to_string()).ok_or_else(|| crate::i18n::tr("Signed out").to_owned())?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    room.send_state_event_raw(ROOM_PAUSE_STATE_TYPE, &me, pause_content(now_ms)).await
        .map_err(|e| crate::i18n::format("Could not pause the room: {error}", &[("error", e.to_string())]))?;
    let (name, time) = who_and_when(room).await;
    post(room, lines::paused_line(&name, &time)).await
}

/// Resume (owner and maintainers): the level event's Resume point moves to
/// the newest pause read now, so a pause sent after it still counts.
pub async fn resume(room: &Room) -> Result<(), String> {
    let (state, events) = read_events(room).await?;
    if !state.can_change {
        return Err(crate::i18n::tr("Only the room's owner and maintainers can resume agent work here.").to_owned());
    }
    let level = state.level.clone().unwrap_or(Level::Off);
    let point = resume_point(&events, state.resumed_at);
    room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, point)).await
        .map_err(|e| crate::i18n::format("Could not resume the room: {error}", &[("error", e.to_string())]))?;
    let (name, time) = who_and_when(room).await;
    post(room, lines::resumed_line(&name, &time)).await
}
