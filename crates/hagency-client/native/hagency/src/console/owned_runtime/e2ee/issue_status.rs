//! The room's published issue states (ADR 0023) as the Agent's own device
//! sees them: the plaintext `io.hagency.issue_status` state events and the
//! room membership of everyone they name. An encrypted room has none (they
//! are never published there), and its state is never read for them.
use super::*;
use hagency_runtime::repo_status::{ISSUE_STATUS_STATE_TYPE, Member, linked::{ROOM_REPOS_STATE_TYPE, linked}, people_in};
use matrix_sdk::ruma::events::{StateEventType, room::member::MembershipState};
use serde_json::Value;
use std::collections::BTreeMap;

pub(in crate::console) struct StatusRoom {
    pub events: Vec<Value>,
    /// Membership and display name of every publisher and Agent the events name.
    pub members: BTreeMap<String, Member>,
    /// Repositories linked to the room by hand (`io.hagency.room_repos`).
    pub linked: Vec<String>,
}
impl AgentDevice {
    pub(in crate::console) async fn issue_status_room(&self, room: &str) -> Result<StatusRoom, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let security = joined.latest_encryption_state().await.map_err(|_| RuntimeError::Transport)?;
        if security.is_unknown() {
            return Err(RuntimeError::Transport);
        }
        if security.is_encrypted() {
            return Ok(StatusRoom { events: Vec::new(), members: BTreeMap::new(), linked: Vec::new() });
        }
        let raw = joined.get_state_events(StateEventType::from(ISSUE_STATUS_STATE_TYPE)).await.map_err(|_| RuntimeError::Transport)?;
        let events = raw.iter().map(|e| serde_json::to_value(e).map_err(|_| RuntimeError::Transport)).collect::<Result<Vec<_>, _>>()?;
        let mut members = BTreeMap::new();
        for id in people_in(&events) {
            let Ok(user) = OwnedUserId::try_from(id.as_str()) else { continue };
            let member = joined.get_member_no_sync(&user).await.map_err(|_| RuntimeError::Transport)?;
            let found = member.map(|m| Member { joined: *m.membership() == MembershipState::Join, name: m.display_name().map(str::to_owned) });
            members.insert(id, found.unwrap_or_default());
        }
        let repos = joined.get_state_event(StateEventType::from(ROOM_REPOS_STATE_TYPE), "").await.map_err(|_| RuntimeError::Transport)?;
        let repos = match repos {
            Some(raw) => serde_json::to_value(&raw).map_err(|_| RuntimeError::Transport)?,
            None => Value::Null,
        };
        Ok(StatusRoom { events, members, linked: linked(&repos["content"]) })
    }
}

impl AgentDevice {
    /// The repositories of `room` that widen the card-free read-only GitHub
    /// scope (ADR 0023) for a request from `requester`: the linked ones
    /// (their event needs the room's power level, which the homeserver
    /// enforces) and those other owners publish as watched, but only from a
    /// publisher whose own power level could have linked them. Empty when
    /// the requester is not a joined member.
    /// Also the room's own linked repositories alone, when the requester is
    /// the room's owner or a maintainer and the list's sender may still link
    /// them (ADR 0030: the only ones a card-free issue is filed on).
    pub(in crate::console) async fn read_scope(&self, room: &str, requester: &str, owners: &BTreeMap<String, String>, now: i64) -> Result<(Vec<String>, Vec<String>), RuntimeError> {
        use hagency_runtime::repo_status::{accepted, linked::room_repos};
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let asker = OwnedUserId::try_from(requester).map_err(|_| RuntimeError::Request)?;
        let member = joined.get_member_no_sync(&asker).await.map_err(|_| RuntimeError::Transport)?;
        if !member.is_some_and(|m| *m.membership() == MembershipState::Join) {
            return Ok((Vec::new(), Vec::new()));
        }
        let status = self.issue_status_room(room).await?;
        let levels = joined.power_levels().await.map_err(|_| RuntimeError::Transport)?;
        let may_link = |id: &str| OwnedUserId::try_from(id).is_ok_and(|u| levels.user_can_send_state(&u, StateEventType::from(ROOM_REPOS_STATE_TYPE)));
        let seen = accepted(&status.events, |id| status.members.get(id).cloned().unwrap_or_default(), |agent| owners.get(agent).cloned(), now).seen;
        let published = seen.iter().filter(|s| may_link(&s.publisher)).map(|s| s.issue.repo.as_str());
        // Card-free issue filing (ADR 0030): the linked list, its sender, the asker's
        // membership and the power levels all as the homeserver holds them now, never
        // the sync store's (batch GPT review); unreadable: none (fail closed).
        let state = server_state(&joined).await.unwrap_or_default();
        let levels = power_levels_of(&joined, &state);
        Ok((room_repos(published, &status.linked), issue_grant(&state, levels.as_ref(), &asker)))
    }
}

