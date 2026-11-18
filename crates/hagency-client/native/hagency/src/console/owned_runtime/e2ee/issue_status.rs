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
    /// Who sent the linked list (empty: no list).
    pub linked_by: String,
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
            return Ok(StatusRoom { events: Vec::new(), members: BTreeMap::new(), linked: Vec::new(), linked_by: String::new() });
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
        let linked_by = repos["sender"].as_str().unwrap_or_default().to_owned();
        Ok(StatusRoom { events, members, linked: linked(&repos["content"]), linked_by })
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
        // Card-free issue filing (ADR 0030) only for the room's owner or a maintainer,
        // by the server's power levels read now, never the sync store's.
        let maintainer = server_levels(&joined).await.is_some_and(|l| may_file_issues(&l, &asker, &status.linked_by));
        let linked = if maintainer { status.linked.clone() } else { Vec::new() };
        Ok((room_repos(published, &status.linked), linked))
    }
}

/// `room`'s power levels as the homeserver holds them now; `None` when unreadable.
async fn server_levels(room: &matrix_sdk::Room) -> Option<matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels> {
    use matrix_sdk::ruma::api::client::state::get_state_event_for_key::v3 as get_state;
    use matrix_sdk::ruma::events::room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource};
    let request = get_state::Request::new(room.room_id().to_owned(), StateEventType::RoomPowerLevels, String::new());
    let reply = room.client().send(request).await.ok()?;
    let content: RoomPowerLevelsEventContent = serde_json::from_str(reply.event_or_content.get()).ok()?;
    let rules = room.clone_info().room_version_rules_or_default().authorization;
    Some(RoomPowerLevels::new(RoomPowerLevelsSource::Original(content), &rules, room.creators()?))
}

/// Whether `asker`'s request may file issues card-free on the room's linked
/// repositories (ADR 0030): the asker is the room's owner or a maintainer and
/// the list's sender may still link them, both under `levels`.
pub(in crate::console) fn may_file_issues(levels: &matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels, asker: &matrix_sdk::ruma::UserId, linked_by: &str) -> bool {
    let may_still_link = OwnedUserId::try_from(linked_by).is_ok_and(|u| levels.user_can_send_state(&u, StateEventType::from(ROOM_REPOS_STATE_TYPE)));
    may_still_link && levels.for_user(asker) >= matrix_sdk::ruma::Int::new_saturating(hagency_agent_local::room_autonomy::MAINTAINER_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::events::room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource};

    fn levels() -> RoomPowerLevels {
        let content: RoomPowerLevelsEventContent = serde_json::from_value(serde_json::json!({"users": {"@own:x": 100, "@mo:x": 50}, "state_default": 50})).unwrap();
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
    }
}
