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

/// `room`'s loop state, read from the homeserver now; also shown in the
/// room's Status tab and header when they show it (the loop's check keeps them current).
pub async fn read(room: &Room) -> Result<RoomState, String> {
    let ticket = crate::home::room_watches::autonomy::observe(room.room_id());
    let read = read_events(room).await.map(|(state, _)| state);
    crate::home::room_watches::autonomy::observed(ticket, room.room_id(), &read);
    read
}

/// This account's name for room lines, and the local time now.
async fn who_and_when(room: &Room) -> (String, String) {
    let me = room.client().user_id().map(|u| u.to_string()).unwrap_or_default();
    let name = room.client().account().get_display_name().await.ok().flatten().filter(|n| !n.trim().is_empty());
    (name.unwrap_or(me), chrono::Local::now().format("%H:%M").to_string())
}

async fn post(room: &Room, text: String) -> Result<(), String> {
    room.send(RoomMessageEventContent::text_plain(text)).await.map(|_| ()).map_err(|e| e.to_string())
}

/// What a change returns once its state event is saved (the commit): `None`,
/// or why the room line announcing it could not be posted. The change holds
/// either way (GPT review: a refused line must not skip stopping runs).
pub type Announced = Option<String>;

/// `write` (the state event, the commit), then `after` (what the commit
/// starts, such as stopping runs) and `announce` (the room line) together, so
/// a slow line never holds `after` back (GPT review). Only a failed write is
/// an error; a failed line comes back as [`Announced`].
pub async fn commit_then<T>(
    write: impl std::future::Future<Output = Result<(), String>>,
    after: impl std::future::Future<Output = T>,
    announce: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(T, Announced), String> {
    write.await?;
    let (done, announced) = tokio::join!(after, announce);
    Ok((done, announced.err().map(|error| crate::i18n::format("The change is saved, but the room was not told: {error}", &[("error", error)]))))
}

/// Set the level (owner and maintainers), keeping the Resume point; one room line.
pub async fn set_level(room: &Room, level: Level) -> Result<Announced, String> {
    set_level_if(room, level, |_| true).await.map(|written| written.unwrap_or_default())
}

/// [`set_level`] only while `still` holds for the room's state read right
/// before the write (GPT review P2: the migration's "unset" was judged on an
/// earlier read and could overwrite a maintainer's Off); `Err(state)` when it
/// no longer held and nothing was written. Matrix has no conditional state
/// write, so this narrows the window to one request; it cannot close it.
pub async fn set_level_if(room: &Room, level: Level, still: impl Fn(&RoomState) -> bool) -> Result<Result<Announced, RoomState>, String> {
    super::writes::serialized(room.room_id(), write_level(room, level, still)).await
}

/// The Status tab's level choice `ticket` ([`super::writes::choose`]):
/// written after the room's earlier level writes, unless a newer choice
/// superseded it meanwhile (`None`: not written).
pub async fn choose_level(room: &Room, level: Level, ticket: u64) -> Option<Result<Announced, String>> {
    let written = super::writes::latest_only(room.room_id(), ticket, write_level(room, level, |_| true)).await?;
    Some(written.map(|w| w.unwrap_or_default()))
}

/// [`set_level_if`]'s read and write; callers hold the room's level lock.
async fn write_level(room: &Room, level: Level, still: impl Fn(&RoomState) -> bool) -> Result<Result<Announced, RoomState>, String> {
    let state = read(room).await?;
    if !still(&state) {
        return Ok(Err(state));
    }
    if !state.can_change {
        return Err(crate::i18n::tr("Only the room's owner and maintainers can change agent work here.").to_owned());
    }
    let write = async {
        room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, &state.cleared)).await.map(|_| ())
            .map_err(|e| crate::i18n::format("Could not change agent work: {error}", &[("error", e.to_string())]))
    };
    let pushed = super::gate::push(room, super::gate::Saved::Level(level));
    commit_then(write, pushed, async { post(room, lines::level_line(&who_and_when(room).await.0, level)).await }).await.map(|(p, a)| Ok(told(a, p)))
}