/// `room`'s full state as the homeserver holds it now (one read, never the
/// sync store); `None` when unreadable.
async fn server_state(room: &matrix_sdk::Room) -> Option<Vec<Value>> {
    use matrix_sdk::ruma::api::client::state::get_state_events::v3 as get_state;
    let reply = room.client().send(get_state::Request::new(room.room_id().to_owned())).await.ok()?;
    reply.room_state.iter().map(|e| serde_json::to_value(e).ok()).collect()
}

/// The state event of `kind` keyed `key` among `state`.
fn state_event<'a>(state: &'a [Value], kind: &str, key: &str) -> Option<&'a Value> {
    state.iter().find(|e| e["type"] == kind && e["state_key"] == key)
}

/// `room`'s power levels from the server's `state`; `None` when unreadable.
fn power_levels_of(room: &matrix_sdk::Room, state: &[Value]) -> Option<matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels> {
    use matrix_sdk::ruma::events::room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource};
    let content: RoomPowerLevelsEventContent = serde_json::from_value(state_event(state, "m.room.power_levels", "")?["content"].clone()).ok()?;
    let rules = room.clone_info().room_version_rules_or_default().authorization;
    Some(RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), &rules, room.creators()?))
}

/// The room's own linked repositories a card-free issue may be filed on for
/// `asker`, by the server's `state` and `levels`: the room is not encrypted
/// (a public issue would publish its private discussion; the card stays,
/// round-2 GPT review), the asker and the list's sender are joined, the asker
/// is the room's owner or a maintainer and the sender may still link them
/// ([`may_file_issues`]). Else none.
pub(in crate::console) fn issue_grant(state: &[Value], levels: Option<&matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels>, asker: &matrix_sdk::ruma::UserId) -> Vec<String> {
    if state_event(state, "m.room.encryption", "").is_some() {
        return Vec::new();
    }
    let joined = |user: &str| state_event(state, "m.room.member", user).is_some_and(|m| m["content"]["membership"] == "join");
    let Some(list) = state_event(state, ROOM_REPOS_STATE_TYPE, "") else { return Vec::new() };
    let sender = list["sender"].as_str().unwrap_or_default();
    let granted = joined(asker.as_str()) && joined(sender) && levels.is_some_and(|l| may_file_issues(l, asker, sender));
    if granted { linked(&list["content"]) } else { Vec::new() }
}

