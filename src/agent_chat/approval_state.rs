// Adapted from robrix2 da375f241d; protocol and adversarial tests retained.
//! Pure, account-scoped state for agent-chat approval projections.

use std::collections::{BTreeSet, HashMap, HashSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const KEY: &str = "com.agentchat.approval";
const REQUEST_TYPE: &str = "com.agentchat.approval.request.v1";
const STATUS_TYPE: &str = "com.agentchat.approval.status.v1";
const MARKER_TYPE: &str = "com.agentchat.approval.room.v2";
pub(crate) const MARKER_FRESHNESS_MS: u64 = 300_000;
const MAX_ACCOUNTS: usize = 16;
const MAX_APPROVALS: usize = 1024;
const MAX_MARKERS: usize = 256;
const MAX_ASSOCIATIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CanonicalApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Consumed,
}

impl CanonicalApprovalState {
    fn terminal(self) -> bool {
        self != Self::Pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalDecision {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApprovalAction {
    ApproveOnce,
    ApproveTask,
    ApproveAlways,
    Deny,
}

impl ApprovalAction {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::ApproveOnce => "approve_once",
            Self::ApproveTask => "approve_task",
            Self::ApproveAlways => "approve_always",
            Self::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ApprovalRequestKey {
    pub(crate) account_mxid: String,
    pub(crate) approval_room_id: String,
    pub(crate) request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApprovalClaimKey {
    pub(crate) account_mxid: String,
    pub(crate) approval_room_id: String,
    pub(crate) request_id: String,
    pub(crate) input_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ApprovalBinding {
    pub(crate) request_id: String,
    pub(crate) agent: String,
    pub(crate) project: String,
    pub(crate) project_room_id: String,
    pub(crate) input_digest: String,
    pub(crate) owner_mxid: String,
    pub(crate) publisher_mxid: String,
    pub(crate) approval_room_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalView {
    pub(crate) binding: ApprovalBinding,
    pub(crate) state: CanonicalApprovalState,
    pub(crate) decision: Option<ApprovalDecision>,
    pub(crate) latest_revision: u64,
    pub(crate) expires_at: Option<u64>,
    pub(crate) source_event_ids: Vec<String>,
    pub(crate) conflicted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangedApproval {
    pub(crate) key: ApprovalRequestKey,
    pub(crate) room_epoch: u64,
    pub(crate) source_event_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ApprovalModelError {
    InvalidContext,
    InvalidContent,
    BindingMismatch,
    RevisionConflict,
    InvalidTransition,
    CapacityExceeded,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IngestResult {
    Changed(ChangedApproval),
    Duplicate,
    Rejected(ApprovalModelError),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaimGrant {
    pub(crate) transaction_id: String,
    pub(crate) action: ApprovalAction,
    pub(crate) token: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendResult {
    ConfirmedPreSendFailure,
    OutcomeUnknown,
    Sent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimDisposition {
    Released,
    Retained,
    Missing,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaimError {
    NotActionable,
    AlreadyClaimed,
    BindingMismatch,
    CapacityExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimState {
    Claimed,
    OutcomeUnknown,
    Sent,
}
#[derive(Clone, Debug)]
struct Claim {
    state: ClaimState,
    action: ApprovalAction,
    token: u64,
}
#[derive(Clone, Debug)]
struct Record {
    binding: ApprovalBinding,
    state: CanonicalApprovalState,
    decision: Option<ApprovalDecision>,
    revision: u64,
    expires_at: Option<u64>,
    sources: BTreeSet<String>,
    has_request: bool,
    actions: Vec<ApprovalAction>,
    scope: Option<super::approval::ReusableScope>,
    conflicted: bool,
}
impl Record {
    fn view(&self) -> ApprovalView {
        ApprovalView {
            binding: self.binding.clone(),
            state: self.state,
            decision: self.decision,
            latest_revision: self.revision,
            expires_at: self.expires_at,
            source_event_ids: self.sources.iter().cloned().collect(),
            conflicted: self.conflicted,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ApprovalSession {
    records: HashMap<ApprovalRequestKey, Record>,
    claims: HashMap<ApprovalClaimKey, Claim>,
    epochs: HashMap<(String, String), u64>,
    next_claim: u64,
}

#[derive(Clone, Debug)]
struct Parsed {
    binding: ApprovalBinding,
    state: CanonicalApprovalState,
    decision: Option<ApprovalDecision>,
    revision: u64,
    expires_at: Option<u64>,
    request: bool,
    actions: Vec<ApprovalAction>,
    scope: Option<super::approval::ReusableScope>,
}

impl ApprovalSession {
    pub(crate) fn ingest_message(
        &mut self,
        account: &str,
        room: &str,
        event: &str,
        sender: &str,
        content: &Value,
        _now: u64,
    ) -> IngestResult {
        if !mxid(account) || !room_id(room) || !event_id(event) || !mxid(sender) {
            return IngestResult::Rejected(ApprovalModelError::InvalidContext);
        }
        let parsed = match parse_projection(room, sender, content) {
            Ok(v) => v,
            Err(e) => return IngestResult::Rejected(e),
        };
        let key = ApprovalRequestKey {
            account_mxid: account.into(),
            approval_room_id: room.into(),
            request_id: parsed.binding.request_id.clone(),
        };
        if !self.records.contains_key(&key) {
            if (self.account_count() >= MAX_ACCOUNTS && !self.has_account(account))
                || self
                    .records
                    .keys()
                    .filter(|k| k.account_mxid == account)
                    .count()
                    >= MAX_APPROVALS
            {
                return IngestResult::Rejected(ApprovalModelError::CapacityExceeded);
            }
            let mut sources = BTreeSet::new();
            if parsed.request {
                sources.insert(event.into());
            }
            self.records.insert(
                key.clone(),
                Record {
                    binding: parsed.binding,
                    state: parsed.state,
                    decision: parsed.decision,
                    revision: parsed.revision,
                    expires_at: parsed.expires_at,
                    sources,
                    has_request: parsed.request,
                    actions: parsed.actions,
                    scope: parsed.scope,
                    conflicted: false,
                },
            );
            return IngestResult::Changed(self.changed(&key));
        }
        let binding_mismatch = self
            .records
            .get(&key)
            .is_some_and(|r| r.binding != parsed.binding);
        if binding_mismatch {
            if let Some(record) = self.records.get_mut(&key) {
                record.conflicted = true;
            }
            let _ = self.changed(&key);
            return IngestResult::Rejected(ApprovalModelError::BindingMismatch);
        }
        if parsed.request {
            let Some(record) = self.records.get_mut(&key) else {
                return IngestResult::Rejected(ApprovalModelError::InvalidTransition);
            };
            if record.has_request {
                if record.expires_at == parsed.expires_at
                    && record.actions == parsed.actions
                    && record.scope == parsed.scope
                {
                    return IngestResult::Duplicate;
                }
                record.conflicted = true;
                let _ = self.changed(&key);
                return IngestResult::Rejected(ApprovalModelError::RevisionConflict);
            }
            record.has_request = true;
            record.actions = parsed.actions;
            record.scope = parsed.scope;
            record.expires_at = parsed.expires_at;
            record.sources.insert(event.into());
            return IngestResult::Changed(self.changed(&key));
        }
        let Some(record) = self.records.get(&key) else {
            return IngestResult::Rejected(ApprovalModelError::InvalidTransition);
        };
        let (old_rev, old_state, old_decision) = (record.revision, record.state, record.decision);
        if parsed.revision < old_rev {
            return IngestResult::Duplicate;
        }
        if parsed.revision == old_rev {
            if old_state == parsed.state && old_decision == parsed.decision {
                return IngestResult::Duplicate;
            }
            if let Some(record) = self.records.get_mut(&key) {
                record.conflicted = true;
            }
            let _ = self.changed(&key);
            return IngestResult::Rejected(ApprovalModelError::RevisionConflict);
        }
        if !transition(old_state, old_decision, parsed.state, parsed.decision) {
            if let Some(record) = self.records.get_mut(&key) {
                record.conflicted = true;
            }
            let _ = self.changed(&key);
            return IngestResult::Rejected(ApprovalModelError::InvalidTransition);
        }
        if let Some(record) = self.records.get_mut(&key) {
            record.revision = parsed.revision;
            record.state = parsed.state;
            record.decision = parsed.decision;
        }
        if parsed.state.terminal() {
            self.claims.retain(|c, _| {
                !(c.account_mxid == account
                    && c.approval_room_id == room
                    && c.request_id == key.request_id)
            });
        }
        IngestResult::Changed(self.changed(&key))
    }

    pub(crate) fn approval(
        &self,
        account: &str,
        room: &str,
        request: &str,
    ) -> Option<ApprovalView> {
        self.records
            .get(&ApprovalRequestKey {
                account_mxid: account.into(),
                approval_room_id: room.into(),
                request_id: request.into(),
            })
            .map(Record::view)
    }
    pub(crate) fn room_epoch(&self, account: &str, room: &str) -> u64 {
        self.epochs
            .get(&(account.into(), room.into()))
            .copied()
            .unwrap_or(0)
    }
    pub(crate) fn is_actionable(&self, account: &str, room: &str, request: &str, now: u64) -> bool {
        let key = ApprovalRequestKey {
            account_mxid: account.into(),
            approval_room_id: room.into(),
            request_id: request.into(),
        };
        let Some(r) = self.records.get(&key) else {
            return false;
        };
        r.has_request
            && !r.conflicted
            && r.state == CanonicalApprovalState::Pending
            && r.binding.owner_mxid == account
            && r.expires_at.is_some_and(|e| now < e)
            && !self.claims.keys().any(|c| {
                c.account_mxid == account
                    && c.approval_room_id == room
                    && c.request_id == request
                    && c.input_digest == r.binding.input_digest
            })
    }
    pub(crate) fn try_claim(
        &mut self,
        key: ApprovalClaimKey,
        action: ApprovalAction,
        now: u64,
    ) -> Result<ClaimGrant, ClaimError> {
        let rk = ApprovalRequestKey {
            account_mxid: key.account_mxid.clone(),
            approval_room_id: key.approval_room_id.clone(),
            request_id: key.request_id.clone(),
        };
        let Some(r) = self.records.get(&rk) else {
            return Err(ClaimError::NotActionable);
        };
        if r.binding.input_digest != key.input_digest || !r.actions.contains(&action) {
            return Err(ClaimError::BindingMismatch);
        }
        if !self.is_actionable(
            &key.account_mxid,
            &key.approval_room_id,
            &key.request_id,
            now,
        ) {
            return Err(if self.claims.contains_key(&key) {
                ClaimError::AlreadyClaimed
            } else {
                ClaimError::NotActionable
            });
        }
        if self.claims.len() >= MAX_ACCOUNTS * MAX_APPROVALS {
            return Err(ClaimError::CapacityExceeded);
        }
        let transaction_id = stable_txn(&key.request_id, action, &key.input_digest)
            .ok_or(ClaimError::BindingMismatch)?;
        self.next_claim = self
            .next_claim
            .checked_add(1)
            .ok_or(ClaimError::CapacityExceeded)?;
        let token = self.next_claim;
        self.claims.insert(
            key.clone(),
            Claim {
                state: ClaimState::Claimed,
                action,
                token,
            },
        );
        let _ = self.bump_room_epoch(&key.account_mxid, &key.approval_room_id);
        Ok(ClaimGrant {
            transaction_id,
            action,
            token,
        })
    }
    pub(crate) fn claim_is_current(
        &self,
        key: &ApprovalClaimKey,
        token: u64,
        action: ApprovalAction,
        source_event_id: &str,
        now: u64,
    ) -> bool {
        let Some(claim) = self.claims.get(key) else {
            return false;
        };
        if claim.token != token || claim.action != action || claim.state != ClaimState::Claimed {
            return false;
        }
        self.records
            .get(&ApprovalRequestKey {
                account_mxid: key.account_mxid.clone(),
                approval_room_id: key.approval_room_id.clone(),
                request_id: key.request_id.clone(),
            })
            .is_some_and(|record| {
                record.has_request
                    && !record.conflicted
                    && record.state == CanonicalApprovalState::Pending
                    && record.binding.owner_mxid == key.account_mxid
                    && record.binding.input_digest == key.input_digest
                    && record.sources.contains(source_event_id)
                    && record.actions.contains(&action)
                    && record.expires_at.is_some_and(|deadline| now < deadline)
            })
    }

    pub(crate) fn record_send_result(
        &mut self,
        key: &ApprovalClaimKey,
        token: u64,
        result: SendResult,
        now: u64,
    ) -> ClaimDisposition {
        let Some(claim) = self.claims.get(key) else {
            return ClaimDisposition::Missing;
        };
        if claim.token != token {
            return ClaimDisposition::Missing;
        }
        match result {
            SendResult::ConfirmedPreSendFailure => {
                let rk = ApprovalRequestKey {
                    account_mxid: key.account_mxid.clone(),
                    approval_room_id: key.approval_room_id.clone(),
                    request_id: key.request_id.clone(),
                };
                let release = claim.state == ClaimState::Claimed
                    && self.records.get(&rk).is_some_and(|r| {
                        r.state == CanonicalApprovalState::Pending
                            && !r.conflicted
                            && r.expires_at.is_some_and(|e| now < e)
                    });
                if release {
                    self.claims.remove(key);
                    let _ = self.bump_room_epoch(&key.account_mxid, &key.approval_room_id);
                    ClaimDisposition::Released
                } else {
                    ClaimDisposition::Retained
                }
            }
            SendResult::OutcomeUnknown => {
                if let Some(claim) = self.claims.get_mut(key) {
                    claim.state = ClaimState::OutcomeUnknown;
                }
                let _ = self.bump_room_epoch(&key.account_mxid, &key.approval_room_id);
                ClaimDisposition::Retained
            }
            SendResult::Sent => {
                if let Some(claim) = self.claims.get_mut(key) {
                    claim.state = ClaimState::Sent;
                }
                let _ = self.bump_room_epoch(&key.account_mxid, &key.approval_room_id);
                ClaimDisposition::Retained
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn claim_state(&self, key: &ApprovalClaimKey) -> Option<ClaimState> {
        self.claims.get(key).map(|c| c.state)
    }
    #[cfg(test)]
    pub(crate) fn remove_room(&mut self, account: &str, room: &str) {
        self.records
            .retain(|k, _| k.account_mxid != account || k.approval_room_id != room);
        self.claims
            .retain(|k, _| k.account_mxid != account || k.approval_room_id != room);
        self.epochs.remove(&(account.into(), room.into()));
    }
    #[cfg(test)]
    pub(crate) fn clear_account(&mut self, account: &str) {
        self.records.retain(|k, _| k.account_mxid != account);
        self.claims.retain(|k, _| k.account_mxid != account);
        self.epochs.retain(|(a, _), _| a != account);
    }
    fn changed(&mut self, key: &ApprovalRequestKey) -> ChangedApproval {
        let epoch = self
            .epochs
            .entry((key.account_mxid.clone(), key.approval_room_id.clone()))
            .or_default();
        *epoch = epoch.saturating_add(1);
        ChangedApproval {
            key: key.clone(),
            room_epoch: *epoch,
            source_event_ids: self
                .records
                .get(key)
                .map(|r| r.sources.iter().cloned().collect())
                .unwrap_or_default(),
        }
    }
    fn bump_room_epoch(&mut self, account: &str, room: &str) -> u64 {
        let epoch = self
            .epochs
            .entry((account.into(), room.into()))
            .or_default();
        *epoch = epoch.saturating_add(1);
        *epoch
    }
    fn has_account(&self, a: &str) -> bool {
        self.records.keys().any(|k| k.account_mxid == a)
    }
    fn account_count(&self) -> usize {
        self.records
            .keys()
            .map(|k| &k.account_mxid)
            .collect::<HashSet<_>>()
            .len()
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ApprovalMarkerIndex {
    version: u8,
    markers: Vec<Marker>,
    #[serde(skip)]
    validated: HashSet<(String, String, u64)>,
}

impl Default for ApprovalMarkerIndex {
    fn default() -> Self {
        Self {
            version: 3,
            markers: Vec::new(),
            validated: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Association {
    agent: String,
    project_room_id: String,
    active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Marker {
    account: String,
    room: String,
    event: String,
    publisher: String,
    owner: String,
    generation: u64,
    associations: Vec<Association>,
    validated_at: u64,
    conflicted: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApprovalMarkerIndexWire {
    version: u8,
    markers: Vec<Marker>,
}

impl<'de> Deserialize<'de> for ApprovalMarkerIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ApprovalMarkerIndexWire::deserialize(deserializer)?;
        if wire.version != 3 {
            return Ok(Self::default());
        }
        if wire.markers.len() > MAX_ACCOUNTS.saturating_mul(MAX_MARKERS) {
            return Ok(Self::default());
        }
        Ok(Self {
            version: 3,
            markers: wire.markers,
            validated: HashSet::new(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MarkerResolution {
    Unique(String),
    Missing,
    Ambiguous,
    NotJoined,
    Stale,
}

impl ApprovalMarkerIndex {
    /// A refresh must authenticate every candidate again while retaining
    /// binding-generation evidence from earlier observations.
    pub(crate) fn begin_revalidation(&mut self) {
        self.validated.clear();
    }

    pub(crate) fn retain_joined(&mut self, account: &str, joined: &HashSet<String>) {
        let removed: Vec<_> = self
            .markers
            .iter()
            .filter(|marker| marker.account == account && !joined.contains(&marker.room))
            .map(|marker| marker.room.clone())
            .collect();
        for room in removed {
            self.remove_room(account, &room);
        }
    }

    pub(crate) fn ingest_marker(
        &mut self,
        account: &str,
        room: &str,
        event: &str,
        event_type: &str,
        sender: &str,
        state_key: &str,
        content: &Value,
        now: u64,
    ) -> Result<(), ApprovalModelError> {
        if !state_key.is_empty()
            || !mxid(account)
            || !room_id(room)
            || !event_id(event)
            || !mxid(sender)
        {
            return Err(ApprovalModelError::InvalidContext);
        }
        if content.as_object().is_none_or(|fields| fields.len() != 5) {
            return Err(ApprovalModelError::InvalidContent);
        }
        let detail = content;
        let protocol = detail
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|version| u8::try_from(version).ok())
            .ok_or(ApprovalModelError::InvalidContent)?;
        if event_type != MARKER_TYPE || protocol != 2 {
            return Err(ApprovalModelError::InvalidContent);
        }
        self.mark_room_unavailable(account, room);
        let publisher = req(detail, "publisher_mxid")?;
        let owner = req(detail, "owner_mxid")?;
        let generation = detail
            .get("binding_generation")
            .and_then(Value::as_u64)
            .filter(|generation| *generation > 0)
            .ok_or(ApprovalModelError::InvalidContent)?;
        if !mxid(&publisher) || !mxid(&owner) || publisher != sender || owner != account {
            return Err(ApprovalModelError::InvalidContext);
        }
        let items = detail
            .get("project_room_associations")
            .and_then(Value::as_array)
            .ok_or(ApprovalModelError::InvalidContent)?;
        if items.len() > MAX_ASSOCIATIONS {
            return Err(ApprovalModelError::CapacityExceeded);
        }
        if detail.get("agent").is_some() {
            return Err(ApprovalModelError::InvalidContent);
        }
        let mut seen = HashSet::new();
        let mut associations = Vec::with_capacity(items.len());
        for item in items {
            if item.as_object().is_none_or(|fields| fields.len() != 3) {
                return Err(ApprovalModelError::InvalidContent);
            }
            let agent = req(item, "agent")?;
            let project_room_id = req(item, "project_room_id")?;
            let active = item
                .get("active")
                .and_then(Value::as_bool)
                .ok_or(ApprovalModelError::InvalidContent)?;
            if agent.len() > 128
                || !room_id(&project_room_id)
                || !seen.insert((agent.clone(), project_room_id.clone()))
            {
                return Err(ApprovalModelError::InvalidContent);
            }
            associations.push(Association {
                agent,
                project_room_id,
                active,
            });
        }
        associations.sort_by(|left, right| {
            (&left.agent, &left.project_room_id).cmp(&(&right.agent, &right.project_room_id))
        });
        let candidate = Marker {
            account: account.into(),
            room: room.into(),
            event: event.into(),
            publisher,
            owner,
            generation,
            associations,
            validated_at: now,
            conflicted: false,
        };
        let validation = (account.into(), room.into(), generation);
        if let Some(old) = self
            .markers
            .iter_mut()
            .find(|marker| marker.account == account && marker.room == room)
        {
            if generation < old.generation {
                self.validated
                    .remove(&(account.into(), room.into(), old.generation));
                return Err(ApprovalModelError::RevisionConflict);
            }
            if generation == old.generation {
                let same = old.publisher == candidate.publisher
                    && old.owner == candidate.owner
                    && old.associations == candidate.associations;
                if same {
                    old.validated_at = now;
                    old.event = event.into();
                    self.validated.insert(validation);
                    return Ok(());
                }
                old.conflicted = true;
                self.validated
                    .remove(&(account.into(), room.into(), generation));
                return Err(ApprovalModelError::RevisionConflict);
            }
            self.validated.retain(|(found_account, found_room, _)| {
                found_account != account || found_room != room
            });
            *old = candidate;
            self.validated.insert(validation);
            return Ok(());
        }
        let accounts = self
            .markers
            .iter()
            .map(|marker| &marker.account)
            .collect::<HashSet<_>>()
            .len();
        if self
            .markers
            .iter()
            .filter(|marker| marker.account == account)
            .count()
            >= MAX_MARKERS
            || (accounts >= MAX_ACCOUNTS
                && !self.markers.iter().any(|marker| marker.account == account))
        {
            return Err(ApprovalModelError::CapacityExceeded);
        }
        self.markers.push(candidate);
        self.validated.insert(validation);
        Ok(())
    }

    pub(crate) fn candidates(
        &self,
        account: &str,
        project: Option<&str>,
        joined: &HashSet<String>,
        now: u64,
    ) -> Vec<(String, String, String)> {
        let mut candidates = BTreeSet::new();
        for marker in &self.markers {
            for association in &marker.associations {
                if association.active && project.is_none_or(|p| p == association.project_room_id) {
                    if let MarkerResolution::Unique(room) = self.resolve(
                        account,
                        &association.agent,
                        &association.project_room_id,
                        joined,
                        now,
                    ) {
                        candidates.insert((
                            room,
                            association.agent.clone(),
                            association.project_room_id.clone(),
                        ));
                    }
                }
            }
        }
        candidates.into_iter().collect()
    }

    pub(crate) fn resolve(
        &self,
        account: &str,
        agent: &str,
        project: &str,
        joined: &HashSet<String>,
        now: u64,
    ) -> MarkerResolution {
        let candidates: Vec<_> = self
            .markers
            .iter()
            .filter(|marker| {
                marker.account == account
                    && marker.owner == account
                    && marker.associations.iter().any(|association| {
                        association.active
                            && association.agent == agent
                            && association.project_room_id == project
                    })
            })
            .collect();
        if candidates.is_empty() {
            return MarkerResolution::Missing;
        }
        if candidates.iter().any(|marker| marker.conflicted) {
            return MarkerResolution::Ambiguous;
        }
        if candidates.iter().any(|marker| {
            !self.validated.contains(&(
                marker.account.clone(),
                marker.room.clone(),
                marker.generation,
            )) || marker.validated_at == 0
                || now.saturating_sub(marker.validated_at) > MARKER_FRESHNESS_MS
        }) {
            return MarkerResolution::Stale;
        }
        if candidates.len() != 1 {
            return MarkerResolution::Ambiguous;
        }
        if !joined.contains(&candidates[0].room) {
            MarkerResolution::NotJoined
        } else {
            MarkerResolution::Unique(candidates[0].room.clone())
        }
    }

    pub(crate) fn mark_room_unavailable(&mut self, account: &str, room: &str) {
        self.validated.retain(|(found_account, found_room, _)| {
            found_account != account || found_room != room
        });
    }

    pub(crate) fn remove_room(&mut self, account: &str, room: &str) {
        self.markers
            .retain(|marker| marker.account != account || marker.room != room);
        self.mark_room_unavailable(account, room);
    }

    #[cfg(test)]
    pub(crate) fn clear_account(&mut self, account: &str) {
        self.markers.retain(|marker| marker.account != account);
        self.validated
            .retain(|(found_account, _, _)| found_account != account);
    }
}

fn parse_projection(room: &str, sender: &str, c: &Value) -> Result<Parsed, ApprovalModelError> {
    if c.get(KEY).and_then(|v| v.get("migration_kind")).is_some() {
        return Err(ApprovalModelError::InvalidContent);
    }
    let typ = c
        .get("msgtype")
        .and_then(Value::as_str)
        .ok_or(ApprovalModelError::InvalidContent)?;
    let d = c.get(KEY).ok_or(ApprovalModelError::InvalidContent)?;
    if d.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(ApprovalModelError::InvalidContent);
    }
    let kind = if typ == REQUEST_TYPE {
        "request"
    } else if typ == STATUS_TYPE {
        "status"
    } else {
        return Err(ApprovalModelError::InvalidContent);
    };
    if d.get("kind").and_then(Value::as_str) != Some(kind) {
        return Err(ApprovalModelError::InvalidContent);
    }
    let request_id = req(d, "request_id")?;
    let agent = req(d, "agent")?;
    let project = req(d, "project")?;
    let project_room_id = req(d, "project_room_id")?;
    let input_digest = req(d, "input_digest")?;
    let owner_mxid = req(d, "owner_mxid")?;
    let publisher_mxid = req(d, "publisher_mxid")?;
    let revision = d
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or(ApprovalModelError::InvalidContent)?;
    if !request_id
        .strip_prefix("approval_")
        .is_some_and(|s| hex(s, 40))
        || !room_id(&project_room_id)
        || !hex(&input_digest, 64)
        || !mxid(&owner_mxid)
        || !mxid(&publisher_mxid)
        || publisher_mxid != sender
    {
        return Err(ApprovalModelError::InvalidContent);
    }
    let binding = ApprovalBinding {
        request_id,
        agent,
        project,
        project_room_id,
        input_digest,
        owner_mxid,
        publisher_mxid,
        approval_room_id: room.into(),
    };
    if typ == REQUEST_TYPE {
        let expires_at = d
            .get("expires_at")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
            .ok_or(ApprovalModelError::InvalidContent)?;
        let request =
            super::approval::parse_approval_request(c).ok_or(ApprovalModelError::InvalidContent)?;
        let actions = request
            .actions
            .iter()
            .map(|action| match action.id.as_str() {
                "approve_once" => Ok(ApprovalAction::ApproveOnce),
                "approve_task" => Ok(ApprovalAction::ApproveTask),
                "approve_always" => Ok(ApprovalAction::ApproveAlways),
                "deny" => Ok(ApprovalAction::Deny),
                _ => Err(ApprovalModelError::InvalidContent),
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Parsed {
            binding,
            state: CanonicalApprovalState::Pending,
            decision: None,
            revision,
            expires_at: Some(expires_at),
            request: true,
            actions,
            scope: request.reusable_scope,
        });
    }
    if d.get("actions").is_some() {
        return Err(ApprovalModelError::InvalidContent);
    }
    let state = match d.get("state").and_then(Value::as_str) {
        Some("pending") => CanonicalApprovalState::Pending,
        Some("approved") => CanonicalApprovalState::Approved,
        Some("denied") => CanonicalApprovalState::Denied,
        Some("expired") => CanonicalApprovalState::Expired,
        Some("consumed") => CanonicalApprovalState::Consumed,
        _ => return Err(ApprovalModelError::InvalidContent),
    };
    let decision = match d.get("decision") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if v == "allow" => Some(ApprovalDecision::Allow),
        Some(Value::String(v)) if v == "deny" => Some(ApprovalDecision::Deny),
        _ => return Err(ApprovalModelError::InvalidContent),
    };
    if !state_decision(state, decision) {
        return Err(ApprovalModelError::InvalidContent);
    }
    Ok(Parsed {
        binding,
        state,
        decision,
        revision,
        expires_at: None,
        request: false,
        actions: Vec::new(),
        scope: None,
    })
}
fn transition(
    a: CanonicalApprovalState,
    ad: Option<ApprovalDecision>,
    b: CanonicalApprovalState,
    bd: Option<ApprovalDecision>,
) -> bool {
    matches!(
        (a, b),
        (
            CanonicalApprovalState::Pending,
            CanonicalApprovalState::Approved
                | CanonicalApprovalState::Denied
                | CanonicalApprovalState::Expired
                | CanonicalApprovalState::Consumed
        )
    ) || matches!(
        (a, b),
        (
            CanonicalApprovalState::Approved
                | CanonicalApprovalState::Denied
                | CanonicalApprovalState::Expired,
            CanonicalApprovalState::Consumed
        )
    ) && ad == bd
}
fn state_decision(s: CanonicalApprovalState, d: Option<ApprovalDecision>) -> bool {
    match s {
        CanonicalApprovalState::Pending => d.is_none(),
        CanonicalApprovalState::Approved => d == Some(ApprovalDecision::Allow),
        CanonicalApprovalState::Denied | CanonicalApprovalState::Expired => {
            d == Some(ApprovalDecision::Deny)
        }
        CanonicalApprovalState::Consumed => d.is_some(),
    }
}
fn stable_txn(request: &str, action: ApprovalAction, digest: &str) -> Option<String> {
    let suffix = request.strip_prefix("approval_")?;
    if !hex(suffix, 40) || !hex(digest, 64) {
        return None;
    }
    let s = format!(
        "robrix.approval.{suffix}.{}.{}",
        action.name(),
        &digest[..16]
    );
    (s.len() <= 128).then_some(s)
}
fn req(v: &Value, k: &str) -> Result<String, ApprovalModelError> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.trim() == *s && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or(ApprovalModelError::InvalidContent)
}
fn mxid(s: &str) -> bool {
    sigil(s, '@')
}
fn room_id(s: &str) -> bool {
    sigil(s, '!')
}
fn event_id(s: &str) -> bool {
    s.starts_with('$') && s.len() > 1 && s.len() <= 255 && !s.chars().any(char::is_whitespace)
}
fn sigil(s: &str, c: char) -> bool {
    s.starts_with(c)
        && s.len() <= 255
        && s[1..]
            .split_once(':')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && !s.chars().any(char::is_whitespace)
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "@owner:test";
    const R: &str = "!approval:test";
    const P: &str = "@bridge:test";
    const Q: &str = "approval_0123456789abcdef0123456789abcdef01234567";
    const D: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    fn detail() -> Value {
        json!({"version":1,"request_id":Q,"agent":"worker","project":"p","project_room_id":"!project:test","input_digest":D,"owner_mxid":A,"publisher_mxid":P})
    }
    fn request() -> Value {
        let mut d = detail();
        d["kind"] = json!("request");
        d["revision"] = json!(1);
        d["upstream_request_id"] = json!("1");
        d["runtime"] = json!("codex");
        d["tool_name"] = json!("test");
        d["description"] = json!("Test request");
        d["input_preview"] = json!("test");
        d["expires_at"] = json!(5000);
        d["actions"] = json!([{"id":"approve_once","label":"Approve once","style":"primary"},{"id":"deny","label":"Deny","style":"danger"}]);
        json!({"msgtype":REQUEST_TYPE,KEY:d})
    }
    fn status(rev: u64, state: &str, decision: Option<&str>) -> Value {
        let mut d = detail();
        d["kind"] = json!("status");
        d["revision"] = json!(rev);
        d["state"] = json!(state);
        d["decision"] = decision.map_or(Value::Null, |v| json!(v));
        json!({"msgtype":STATUS_TYPE,KEY:d})
    }
    fn put(s: &mut ApprovalSession, id: &str, v: &Value) -> IngestResult {
        s.ingest_message(A, R, id, P, v, 1000)
    }
    fn ck() -> ApprovalClaimKey {
        ApprovalClaimKey {
            account_mxid: A.into(),
            approval_room_id: R.into(),
            request_id: Q.into(),
            input_digest: D.into(),
        }
    }
    fn marker() -> Value {
        json!({"version":2,"binding_generation":1,"publisher_mxid":P,"owner_mxid":A,"project_room_associations":[{"agent":"worker","project_room_id":"!project:test","active":true}]})
    }

    fn marker_v2() -> Value {
        json!({
            "version": 2,
            "binding_generation": 7,
            "publisher_mxid": P,
            "owner_mxid": A,
            "project_room_associations": [
                {"agent":"probe","project_room_id":"!project1:test","active":true},
                {"agent":"claude","project_room_id":"!project1:test","active":true},
                {"agent":"claude","project_room_id":"!project2:test","active":true},
                {"agent":"codex","project_room_id":"!project2:test","active":true}
            ]
        })
    }

    #[test]
    fn approval_state_accepts_exact_original_bindings_and_rejects_sender_owner_digest_and_action_mismatches()
     {
        let mut s = ApprovalSession::default();
        assert!(matches!(
            put(&mut s, "$r", &request()),
            IngestResult::Changed(_)
        ));
        assert!(s.is_actionable(A, R, Q, 1000));
        let mut bad = request();
        bad[KEY]["owner_mxid"] = json!("owner");
        assert!(matches!(put(&mut s, "$b", &bad), IngestResult::Rejected(_)));
        let mut bad = request();
        bad[KEY]["input_digest"] = json!("abc");
        assert!(matches!(put(&mut s, "$b", &bad), IngestResult::Rejected(_)));
        assert!(matches!(
            s.ingest_message(A, R, "$b", "@other:test", &request(), 1000),
            IngestResult::Rejected(_)
        ));
        assert!(!s.is_actionable("@viewer:test", R, Q, 1000));
    }
    #[test]
    fn approval_status_before_request_folds_to_the_highest_terminal_revision() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$s", &status(3, "consumed", Some("allow")));
        put(&mut s, "$r", &request());
        put(&mut s, "$old", &status(2, "approved", Some("allow")));
        let v = s.approval(A, R, Q).unwrap();
        assert_eq!(v.state, CanonicalApprovalState::Consumed);
        assert_eq!(v.latest_revision, 3);
        assert!(!s.is_actionable(A, R, Q, 1000));
    }
    #[test]
    fn equal_revision_conflict_fails_closed() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$r", &request());
        put(&mut s, "$a", &status(2, "approved", Some("allow")));
        assert!(matches!(
            put(&mut s, "$d", &status(2, "denied", Some("deny"))),
            IngestResult::Rejected(ApprovalModelError::RevisionConflict)
        ));
        assert!(s.approval(A, R, Q).unwrap().conflicted);
    }
    #[test]
    fn approval_navigation_requires_unique_joined_loaded_room() {
        let mut i = ApprovalMarkerIndex::default();
        i.ingest_marker(A, R, "$m", MARKER_TYPE, P, "", &marker(), 1000)
            .unwrap();
        let joined = HashSet::from([R.into()]);
        assert_eq!(
            i.resolve(A, "worker", "!project:test", &joined, 1000),
            MarkerResolution::Unique(R.into())
        );
        assert_eq!(
            i.resolve(
                A,
                "worker",
                "!project:test",
                &joined,
                1000 + MARKER_FRESHNESS_MS + 1
            ),
            MarkerResolution::Stale
        );
        assert_eq!(
            i.resolve(A, "worker", "!project:test", &HashSet::new(), 1000),
            MarkerResolution::NotJoined
        );
    }
    #[test]
    fn persisted_marker_requires_revalidation() {
        let mut i = ApprovalMarkerIndex::default();
        i.ingest_marker(A, R, "$m", MARKER_TYPE, P, "", &marker(), 1000)
            .unwrap();
        let bytes = serde_json::to_vec(&i).unwrap();
        let wire: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire["version"], 3);
        assert!(wire.get("protocol_floors").is_none());
        assert!(wire["markers"][0].get("protocol").is_none());
        let mut restored: ApprovalMarkerIndex = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            restored.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1000
            ),
            MarkerResolution::Stale
        );
        assert_eq!(restored.markers[0].event, "$m");
        assert_eq!(restored.markers[0].publisher, P);
        restored
            .ingest_marker(A, R, "$m", MARKER_TYPE, P, "", &marker(), 1001)
            .unwrap();
        assert_eq!(
            restored.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1001
            ),
            MarkerResolution::Unique(R.into())
        );
    }

    #[test]
    fn approval_marker_v2_shared_room_resolves_all_four_bindings() {
        // Membership observation is deliberately absent from the marker wire
        // model. `None` cannot remove a canonical binding-derived tuple.
        let agent_joined_observation: Option<bool> = None;
        assert_eq!(agent_joined_observation, None);
        let mut index = ApprovalMarkerIndex::default();
        index
            .ingest_marker(A, R, "$v2", MARKER_TYPE, P, "", &marker_v2(), 1000)
            .unwrap();
        let joined = HashSet::from([R.into()]);

        assert_eq!(
            index.resolve(A, "claude", "!project2:test", &joined, 1000),
            MarkerResolution::Unique(R.into()),
        );
        assert_eq!(
            index.resolve(A, "codex", "!project2:test", &joined, 1000),
            MarkerResolution::Unique(R.into()),
        );
        assert_eq!(
            index.resolve(A, "probe", "!project1:test", &joined, 1000),
            MarkerResolution::Unique(R.into()),
        );
    }

    #[test]
    fn approval_marker_v2_rejects_v1_and_preserves_generation_after_revalidation() {
        let mut index = ApprovalMarkerIndex::default();
        assert!(
            index
                .ingest_marker(
                    A,
                    R,
                    "$v1",
                    "com.agentchat.approval.room.v1",
                    P,
                    "",
                    &marker(),
                    1000
                )
                .is_err()
        );
        assert_eq!(
            index.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1000,
            ),
            MarkerResolution::Missing,
        );

        index
            .ingest_marker(A, R, "$v2", MARKER_TYPE, P, "", &marker_v2(), 2000)
            .unwrap();
        index.begin_revalidation();
        assert_eq!(
            index.resolve(
                A,
                "claude",
                "!project2:test",
                &HashSet::from([R.into()]),
                2001
            ),
            MarkerResolution::Stale
        );
        let mut index: ApprovalMarkerIndex =
            serde_json::from_value(serde_json::to_value(index).unwrap()).unwrap();
        let mut malformed = marker_v2();
        malformed["binding_generation"] = json!(6);
        assert!(
            index
                .ingest_marker(A, R, "$old", MARKER_TYPE, P, "", &malformed, 3000)
                .is_err()
        );
        assert_eq!(
            index.resolve(
                A,
                "claude",
                "!project2:test",
                &HashSet::from([R.into()]),
                3000,
            ),
            MarkerResolution::Stale,
        );
    }

    #[test]
    fn approval_marker_v2_rejects_duplicate_and_overflow_without_partial_routes() {
        let mut index = ApprovalMarkerIndex::default();
        let mut duplicate = marker_v2();
        duplicate["project_room_associations"] = json!([
            {"agent":"claude","project_room_id":"!project:test","active":true},
            {"agent":"claude","project_room_id":"!project:test","active":false}
        ]);
        assert!(
            index
                .ingest_marker(A, R, "$duplicate", MARKER_TYPE, P, "", &duplicate, 1000)
                .is_err()
        );
        assert_eq!(
            index.resolve(
                A,
                "claude",
                "!project:test",
                &HashSet::from([R.into()]),
                1000
            ),
            MarkerResolution::Missing,
        );

        let mut overflow = marker_v2();
        overflow["project_room_associations"] = Value::Array(
            (0..=MAX_ASSOCIATIONS)
                .map(|item| {
                    json!({
                        "agent": format!("agent{item}"),
                        "project_room_id": format!("!project{item}:test"),
                        "active": true
                    })
                })
                .collect(),
        );
        assert_eq!(
            index.ingest_marker(A, R, "$overflow", MARKER_TYPE, P, "", &overflow, 2000),
            Err(ApprovalModelError::CapacityExceeded),
        );
        assert_eq!(
            index.resolve(
                A,
                "agent0",
                "!project0:test",
                &HashSet::from([R.into()]),
                2000
            ),
            MarkerResolution::Missing,
        );
    }

    #[test]
    fn unknown_persisted_marker_cache_version_fails_closed() {
        let restored: ApprovalMarkerIndex = serde_json::from_value(json!({
            "version": 99,
            "markers": [{
                "account": A,
                "room": R,
                "owner": A,
                "publisher": P,
                "generation": 1,
                "associations": [{
                    "agent": "worker",
                    "project_room_id": "!project:test",
                    "active": true
                }]
            }]
        }))
        .unwrap();
        assert_eq!(
            restored.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1000
            ),
            MarkerResolution::Missing,
        );
    }

    #[test]
    fn lower_marker_generation_invalidates_cached_high_water_mapping() {
        let mut index = ApprovalMarkerIndex::default();
        let mut generation_two = marker();
        generation_two["binding_generation"] = json!(2);
        index
            .ingest_marker(A, R, "$m2", MARKER_TYPE, P, "", &generation_two, 1000)
            .unwrap();

        let mut stale_generation = marker();
        stale_generation["project_room_associations"][0]["active"] = json!(false);
        assert_eq!(
            index.ingest_marker(A, R, "$m1", MARKER_TYPE, P, "", &stale_generation, 2000),
            Err(ApprovalModelError::RevisionConflict),
        );
        assert_eq!(
            index.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                2000,
            ),
            MarkerResolution::Stale,
        );

        index
            .ingest_marker(
                A,
                R,
                "$m2-refresh",
                MARKER_TYPE,
                P,
                "",
                &generation_two,
                3000,
            )
            .unwrap();
        assert_eq!(
            index.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                3000,
            ),
            MarkerResolution::Unique(R.into()),
        );
    }
    #[test]
    fn approval_verdict_outcome_keeps_unknown_claim_locked() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$r", &request());
        let k = ck();
        let before = s.room_epoch(A, R);
        let g = s
            .try_claim(k.clone(), ApprovalAction::ApproveOnce, 1000)
            .unwrap();
        assert!(g.transaction_id.len() <= 128);
        assert!(s.room_epoch(A, R) > before);
        assert_eq!(
            s.record_send_result(&k, g.token, SendResult::ConfirmedPreSendFailure, 1000),
            ClaimDisposition::Released
        );
        let second = s
            .try_claim(k.clone(), ApprovalAction::ApproveOnce, 1000)
            .unwrap();
        assert_eq!(
            s.record_send_result(&k, second.token, SendResult::OutcomeUnknown, 1000),
            ClaimDisposition::Retained
        );
        assert_eq!(s.claim_state(&k), Some(ClaimState::OutcomeUnknown));
        assert_eq!(
            s.record_send_result(&k, second.token, SendResult::ConfirmedPreSendFailure, 1000),
            ClaimDisposition::Retained
        );
        assert_eq!(
            s.try_claim(k, ApprovalAction::ApproveOnce, 1000),
            Err(ClaimError::AlreadyClaimed)
        );
    }
    #[test]
    fn terminal_status_retires_claim_and_advances_room_epoch() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$r", &request());
        let before = s.room_epoch(A, R);
        let k = ck();
        s.try_claim(k.clone(), ApprovalAction::Deny, 1000).unwrap();
        let changed = put(&mut s, "$s", &status(2, "denied", Some("deny")));
        assert!(matches!(changed, IngestResult::Changed(_)));
        assert!(s.room_epoch(A, R) > before);
        assert_eq!(s.claim_state(&k), None);
    }
    #[test]
    fn same_revision_duplicate_ignores_body_and_timestamp() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$r", &request());
        let a = status(2, "approved", Some("allow"));
        put(&mut s, "$a", &a);
        let mut b = a.clone();
        b["body"] = json!("translated");
        b[KEY]["decided_at"] = json!(9999);
        assert_eq!(put(&mut s, "$b", &b), IngestResult::Duplicate);
    }
    #[test]
    fn lifecycle_helpers_are_account_scoped() {
        let mut s = ApprovalSession::default();
        put(&mut s, "$r", &request());
        assert!(s.approval(A, R, Q).is_some());
        let key = ck();
        let grant = s
            .try_claim(key.clone(), ApprovalAction::Deny, 1000)
            .unwrap();
        assert_eq!(
            s.record_send_result(&key, grant.token, SendResult::Sent, 1000),
            ClaimDisposition::Retained
        );
        s.remove_room(A, R);
        assert!(s.approval(A, R, Q).is_none());
        put(&mut s, "$r2", &request());
        s.clear_account(A);
        assert!(s.approval(A, R, Q).is_none());
        let mut index = ApprovalMarkerIndex::default();
        index
            .ingest_marker(A, R, "$m", MARKER_TYPE, P, "", &marker(), 1000)
            .unwrap();
        index.mark_room_unavailable(A, R);
        assert_eq!(
            index.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1000
            ),
            MarkerResolution::Stale
        );
        index.remove_room(A, R);
        assert_eq!(
            index.resolve(A, "worker", "!project:test", &HashSet::new(), 1000),
            MarkerResolution::Missing
        );
        index
            .ingest_marker(A, R, "$m2", MARKER_TYPE, P, "", &marker(), 1000)
            .unwrap();
        index.clear_account(A);
        assert_eq!(
            index.resolve(A, "worker", "!project:test", &HashSet::new(), 1000),
            MarkerResolution::Missing
        );
    }

    #[test]
    fn repeated_claims_and_unknown_delivery_remain_single_use() {
        let mut session = ApprovalSession::default();
        put(&mut session, "$r", &request());
        let grant = session
            .try_claim(ck(), ApprovalAction::ApproveOnce, 1000)
            .unwrap();
        session.record_send_result(&ck(), grant.token, SendResult::OutcomeUnknown, 1000);
        for _ in 0..40 {
            assert_eq!(
                session.record_send_result(
                    &ck(),
                    grant.token,
                    SendResult::ConfirmedPreSendFailure,
                    1000
                ),
                ClaimDisposition::Retained
            );
            assert_eq!(
                session.try_claim(ck(), ApprovalAction::ApproveOnce, 1000),
                Err(ClaimError::AlreadyClaimed)
            );
        }
    }
    #[test]
    fn migration_projections_cannot_create_requests_or_claims() {
        for content in [
            request(),
            status(1, "pending", None),
            status(2, "denied", Some("deny")),
        ] {
            for marker in [
                json!("legacy_v1"),
                Value::Null,
                json!(true),
                json!("unknown"),
            ] {
                let mut content = content.clone();
                content[KEY]["migration_kind"] = marker;
                let mut session = ApprovalSession::default();
                assert_eq!(
                    put(&mut session, "$obsolete", &content),
                    IngestResult::Rejected(ApprovalModelError::InvalidContent)
                );
                assert!(session.approval(A, R, Q).is_none());
                assert!(
                    session
                        .try_claim(ck(), ApprovalAction::ApproveOnce, 1000)
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn native_produced_marker_reaches_current_desktop_discovery() {
        // The native store test compares this same fixture with its real
        // writer output; no hand-maintained Desktop-only marker contract.
        let content: Value = serde_json::from_str(include_str!("../../crates/hagency-client/native/hagency-store/tests/fixtures/native-approval-marker.json")).unwrap();
        let owner = "@owner:example.test";
        let publisher = "@approval:example.test";
        let room = "!private:example.test";
        let joined = HashSet::from([room.to_owned()]);
        let mut index = ApprovalMarkerIndex::default();
        assert!(
            index
                .ingest_marker(
                    owner,
                    room,
                    "$wrapper",
                    MARKER_TYPE,
                    publisher,
                    "",
                    &json!({KEY:content.clone()}),
                    1000
                )
                .is_err()
        );
        assert!(
            index
                .ingest_marker(
                    owner,
                    room,
                    "$wrong",
                    MARKER_TYPE,
                    "@other:example.test",
                    "",
                    &content,
                    1000
                )
                .is_err()
        );
        index
            .ingest_marker(
                owner,
                room,
                "$native",
                MARKER_TYPE,
                publisher,
                "",
                &content,
                1000,
            )
            .unwrap();
        for agent in ["a", "b"] {
            assert_eq!(
                index.resolve(owner, agent, "!project:example.test", &joined, 1000),
                MarkerResolution::Unique(room.into())
            );
        }
        assert_eq!(
            index.resolve(owner, "other", "!project:example.test", &joined, 1000),
            MarkerResolution::Missing
        );
    }

    #[test]
    fn obsolete_marker_cache_and_live_marker_never_resolve() {
        for version in [0, 1, 2, 99] {
            let restored: ApprovalMarkerIndex = serde_json::from_value(json!({
                "version": version,
                "markers":[{"account":A,"room":R,"event":"$old-cache","owner":A,"publisher":P,"protocol":2,"generation":1,"validated_at":1000,"associations":[{"agent":"worker","project_room_id":"!project:test","active":true}]}]
            })).unwrap();
            assert!(restored.markers.is_empty());
            assert!(restored.validated.is_empty());
            assert_eq!(
                restored.resolve(
                    A,
                    "worker",
                    "!project:test",
                    &HashSet::from([R.into()]),
                    1000
                ),
                MarkerResolution::Missing
            );
        }
        let mut old = marker();
        old["version"] = json!(1);
        let mut index = ApprovalMarkerIndex::default();
        assert!(
            index
                .ingest_marker(
                    A,
                    R,
                    "$old",
                    "com.agentchat.approval.room.v1",
                    P,
                    "",
                    &old,
                    1000
                )
                .is_err()
        );
        assert!(index.markers.is_empty());
        index
            .ingest_marker(A, R, "$new", MARKER_TYPE, P, "", &marker(), 1000)
            .unwrap();
        assert_eq!(
            index.resolve(
                A,
                "worker",
                "!project:test",
                &HashSet::from([R.into()]),
                1000
            ),
            MarkerResolution::Unique(R.into())
        );
    }
}
