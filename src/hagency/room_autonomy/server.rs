//! The room's work loop controls on the homeserver (ADR 0030): one full
//! state read (never the sync store or a local echo) for the level, pauses,
//! linked repositories, membership and power levels, and the writes the
//! Status tab makes. The homeserver refuses anyone below an event's level;
//! the checks here only spare a refused request.
use super::lines;
use super::state::{Rights, RoomState, state_of};
use crate::hagency::issue_watch::room_rules::{MEMBER_EVENT, POWER_LEVELS_EVENT, joined, power_levels_event};
use ::hagency::issue_watch::safeguards::room_autonomy::{
    Level, MAINTAINER_LEVEL, ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, level_content, pause_content, resume_clears,
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

/// Whether `user` may send state `kind` under `levels` now; the level also
/// needs the room's owner or a maintainer (GPT review: a permissive room's
/// event level alone is not the role).
pub fn may_send_now(levels: &RoomPowerLevels, user: &UserId, kind: &str) -> bool {
    let role = kind != ROOM_AUTONOMY_STATE_TYPE || levels.for_user(user) >= Int::new_saturating(MAINTAINER_LEVEL);
    role && levels.user_can_send_state(user, StateEventType::from(kind))
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
        levels.as_ref().zip(user).is_some_and(|(l, u)| may_send_now(l, &u, kind))
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
    room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, &state.cleared)).await
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

/// Resume (owner and maintainers): the level event clears exactly the pause
/// events read now, so a pause sent after the read still counts.
pub async fn resume(room: &Room) -> Result<(), String> {
    let (state, events) = read_events(room).await?;
    if !state.can_change {
        return Err(crate::i18n::tr("Only the room's owner and maintainers can resume agent work here.").to_owned());
    }
    let level = state.level.clone().unwrap_or(Level::Off);
    room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, &resume_clears(&events))).await
        .map_err(|e| crate::i18n::format("Could not resume the room: {error}", &[("error", e.to_string())]))?;
    let (name, time) = who_and_when(room).await;
    post(room, lines::resumed_line(&name, &time)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::events::room::power_levels::{RoomPowerLevelsEventContent, RoomPowerLevelsSource};
    use matrix_sdk::ruma::user_id;

    fn levels(content: serde_json::Value) -> RoomPowerLevels {
        let content: RoomPowerLevelsEventContent = serde_json::from_value(content).unwrap();
        RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), &matrix_sdk::ruma::room_version_rules::AuthorizationRules::V10, Vec::new())
    }

    /// Owner decision (Nov 18): the owner does everything, a maintainer changes the
    /// level and resumes, a member only pauses; a permissive room changes none of that.
    #[test]
    fn roles_decide_who_changes_the_level_and_who_only_pauses() {
        let room = levels(serde_json::json!({"users": {"@own:x": 100, "@mo:x": 50}, "state_default": 50,
            "events": {ROOM_AUTONOMY_STATE_TYPE: 50, ROOM_PAUSE_STATE_TYPE: 0, "io.hagency.merge_rules": 100, "m.room.power_levels": 100}}));
        let (own, mo, member) = (rights_of(&room, user_id!("@own:x")), rights_of(&room, user_id!("@mo:x")), rights_of(&room, user_id!("@me:x")));
        assert!(own.change_level && own.pause && own.maintainer);
        assert!(mo.change_level && mo.pause && mo.maintainer, "a maintainer changes the level and resumes");
        assert!(!room.user_can_send_state(user_id!("@mo:x"), StateEventType::from("io.hagency.merge_rules")), "but not the merge rules");
        assert!(!room.user_can_send_state(user_id!("@mo:x"), StateEventType::RoomPowerLevels), "nor maintainers");
        assert!(!member.change_level && member.pause && !member.maintainer, "a member only pauses");
        let permissive = levels(serde_json::json!({"users": {"@own:x": 100}, "state_default": 0}));
        assert!(!may_send_now(&permissive, user_id!("@me:x"), ROOM_AUTONOMY_STATE_TYPE), "a permissive room does not make a member a maintainer");
        assert!(may_send_now(&permissive, user_id!("@me:x"), ROOM_PAUSE_STATE_TYPE));
        let r = rights_of(&permissive, user_id!("@me:x"));
        assert!(r.change_level && !r.maintainer, "so the Status tab's can_change (both) stays off");
    }
}
