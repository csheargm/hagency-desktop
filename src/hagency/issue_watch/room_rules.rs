//! The room's merge rules (ADR 0029), desktop side. They are room state a
//! room admin sets; this reads them from the homeserver itself (never the
//! sync store or a local echo), accepts only the exact event of that room
//! from a person who may send it under the room's power levels, and records
//! a snapshot per watched repository in this device's ledger through the
//! owner API, every poll, so the merge gate judges by fresh rules.
use crate::hagency::backend::Service;
use ::hagency::issue_watch::safeguards::{MERGE_RULES_STATE_TYPE, MergePolicy, RoomRulesSnapshot, RoomRulesState, content_of, policy_text, render_policy};
use ::hagency::issue_watch::{IssueWatch, OwnerRules, watch_entries};
use matrix_sdk::Room;
use matrix_sdk::ruma::UserId;
use matrix_sdk::ruma::events::{StateEventType, room::power_levels::RoomPowerLevels};
use serde_json::{Value, json};

/// What a room's state says about its merge rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomRead {
    /// No rules event.
    None,
    /// An event that does not count, and why (it is ignored: a person merges).
    Refused(&'static str),
    /// The accepted event: its id, its sender and its content.
    Rules { event_id: String, sender: String, content: Value },
}

impl RoomRead {
    /// The rules as the gate judges them; a refused event counts as none.
    pub fn state(&self) -> RoomRulesState {
        match self {
            Self::Rules { content, .. } => RoomRulesState::of(Some(content)),
            Self::None | Self::Refused(_) => RoomRulesState::Absent,
        }
    }
}

const REFUSED_SENDER: &str = "set by someone who may not set them now";
const REFUSED_LEFT: &str = "set by someone who is no longer in the room";
const REFUSED_AGENT: &str = "set by an Agent or a Hagency service account";
const REFUSED_SHAPE: &str = "not a rules event of this room";

/// The room's rules event among the server's state `events` of `room_id`
/// (with its `m.room.member` events). `may_send(sender)`: whether the room's
/// power levels let the sender send the event now. The sender must still be
/// joined (GPT review: a kick or ban can leave a power level behind); an
/// Agent puppet or Hagency service account never counts.
pub fn accept(events: &[Value], room_id: &str, may_send: impl Fn(&str) -> bool) -> RoomRead {
    let Some(event) = events.iter().find(|e| e["type"] == MERGE_RULES_STATE_TYPE && e["state_key"] == "") else { return RoomRead::None };
    let (Some(event_id), Some(sender)) = (event["event_id"].as_str(), event["sender"].as_str()) else { return RoomRead::Refused(REFUSED_SHAPE) };
    let other_room = event["room_id"].as_str().is_some_and(|r| r != room_id);
    if other_room || !event_id.starts_with('$') || !event["content"].is_object() {
        return RoomRead::Refused(REFUSED_SHAPE);
    }
    let localpart = sender.strip_prefix('@').and_then(|s| s.split(':').next()).unwrap_or_default();
    if localpart.starts_with(hagency_core::puppet::RESERVED_LOCALPART_PREFIX) {
        return RoomRead::Refused(REFUSED_AGENT);
    }
    if !joined(events, sender) {
        return RoomRead::Refused(REFUSED_LEFT);
    }
    if !may_send(sender) {
        return RoomRead::Refused(REFUSED_SENDER);
    }
    RoomRead::Rules { event_id: event_id.to_owned(), sender: sender.to_owned(), content: event["content"].clone() }
}

/// Whether `user` is joined, by the server's `m.room.member` state in `events`.
pub fn joined(events: &[Value], user: &str) -> bool {
    events.iter().any(|e| e["type"] == MEMBER_EVENT && e["state_key"] == user && e["content"]["membership"] == "join")
}
const MEMBER_EVENT: &str = "m.room.member";

/// The owner-API snapshot of `read` for `repo` in `room_id`; rules without
/// a text count as none.
pub fn snapshot(repo: &str, room_id: &str, read: &RoomRead) -> RoomRulesSnapshot {
    let base = RoomRulesSnapshot { repo: repo.to_owned(), room_id: room_id.to_owned(), ..Default::default() };
    match read {
        RoomRead::Rules { event_id, sender, content } if policy_text(content).is_some() => RoomRulesSnapshot {
            event_id: Some(event_id.clone()), sender: Some(sender.clone()), policy: policy_text(content).map(str::to_owned), ..base
        },
        _ => base,
    }
}

