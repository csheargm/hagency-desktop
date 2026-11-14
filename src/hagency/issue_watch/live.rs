//! The live hand-off: the composer's message into the chosen room, then the
//! owner API's `issueSeen` record in this device's ledger.
use super::handoff::Handoff;
use crate::hagency::backend::Service;
use crate::hagency::model::{Command, LocalAction};
use ::hagency::issue_watch::IssueWatch;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, OwnedUserId};
use serde_json::{Value, json};

pub(super) struct Live {
    service: Service,
    agent_id: String,
    repo: String,
    room_id: OwnedRoomId,
    /// The Agent's Matrix user, mentioned like the composer does.
    puppet: OwnedUserId,
    label: String,
    /// The account session this check started in; nothing is sent after it ends.
    epoch: u64,
}

impl Live {
    /// The sender for one Agent record of the Agents list and its saved watch.
    pub(super) fn new(service: &Service, agent: &Value, watch: &IssueWatch, epoch: u64) -> Result<Self, String> {
        let room = &watch.setting.room_id;
        let puppet = agent["puppetMxid"].as_str().unwrap_or_default();
        Ok(Self {
            service: service.clone(),
            agent_id: agent["id"].as_str().unwrap_or_default().to_owned(),
            repo: watch.setting.repo.clone(),
            room_id: room.as_str().try_into().map_err(|_| format!("Invalid room {room}"))?,
            puppet: puppet.try_into().map_err(|_| format!("Unreadable Agent Matrix ID {puppet}"))?,
            label: agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent").to_owned(),
            epoch,
        })
    }
}

impl Handoff for Live {
    async fn post(&self, number: i64, text: &str) -> Result<(), String> {
        let message = crate::hagency::room_send::message_content(text, &self.puppet, &self.label);
        super::still_current(self.epoch)?;
        let txn_id = handoff_txn_id(&self.agent_id, &self.repo, number);
        crate::hagency::room_send::send_routed_with(self.room_id.clone(), message, txn_id).await
    }
    async fn mark_seen(&self, number: i64) -> Result<(), String> {
        super::still_current(self.epoch)?;
        let input = json!({"issueSeen": {"repo": self.repo, "number": number}});
        saved(&self.service, &self.agent_id, Some(input)).await.map(|_| ())
    }
}

/// The owner API's issue-watch record (`issueWatch`, `issueSeen`); a change
/// is saved first when given.
pub(crate) async fn saved(service: &Service, agent_id: &str, change: Option<Value>) -> Result<Value, String> {
    let action = if change.is_some() { LocalAction::SaveIssueWatch } else { LocalAction::IssueWatch };
    service.execute(Command::Local { agent_id: agent_id.into(), action, input: change, binding_id: None, requester: None }).await
}

/// The same id for the same Agent, repository and issue on every retry, so a
/// post whose `issueSeen` save failed is deduplicated by the homeserver
/// (Matrix transaction ids are scoped to this device's access token).
pub(super) fn handoff_txn_id(agent_id: &str, repo: &str, number: i64) -> OwnedTransactionId {
    let key = blake3::hash(format!("{agent_id}\n{repo}\n{number}").as_bytes());
    OwnedTransactionId::from(format!("hagency-issue-{}", key.to_hex()))
}
