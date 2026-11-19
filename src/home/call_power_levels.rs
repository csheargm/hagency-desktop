//! Let every member of a private room the desktop creates join an Element Call
//! and publish their own Agents' issue status (ADR 0023).
//! MatrixRTC (Element Call v0.16) publishes each participant as an
//! `org.matrix.msc3401.call.member` state event; under the private_chat preset
//! members have PL 0 while state needs 50, so non-moderators got 403.
//!
//! This is a read-modify-write of the room's power levels right after creation,
//! not `power_level_content_override`: servers replace the whole `events` map
//! from an override, which would drop server defaults (v12 tombstone at 150)
//! and any operator-configured restrictions. Here only the call key changes.
//!
//! Rooms made before the issue status existed lack its entry; a moderator or
//! admin's desktop adds that one entry when they link a repository
//! ([`allow_member_state_in`]): an explicit action, never a background load.
//!
//! The merge rules (ADR 0029) decide what Agents may merge, so they are for
//! room admins only: both paths add `events[MERGE_RULES_STATE_TYPE] = 100`
//! (or a stricter `state_default`) when the room has no entry. The autonomy
//! level (ADR 0030) gets the maintainer level (50) and a member's pause 0,
//! the same way. An entry already there is an admin's choice and is never
//! lowered or overwritten.
use matrix_sdk::{
    Room,
    ruma::{Int, OwnedRoomId, OwnedUserId, RoomId, UserId, api::client::state::get_state_event_for_key::v3 as get_state, events::{StateEventType, TimelineEventType, room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource}}, room_version_rules::AuthorizationRules},
};
use serde_json::Value;

/// MatrixRTC membership state event used by Element Call v0.16.
pub const CALL_MEMBER_EVENT: &str = "org.matrix.msc3401.call.member";
const MEMBER_LEVEL: i64 = 0;
/// The Matrix presets' admin level.
const ADMIN_LEVEL: i64 = 100;
/// State every member may write. The issue status (ADR 0023) is keyed by its
/// sender's own Matrix ID, which the homeserver lets nobody else write, so
/// opening it lets each owner publish only their own Agents' states.
/// A pause (ADR 0030) is keyed by its sender's own Matrix ID too: any member
/// may pause the room's agent work, never resume it.
const MEMBER_STATE_EVENTS: [&str; 3] = [CALL_MEMBER_EVENT, ::hagency::repo_status::ISSUE_STATUS_STATE_TYPE, PAUSE_EVENT];
const PAUSE_EVENT: &str = ::hagency::issue_watch::safeguards::room_autonomy::ROOM_PAUSE_STATE_TYPE;
/// State only room admins may write; added at [`ADMIN_LEVEL`] only where missing.
const ADMIN_STATE_EVENTS: [&str; 1] = [::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE];
/// State the room's owner and maintainers write (ADR 0030: the autonomy
/// level, which also carries Resume); added at the maintainer level only where missing.
const MAINTAINER_STATE_EVENTS: [&str; 1] = [::hagency::issue_watch::safeguards::room_autonomy::ROOM_AUTONOMY_STATE_TYPE];
/// What an existing room gets (GPT review): only Hagency's own state, each
/// entry only when missing; call membership stays as that room's admins set it.
const EXISTING_ROOM_EVENTS: [&str; 4] = [::hagency::repo_status::ISSUE_STATUS_STATE_TYPE, ADMIN_STATE_EVENTS[0], MAINTAINER_STATE_EVENTS[0], PAUSE_EVENT];
/// The keys every room this desktop creates gets when missing, besides the call keys.
const CREATED_ROOM_EVENTS: [&str; 3] = [ADMIN_STATE_EVENTS[0], MAINTAINER_STATE_EVENTS[0], PAUSE_EVENT];
/// Bound on the follow-up so a stalled server cannot leave creation at "Creating…".
const FOLLOW_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The Matrix default `state_default` when power levels leave it out.
const DEFAULT_STATE_LEVEL: i64 = 50;

/// The level Hagency gives `key`: members for member state; for admin-only
/// state at least admins, never below a stricter `state_default` the room
/// already applies to it (GPT review: filling the entry must not loosen it).
fn target_level(key: &str, state_default: i64) -> i64 {
    if ADMIN_STATE_EVENTS.contains(&key) {
        ADMIN_LEVEL.max(state_default)
    } else if MAINTAINER_STATE_EVENTS.contains(&key) {
        ::hagency::issue_watch::safeguards::room_autonomy::MAINTAINER_LEVEL.max(state_default)
    } else {
        MEMBER_LEVEL
    }
}