/// Whether `asker`'s request may file issues card-free on the room's linked
/// repositories (ADR 0030): the asker is the room's owner or a maintainer and
/// the list's sender may still link them, both under `levels`.
pub(in crate::console) fn may_file_issues(levels: &matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels, asker: &matrix_sdk::ruma::UserId, linked_by: &str) -> bool {
    // Never a list an Agent puppet or Hagency service account sent.
    let person = |u: &matrix_sdk::ruma::UserId| !u.localpart().starts_with(hagency_core::puppet::RESERVED_LOCALPART_PREFIX);
    let may_still_link = OwnedUserId::try_from(linked_by).is_ok_and(|u| person(&u) && levels.user_can_send_state(&u, StateEventType::from(ROOM_REPOS_STATE_TYPE)));
    may_still_link && levels.for_user(asker) >= matrix_sdk::ruma::Int::new_saturating(hagency_agent_local::room_autonomy::MAINTAINER_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::events::room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource};

    fn levels() -> RoomPowerLevels {
        let content: RoomPowerLevelsEventContent = serde_json::from_value(serde_json::json!({"users": {"@own:x": 100, "@mo:x": 50, "@_hagency_agt_bird:x": 100}, "state_default": 50})).unwrap();
        RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), &matrix_sdk::ruma::room_version_rules::AuthorizationRules::V1, Vec::new())
    }

    /// Owner decision (Nov 18): only the room's owner or a maintainer files issues card-free.
    #[test]
    fn only_the_owner_or_a_maintainer_files_issues_card_free() {
        let user = |id: &str| OwnedUserId::try_from(id).unwrap();
        assert!(may_file_issues(&levels(), &user("@mo:x"), "@own:x"), "a maintainer");
        assert!(may_file_issues(&levels(), &user("@own:x"), "@mo:x"), "the owner");
        assert!(!may_file_issues(&levels(), &user("@anna:x"), "@own:x"), "a plain member asks the card");
        assert!(!may_file_issues(&levels(), &user("@mo:x"), "@anna:x"), "a list a member could not have linked counts for nothing");
        assert!(!may_file_issues(&levels(), &user("@mo:x"), ""), "no list");
        assert!(!may_file_issues(&levels(), &user("@mo:x"), "@_hagency_agt_bird:x"), "never a list an Agent sent");
    }

    /// Batch GPT review: the issue grant goes by the server's state only. A
    /// maintainer whose server membership is gone, a list whose sender left,
    /// no list or unreadable levels: no grant.
    #[test]
    fn the_issue_grant_follows_the_servers_state() {
        let member = |id: &str, membership: &str| serde_json::json!({"type": "m.room.member", "state_key": id, "content": {"membership": membership}});
        let list = serde_json::json!({"type": ROOM_REPOS_STATE_TYPE, "state_key": "", "sender": "@own:x", "content": {"repos": ["acme/app"]}});
        let mo = OwnedUserId::try_from("@mo:x").unwrap();
        let state = vec![member("@own:x", "join"), member("@mo:x", "join"), list.clone()];
        assert_eq!(issue_grant(&state, Some(&levels()), &mo), ["acme/app"]);
        assert!(issue_grant(&state, None, &mo).is_empty(), "levels unreadable");
        assert!(issue_grant(&[member("@own:x", "join"), member("@mo:x", "leave"), list.clone()], Some(&levels()), &mo).is_empty(), "the asker left");
        assert!(issue_grant(&[member("@own:x", "leave"), member("@mo:x", "join"), list], Some(&levels()), &mo).is_empty(), "the list's sender left");
        assert!(issue_grant(&[member("@own:x", "join"), member("@mo:x", "join")], Some(&levels()), &mo).is_empty(), "no list");
        assert!(issue_grant(&[], Some(&levels()), &mo).is_empty(), "the server's state unread");
    }

    /// Round-2 GPT review: an encrypted room never grants card-free issue
    /// filing, whatever its linked list and levels say (a public issue would
    /// publish private discussion); the card stays.
    #[test]
    fn an_encrypted_room_never_grants_card_free_issues() {
        let member = |id: &str| serde_json::json!({"type": "m.room.member", "state_key": id, "content": {"membership": "join"}});
        let list = serde_json::json!({"type": ROOM_REPOS_STATE_TYPE, "state_key": "", "sender": "@own:x", "content": {"repos": ["acme/app"]}});
        let encryption = serde_json::json!({"type": "m.room.encryption", "state_key": "", "content": {"algorithm": "m.megolm.v1.aes-sha2"}});
        let mo = OwnedUserId::try_from("@mo:x").unwrap();
        assert!(issue_grant(&[member("@own:x"), member("@mo:x"), list, encryption], Some(&levels()), &mo).is_empty());
    }
}
