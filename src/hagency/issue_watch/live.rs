//! The live hand-off: the composer's message into the chosen room, then the
//! owner API's `issueSeen` record in this device's ledger.
use super::handoff::Handoff;
use crate::hagency::backend::Service;
use crate::hagency::model::{Command, LocalAction};
use ::hagency::issue_watch::{Issue, IssueWatch};
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
    /// The Agent's auto-merge switch (`Notice::with_merge_policy`).
    auto_merge: bool,
    /// The account session this check started in; nothing is sent after it ends.
    epoch: u64,
    /// Which hand-off of the issue this is (1: the first; the tracker retries).
    attempt: u32,
    /// A branch the last run pushed without a pull request (a retry asks for one).
    branch: Option<String>,
    /// > 0: the same attempt handed off again after an interrupted run.
    again: u32,
}

impl Live {
    /// The sender for one Agent record of the Agents list and its saved watch.
    pub(super) fn new(service: &Service, agent: &Value, watch: &IssueWatch, auto_merge: bool, epoch: u64) -> Result<Self, String> {
        let room = &watch.setting.room_id;
        let puppet = agent["puppetMxid"].as_str().unwrap_or_default();
        Ok(Self {
            service: service.clone(),
            agent_id: agent["id"].as_str().unwrap_or_default().to_owned(),
            repo: watch.setting.repo.clone(),
            room_id: room.as_str().try_into().map_err(|_| format!("Invalid room {room}"))?,
            puppet: puppet.try_into().map_err(|_| format!("Unreadable Agent Matrix ID {puppet}"))?,
            label: agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent").to_owned(),
            auto_merge,
            epoch,
            attempt: 1,
            branch: None,
            again: 0,
        })
    }
    /// The same sender for hand-off `attempt` (its own transaction id).
    pub(super) fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }
    /// The same sender re-posting its attempt after an interrupted run (`again` > 0).
    pub(super) fn with_again(mut self, again: u32) -> Self {
        self.again = again;
        self
    }
    /// The same sender asking for the pull request from a pushed `branch`.
    pub(super) fn with_branch(mut self, branch: Option<&str>) -> Self {
        self.branch = branch.map(str::to_owned);
        self
    }
}

impl Handoff for Live {
    async fn post(&self, issue: &Issue) -> Result<(), String> {
        let notice = super::notice::Notice::new(&self.repo, issue, &self.label).with_merge_policy(self.auto_merge).with_branch(self.branch.clone());
        super::still_current(self.epoch)?;
        let txn_id = handoff_txn_id(&self.agent_id, &self.repo, issue.number, self.attempt, self.again);
        crate::hagency::room_send::send_routed_with(self.room_id.clone(), notice.content(&self.puppet), notice.marker(), txn_id).await
    }
    async fn mark_seen(&self, number: i64) -> Result<(), String> {
        super::still_current(self.epoch)?;
        // One transaction: the issue is seen and its tracked attempt counted (ADR 0020).
        let track = if self.again > 0 { json!({"repo": self.repo, "number": number, "rehanded": true}) } else { json!({"repo": self.repo, "number": number, "handed": self.attempt}) };
        let input = json!({"issueSeen": {"repo": self.repo, "number": number}, "track": track});
        saved(&self.service, &self.agent_id, Some(input)).await.map(|_| ())
    }
    fn paused(&self) -> bool {
        crate::hagency::kill_switch::paused_here(&self.agent_id)
    }
}

/// The owner API's issue-watch record (`issueWatch`, `issueSeen`); a change
/// is saved first when given.
pub(crate) async fn saved(service: &Service, agent_id: &str, change: Option<Value>) -> Result<Value, String> {
    let action = if change.is_some() { LocalAction::SaveIssueWatch } else { LocalAction::IssueWatch };
    service.execute(Command::Local { agent_id: agent_id.into(), action, input: change, binding_id: None, requester: None }).await
}

/// The Agent's auto-merge switch, from the same local status the agent panel
/// reads (`approvals.autoMerge`).
pub(super) async fn auto_merge(agent_id: &str) -> bool {
    let account = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    // An unreadable status only leaves the merge-policy sentence out of the
    // hand-off (the draft-PR request is unchanged and the merge stays gated),
    // so it must not block the issue hand-off itself.
    crate::host::hagency::local::Call::Status(agent_id.into()).execute(&account).await
        .is_ok_and(|status| status["approvals"]["autoMerge"] == true)
}

/// The same id for the same Agent, repository and issue on every retry, so a
/// post whose `issueSeen` save failed is deduplicated by the homeserver
/// (Matrix transaction ids are scoped to this device's access token).
/// A retry (`attempt` > 1) or a re-post after an interrupted run (`again`
/// > 0) gets its own id, or the homeserver would drop it.
pub(super) fn handoff_txn_id(agent_id: &str, repo: &str, number: i64, attempt: u32, again: u32) -> OwnedTransactionId {
    let base = format!("{agent_id}\n{repo}\n{number}");
    let base = if attempt <= 1 { base } else { format!("{base}\n{attempt}") };
    let key = blake3::hash(if again == 0 { base } else { format!("{base}\nagain {again}") }.as_bytes());
    OwnedTransactionId::from(format!("hagency-issue-{}", key.to_hex()))
}