/// For a room just created: call membership and issue status opened to PL 0,
/// and the merge rules at PL 100 when they have no entry; `None` when nothing
/// changes (nothing to send). Every other field is kept verbatim.
pub fn open_call_membership(levels: Value) -> anyhow::Result<Option<Value>> {
    let keys: Vec<&'static str> = MEMBER_STATE_EVENTS.into_iter().chain(ADMIN_STATE_EVENTS).chain(MAINTAINER_STATE_EVENTS).collect();
    set_where(levels, &keys, |key, level| match level {
        None => true,
        Some(level) => MEMBER_STATE_EVENTS.contains(&key) && level.as_i64() != Some(MEMBER_LEVEL),
    })
}

/// For an existing room: each of `keys` with no entry at all gets Hagency's
/// level for it, or `None` when every one has an entry. An entry an admin
/// set, even a raised or lowered one, is never changed.
pub fn add_missing_member_state(levels: Value, keys: &[&'static str]) -> anyhow::Result<Option<Value>> {
    set_where(levels, keys, |_, level| level.is_none())
}

/// Sets each of `keys` whose current entry `reset` selects to its [`target_level`].
fn set_where(mut levels: Value, keys: &[&'static str], reset: impl Fn(&str, Option<&Value>) -> bool) -> anyhow::Result<Option<Value>> {
    let object = levels.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels are not an object"))?;
    // Room versions before 10 also allow a level as a string ("150"), as Ruma reads it (GPT review).
    let level = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
    let state_default = object.get("state_default").and_then(level).unwrap_or(DEFAULT_STATE_LEVEL);
    let events = object.entry("events").or_insert_with(|| Value::Object(Default::default()));
    let events = events.as_object_mut().ok_or_else(|| anyhow::anyhow!("power levels events is not an object"))?;
    let changed: Vec<&str> = keys.iter().copied().filter(|key| reset(key, events.get(*key))).collect();
    if changed.is_empty() {
        return Ok(None);
    }
    for key in changed {
        events.insert(key.into(), target_level(key, state_default).into());
    }
    Ok(Some(levels))
}

/// The existing-room Hagency state events these power levels give no entry.
pub fn missing_member_state(levels: &RoomPowerLevels) -> Vec<&'static str> {
    EXISTING_ROOM_EVENTS.into_iter().filter(|key| !levels.events.contains_key(&TimelineEventType::from(*key))).collect()
}

/// The missing entries `me` may add: they may change power levels, already
/// reach the event's current level and reach the level it gets, as the
/// homeserver's power-level rule requires of a changed entry. So a moderator
/// allowed to change power levels opens issue status, but only an admin adds
/// the admin-only merge-rules entry; a plain member adds nothing.
pub fn addable_member_state(levels: &RoomPowerLevels, me: &UserId) -> Vec<&'static str> {
    if !levels.user_can_send_state(me, StateEventType::RoomPowerLevels) {
        return Vec::new();
    }
    let mine = levels.for_user(me);
    missing_member_state(levels).into_iter()
        .filter(|key| levels.user_can_send_state(me, StateEventType::from(*key)) && mine >= Int::new_saturating(target_level(key, i64::from(levels.state_default))))
        .collect()
}

/// `room`'s power-level content as the homeserver has it now (not the sync store).
pub async fn server_levels(room: &Room) -> anyhow::Result<Value> {
    let request = get_state::Request::new(room.room_id().to_owned(), StateEventType::RoomPowerLevels, String::new());
    Ok(serde_json::from_str(room.client().send(request).await?.event_or_content.get())?)
}

/// One power-level change at a time per room from this desktop: the Members
/// tab's role change, a repository link and the after-create follow-up each
/// write the room's whole `m.room.power_levels`, so two at once would each put
/// back the other's old value (GPT review: a removed maintainer restored by a
/// link). Per room (GPT review): a slow change in one room never makes
/// another room's link time out.
static CHANGING: RoomLocks = RoomLocks::new();

/// One async lock per room, made on first use: a lock domain whose holders
/// wait on the network serialises one room, never every room (GPT review).
// TODO(R3): `agent_replies_setting::room_lock` can use this too.
pub struct RoomLocks(std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<OwnedRoomId, std::sync::Arc<tokio::sync::Mutex<()>>>>>);

impl RoomLocks {
    pub const fn new() -> Self {
        Self(std::sync::LazyLock::new(Default::default))
    }
    /// `room_id`'s lock in this domain.
    pub fn of(&self, room_id: &RoomId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).entry(room_id.to_owned()).or_default().clone()
    }
}

/// The lock serialising `room_id`'s power-level changes on this desktop.
fn room_lock(room_id: &RoomId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    CHANGING.of(room_id)
}
/// Writes of one change: the first, and one more when another write undid it.
const WRITE_ATTEMPTS: usize = 2;

