//! Device-side reads behind the encrypted owned-turn room context: the
//! Agent's own join membership event and display names come from its synced
//! room state, and timeline pages are the SDK-decrypted history records, cut
//! at that join event in timeline order (never by `origin_server_ts`, which
//! the sending homeserver chooses).
use super::*;
use super::history::history_event;
use crate::console::owned_runtime::encrypted_context::{RecentSource, cut_at_join};
use matrix_sdk::ruma::events::room::member::MembershipState;

impl RecentSource for AgentDevice {
    async fn join_event(&self, room: &str) -> Result<Option<String>, RuntimeError> {
        let room = self.joined_room(room)?;
        let user = self.client.user_id().ok_or(RuntimeError::Transport)?;
        let member = room.get_member_no_sync(user).await.map_err(|_| RuntimeError::Transport)?;
        Ok(member
            .filter(|m| *m.membership() == MembershipState::Join)
            .and_then(|m| m.event().event_id().map(|id| id.to_string())))
    }
    async fn display_name(&self, room: &str, user: &str) -> Option<String> {
        let room = self.joined_room(room).ok()?;
        let user = OwnedUserId::try_from(user).ok()?;
        // A missing name only drops the label; the Matrix ID is always shown.
        let member = room.get_member_no_sync(&user).await.ok()??;
        member.display_name().map(str::to_owned)
    }
    async fn start(&self, room: &str, anchor: &str) -> Result<Option<String>, RuntimeError> {
        self.history_start(room, anchor).await
    }
    async fn page(&self, room: &str, from: &str, join_event: &str) -> Result<(HistoryBatch, bool), RuntimeError> {
        let (chunk, next) = self.timeline_page(room, from).await?;
        let entries = chunk.iter().map(|e| (e.kind.parse_event_id().map(|id| id.to_string()), history_event(e)));
        let (events, reached_join) = cut_at_join(entries, join_event);
        Ok((HistoryBatch { events, next }, reached_join))
    }
}

impl AgentDevice {
    fn joined_room(&self, room: &str) -> Result<matrix_sdk::Room, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        self.client.get_room(&room_id).ok_or(RuntimeError::Transport)
    }
}