/// The note after a saved change: the room line's failure and the gate's, if any.
fn told(announced: Announced, pushed: Result<(), String>) -> Announced {
    let notes: Vec<String> = announced.into_iter().chain(pushed.err().map(super::gate::not_pushed)).collect();
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// Pause the room (any member): this account's own pause event, then `after`
/// (stopping this desktop's runs) alongside one room line.
pub async fn pause<T>(room: &Room, after: impl std::future::Future<Output = T>) -> Result<(T, Announced), String> {
    let me = room.client().user_id().map(|u| u.to_string()).ok_or_else(|| crate::i18n::tr("Signed out").to_owned())?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let write = async {
        room.send_state_event_raw(ROOM_PAUSE_STATE_TYPE, &me, pause_content(now_ms)).await.map(|_| ())
            .map_err(|e| crate::i18n::format("Could not pause the room: {error}", &[("error", e.to_string())]))
    };
    // The gate hears of the pause now, not on the poller's next check (GPT review P1).
    let after = async { tokio::join!(after, super::gate::push(room, super::gate::Saved::Paused)) };
    commit_then(write, after, async {
        let (name, time) = who_and_when(room).await;
        post(room, lines::paused_line(&name, &time)).await
    }).await.map(|((done, pushed), a)| (done, told(a, pushed)))
}

/// Resume (owner and maintainers): the level event clears exactly the pause
/// events read now, so a pause sent after the read still counts. It holds the
/// room's level lock and reads under it, so a level saved meanwhile on this
/// desktop (an Off) is the one it writes back (GPT review P2).
pub async fn resume(room: &Room) -> Result<Announced, String> {
    super::writes::serialized(room.room_id(), resume_now(room)).await
}

async fn resume_now(room: &Room) -> Result<Announced, String> {
    let (state, events) = read_events(room).await?;
    if !state.can_change {
        return Err(crate::i18n::tr("Only the room's owner and maintainers can resume agent work here.").to_owned());
    }
    let level = state.level.clone().unwrap_or(Level::Off);
    let write = async {
        room.send_state_event_raw(ROOM_AUTONOMY_STATE_TYPE, "", level_content(level, &resume_clears(&events))).await.map(|_| ())
            .map_err(|e| crate::i18n::format("Could not resume the room: {error}", &[("error", e.to_string())]))
    };
    commit_then(write, super::gate::push(room, super::gate::Saved::Resumed), async {
        let (name, time) = who_and_when(room).await;
        post(room, lines::resumed_line(&name, &time)).await
    }).await.map(|(p, a)| told(a, p))
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

    /// GPT review (P2): the state event is the commit. A refused room line is
    /// reported on its own; a refused state event is the error and posts nothing;
    /// what the commit starts never waits for the line.
    #[test]
    fn a_saved_change_holds_even_when_its_room_line_is_refused() {
        fn run<F: std::future::Future>(f: F) -> F::Output { tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f) }
        let refused = run(commit_then(async { Ok(()) }, async { 2 }, async { Err("M_FORBIDDEN".to_owned()) }));
        assert_eq!(refused, Ok((2, Some("The change is saved, but the room was not told: M_FORBIDDEN".to_owned()))));
        let posted = std::cell::Cell::new(false);
        let failed = run(commit_then(async { Err("no".to_owned()) }, async {}, async { posted.set(true); Ok(()) }));
        assert_eq!(failed, Err("no".to_owned()));
        assert!(!posted.get(), "nothing is announced when the change was not saved");
        // The line finishes only once the runs were stopped: run in turn, this would never end.
        let (stopped, line) = tokio::sync::oneshot::channel::<()>();
        let both = run(commit_then(async { Ok(()) }, async { stopped.send(()).unwrap(); 1 }, async { line.await.map_err(|e| e.to_string()) }));
        assert_eq!(both, Ok((1, None)));
    }
}