/// Whether `user` may set the merge rules under `levels`: the room's actual
/// required level for the event, so the admin-only entry (100) a Hagency
/// desktop adds (call_power_levels) keeps moderators out.
pub fn may_set_rules(levels: &RoomPowerLevels, user: &UserId) -> bool {
    levels.user_can_send_state(user, StateEventType::from(MERGE_RULES_STATE_TYPE))
}

/// Whether this account may set `room`'s merge rules (the room's power levels decide).
pub async fn can_edit(room: &Room) -> bool {
    let Some(user) = room.client().user_id().map(ToOwned::to_owned) else { return false };
    room.power_levels().await.is_ok_and(|levels| may_set_rules(&levels, &user))
}

/// `room`'s rules, read from the homeserver now (rules and membership in one
/// response). This account must be joined, or nothing counts.
pub async fn read(room: &Room) -> Result<RoomRead, String> {
    let events = crate::home::room_memory::server_state::events_of_types(&room.client(), room.room_id(), &[MERGE_RULES_STATE_TYPE, MEMBER_EVENT]).await?;
    let me = room.client().user_id().map(|u| u.to_string()).unwrap_or_default();
    if room.state() != matrix_sdk::RoomState::Joined || !joined(&events, &me) {
        return Err(crate::i18n::tr("this desktop has not joined the watch's room").to_owned());
    }
    // Unreadable power levels: nobody's rules count (fail closed).
    let levels = room.power_levels().await.ok();
    let may_send = |sender: &str| {
        let user = UserId::parse(sender).ok();
        levels.as_ref().zip(user).is_some_and(|(l, u)| may_set_rules(l, &u))
    };
    Ok(accept(&events, room.room_id().as_str(), may_send))
}

/// Save `policy` as `room`'s rules; the homeserver refuses a member below the event's level.
pub async fn save(room: &Room, policy: &MergePolicy) -> Result<(), String> {
    let text = render_policy(policy).ok_or_else(|| crate::i18n::tr("A path holds a character the merge rules cannot store").to_owned())?;
    room.send_state_event_raw(MERGE_RULES_STATE_TYPE, "", content_of(&text)).await.map(|_| ())
        .map_err(|e| crate::i18n::format("Could not save the merge rules: {error}", &[("error", e.to_string())]))
}

/// Every `(repo, room)` this Agent's saved watch reports to.
pub(crate) fn watched_rooms(saved: &Value) -> Vec<(String, String)> {
    let watch: IssueWatch = serde_json::from_value(saved["issueWatch"].clone()).unwrap_or_default();
    let work: Vec<OwnerRules> = serde_json::from_value(saved["work"].clone()).unwrap_or_default();
    // A paused watch supplies no rules (GPT review).
    watch_entries(&watch, &work).into_iter().filter(|e| e.active && !e.room_id.is_empty()).map(|e| (e.repo, e.room_id)).collect()
}

/// Records the current rules of every room this Agent's watches report to;
/// the failures, for the check's status line. A room not read is not
/// recorded: its old snapshot ages out and merges need a person.
pub(super) async fn sync(service: &Service, agent_id: &str, saved: &Value) -> Vec<String> {
    let Some(client) = crate::sliding_sync::get_client() else { return Vec::new() };
    let (mut snapshots, mut errors) = (Vec::new(), Vec::new());
    for (repo, room_id) in watched_rooms(saved) {
        let joined = matrix_sdk::ruma::RoomId::parse(&room_id).ok().and_then(|id| client.get_room(&id));
        match joined {
            Some(room) => match read(&room).await {
                Ok(read) => snapshots.push(snapshot(&repo, &room_id, &read)),
                Err(error) => errors.push(crate::i18n::format("Merge rules could not be read: {error}", &[("error", error)])),
            },
            None => errors.push(crate::i18n::tr("Merge rules could not be read: this desktop has not joined the watch's room").to_owned()),
        }
    }
    if !snapshots.is_empty()
        && let Err(error) = super::live::saved(service, agent_id, Some(json!({"roomMergeRules": snapshots_json(&snapshots)}))).await
    {
        errors.push(crate::i18n::format("Merge rules could not be recorded: {error}", &[("error", error)]));
    }
    errors
}

/// The owner API's `roomMergeRules` list.
pub(crate) fn snapshots_json(snapshots: &[RoomRulesSnapshot]) -> Value {
    snapshots.iter().map(|s| json!({"repo": s.repo, "roomId": s.room_id, "eventId": s.event_id, "sender": s.sender, "policy": s.policy})).collect()
}

#[cfg(test)]
#[path = "room_rules_tests.rs"]
mod tests;