/// Applies `change` to the room's power levels as the server has them now
/// (read right before each write); `change` returns `None` when nothing needs
/// sending. Matrix has no conditional state write, so another client's change
/// landing between the read and the write can still undo this one: the levels
/// are read again after the write, and when `change` would still alter them
/// it is made once more on the newer levels; after that the race is reported.
/// Not closed (GPT review, not adoptable client-side): another client whose
/// stale snapshot lands *after* this change was verified undoes it unseen, and
/// that client checks only its own entries. Only a conditional state write or
/// a single authoritative writer would close it; a removed maintainer should
/// be checked on the Members tab after concurrent admin edits.
pub async fn change_levels(room: &Room, mut change: impl FnMut(Value) -> anyhow::Result<Option<Value>>) -> anyhow::Result<()> {
    let lock = room_lock(room.room_id());
    let _one = lock.lock().await;
    for _ in 0..WRITE_ATTEMPTS {
        let Some(updated) = change(server_levels(room).await?)? else { return Ok(()) };
        room.send_state_event_raw("m.room.power_levels", "", updated).await?;
        let now = server_levels(room).await?;
        if change(now.clone())?.is_none_or(|again| again == now) {
            return Ok(());
        }
    }
    anyhow::bail!(crate::i18n::tr("Another change to the room's power levels landed at the same time and undid this one. Try again.").to_owned())
}

/// Adds the missing Hagency entries this account may add to an existing room;
/// returns the missing ones it may not add, for the link to name (empty: none).
/// Judged on the server's levels (GPT review), never the sync store, through
/// [`change_levels`]; it runs only on an admin's explicit link, never in the background.
pub async fn allow_member_state_in(room: &Room) -> anyhow::Result<Vec<&'static str>> {
    let me = room.client().user_id().ok_or_else(|| anyhow::anyhow!("signed out"))?.to_owned();
    let mut left = Vec::new();
    let send = change_levels(room, |current| {
        let typed = typed_levels(room, current.clone()).ok_or_else(|| anyhow::anyhow!("the room's power levels are unreadable"))?;
        let addable = addable_member_state(&typed, &me);
        left = missing_member_state(&typed).into_iter().filter(|key| !addable.contains(key)).collect();
        add_missing_member_state(current, &addable)
    });
    tokio::time::timeout(FOLLOW_UP_TIMEOUT, send).await.map_err(|_| anyhow::anyhow!("timed out"))??;
    Ok(left)
}

/// Power-level `content` as `room`'s typed power levels (its version's rules
/// and creators), or `None` when unreadable.
pub fn typed_levels(room: &Room, content: Value) -> Option<RoomPowerLevels> {
    let rules = room.clone_info().room_version_rules_or_default().authorization;
    levels_from(content, &rules, room.creators()?)
}

/// Power-level `content` under `rules` with `creators`, or `None` when unreadable.
pub fn levels_from(content: Value, rules: &AuthorizationRules, creators: Vec<OwnedUserId>) -> Option<RoomPowerLevels> {
    let content: RoomPowerLevelsEventContent = serde_json::from_value(content).ok()?;
    Some(RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), rules, creators))
}

/// Hagency's levels for a room the user just created (as creator they may
/// edit power levels): `open_calls` (a private room) also opens call
/// membership and issue status ([`open_call_membership`]); every room gets
/// the admin-only merge-rules entry when missing.
pub async fn allow_call_membership(room: &Room, open_calls: bool) -> anyhow::Result<()> {
    // The room was created a moment ago by this user with no other members; the
    // shared lock still keeps it from crossing a link this desktop makes meanwhile.
    change_levels(room, |current| if open_calls { open_call_membership(current) } else { add_missing_member_state(current, &CREATED_ROOM_EVENTS) }).await
}

/// Creation flows call this once a (non-space) room exists. A failure is
/// deliberately not a creation failure (GPT review, not adopted): the room
/// has only its creator, so no moderator exists until that admin promotes
/// one, and reporting an error would invite a duplicate room; the room then behaves as
/// before this fix (calls need a moderator, merge rules follow
/// `state_default`) until an admin links a repository, which retries the
/// merge-rules entry ([`allow_member_state_in`]); the warning records why.
pub async fn allow_call_membership_after_create(room: &Room, open_calls: bool) {
    let outcome = tokio::time::timeout(FOLLOW_UP_TIMEOUT, allow_call_membership(room, open_calls)).await;
    let error = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(error)) => format!("{error:#}"),
        Err(_) => "timed out".to_owned(),
    };
    makepad_widgets::warning!("room {} created but its power levels (calls, admin-only merge rules) not set: {error}", room.room_id());
}

#[cfg(test)]
#[path = "call_power_levels_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "call_power_levels_member_tests.rs"]
mod member_tests;
