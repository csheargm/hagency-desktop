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
    pub(in crate::console) async fn read_scope(&self, room: &str, requester: &str) -> Result<Vec<String>, RuntimeError> {
        use hagency_runtime::repo_status::{accepted, linked::room_repos};
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let asker = OwnedUserId::try_from(requester).map_err(|_| RuntimeError::Request)?;
        let member = joined.get_member_no_sync(&asker).await.map_err(|_| RuntimeError::Transport)?;
        if !member.is_some_and(|m| *m.membership() == MembershipState::Join) {
            return Ok(Vec::new());
        }
        let status = self.issue_status_room(room).await?;
        let levels = joined.power_levels().await.map_err(|_| RuntimeError::Transport)?;
        let may_link = |id: &str| OwnedUserId::try_from(id).is_ok_and(|u| levels.user_can_send_state(&u, StateEventType::from(ROOM_REPOS_STATE_TYPE)));
        let seen = accepted(&status.events, |id| status.members.get(id).cloned().unwrap_or_default()).seen;
        let published = seen.iter().filter(|s| may_link(&s.publisher)).map(|s| s.issue.repo.as_str());
        Ok(room_repos(published, &status.linked))
    }
}
