//! Current account/session approval state bound to validated original events.
use std::{
    collections::HashMap,
    future::{Future, IntoFuture},
    sync::{LazyLock, Mutex},
    task::Poll,
    time::Duration,
};
use matrix_sdk::{
    Client,
    ruma::{RoomId, UserId, EventId, OwnedUserId, OwnedRoomId, OwnedEventId},
};
use matrix_sdk_ui::timeline::EventTimelineItem;
use serde_json::Value;
use tokio::sync::watch;
use super::{
    approval::{
        ApprovalRequest, ApprovalAction, ApprovalDecisionState, Namespace, current_unix_time_millis,
    },
    approval_state::{self as model, ApprovalSession, CanonicalApprovalState},
};

#[derive(Clone, Debug)]
pub struct ApprovalStateChanged {
    pub room: OwnedRoomId,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    room: OwnedRoomId,
    request: String,
}
#[derive(Clone, Debug)]
struct Entry {
    request: ApprovalRequest,
    sender: String,
    conflicted: bool,
    delivery: Option<ApprovalDecisionState>,
    claim_token: Option<u64>,
}
struct Runtime {
    epoch: u64,
    owner: Option<OwnedUserId>,
    session: ApprovalSession,
    entries: HashMap<Key, Entry>,
    changed: watch::Sender<()>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            epoch: 0,
            owner: None,
            session: ApprovalSession::default(),
            entries: HashMap::new(),
            changed: watch::channel(()).0,
        }
    }
}
static STATE: LazyLock<Mutex<Runtime>> = LazyLock::new(|| Mutex::new(Runtime::default()));

/// Called on every client replacement, including a same-account re-login.
pub fn reset(owner: Option<OwnedUserId>) {
    let mut state = STATE.lock().unwrap();
    let epoch = state.epoch.wrapping_add(1);
    *state = Runtime {
        epoch,
        owner,
        ..Default::default()
    };
}

fn key(room: &RoomId, request: &ApprovalRequest) -> Key {
    Key {
        room: room.to_owned(),
        request: request.request_id.clone(),
    }
}
fn claim_key(owner: &str, key: &Key, digest: &str) -> model::ApprovalClaimKey {
    model::ApprovalClaimKey {
        account_mxid: owner.into(),
        approval_room_id: key.room.to_string(),
        request_id: key.request.clone(),
        input_digest: digest.into(),
    }
}

/// Original event contents only. Edited bodies, local echoes and arbitrary
/// agent registrations cannot establish approval authority.
pub fn ingest(client: &Client, room: &RoomId, event: &EventTimelineItem) {
    if !crate::matrix_context::is_current(client) {
        return;
    }
    let Some(owner) = client.user_id() else {
        return;
    };
    let Some(event_id) = event.event_id() else {
        return;
    };
    let Some(content) = event
        .original_json()
        .and_then(|raw| raw.get_field::<Value>("content").ok())
        .flatten()
    else {
        return;
    };
    let mut state = STATE.lock().unwrap();
    if state.owner.as_deref() != Some(owner) {
        return;
    }
    let changed = ingest_original(
        &mut state,
        room,
        event_id,
        event.sender(),
        &content,
        current_unix_time_millis(),
    );
    drop(state);
    if changed {
        makepad_widgets::Cx::post_action(ApprovalStateChanged {
            room: room.to_owned(),
        });
    }
}

fn ingest_original(
    state: &mut Runtime,
    room: &RoomId,
    event_id: &EventId,
    sender: &UserId,
    content: &Value,
    now: u64,
) -> bool {
    let Some(owner) = state.owner.clone() else {
        return false;
    };
    let Some(namespace) = content
        .get("msgtype")
        .and_then(Value::as_str)
        .and_then(Namespace::of_msgtype)
    else {
        return false;
    };
    if content.get("m.new_content").is_some()
        || content
            .get("m.relates_to")
            .and_then(|r| r.get("rel_type"))
            .is_some()
    {
        return false;
    }
    let request = if content["msgtype"].as_str() == Some(namespace.request_msgtype().as_str()) {
        let Some(request) = super::approval::parse_approval_request(content) else {
            return false;
        };
        if request.owner_mxid != owner.as_str() || request.publisher_mxid != sender.as_str() {
            return false;
        }
        Some(request)
    } else {
        None
    };
    let session = &mut state.session;
    let before = session.room_epoch(owner.as_str(), room.as_str());
    let result = session.ingest_message(
        owner.as_str(),
        room.as_str(),
        event_id.as_str(),
        sender.as_str(),
        content,
        now,
    );
    let mut changed = before != session.room_epoch(owner.as_str(), room.as_str());
    if matches!(result, model::IngestResult::Rejected(_)) {
        if changed {
            state.changed.send_replace(());
        }
        return changed;
    }
    if let Some(request) = request {
        let key = key(room, &request);
        if let Some(entry) = state.entries.get_mut(&key) {
            if entry.request != request || entry.sender != sender.as_str() {
                changed |= !entry.conflicted;
                entry.conflicted = true;
            }
        } else if state.entries.len() < 1024 {
            state.entries.insert(
                key,
                Entry {
                    request,
                    sender: sender.to_string(),
                    conflicted: false,
                    delivery: None,
                    claim_token: None,
                },
            );
        }
    }
    if changed {
        state.changed.send_replace(());
    }
    changed
}

fn decision_locked(
    state: &Runtime,
    key: &Key,
    source_event_id: &EventId,
    request: &ApprovalRequest,
    now: u64,
) -> ApprovalDecisionState {
    let Some(owner) = &state.owner else {
        return ApprovalDecisionState::Unavailable;
    };
    let Some(entry) = state.entries.get(key) else {
        return ApprovalDecisionState::Unavailable;
    };
    if entry.conflicted || entry.request != *request {
        return ApprovalDecisionState::Unavailable;
    }

    let session = &state.session;
    let Some(view) = session.approval(owner.as_str(), key.room.as_str(), &key.request) else {
        return ApprovalDecisionState::Unavailable;
    };
    if view.conflicted
        || view.binding.owner_mxid != owner.as_str()
        || request.owner_mxid != owner.as_str()
        || view.binding.publisher_mxid != request.publisher_mxid
        || view.binding.publisher_mxid != entry.sender
        || view.binding.input_digest != request.input_digest
        || !view
            .source_event_ids
            .iter()
            .any(|id| id == source_event_id.as_str())
    {
        return ApprovalDecisionState::Unavailable;
    }
    if view.state != CanonicalApprovalState::Pending {
        return ApprovalDecisionState::Confirmed(
            match view.state {
                CanonicalApprovalState::Approved => "Approved",
                CanonicalApprovalState::Denied => "Denied",
                CanonicalApprovalState::Expired => "Expired",
                CanonicalApprovalState::Consumed => "Consumed",
                _ => unreachable!(),
            }
            .into(),
        );
    }
    // "Expired" means the deadline passed before the user decided. A verdict
    // that left this device, or may have, is not that: keep showing what was
    // sent until the canonical state above replaces it.
    if let Some(
        delivery @ (ApprovalDecisionState::Sending(_)
        | ApprovalDecisionState::Sent(_)
        | ApprovalDecisionState::OutcomeUnknown(_)),
    ) = &entry.delivery
    {
        return delivery.clone();
    }
    if request.is_expired(now) {
        return ApprovalDecisionState::Expired;
    }
    if entry.delivery.is_none()
        && !session.is_actionable(owner.as_str(), key.room.as_str(), &key.request, now)
    {
        return if request.is_expired(now) {
            ApprovalDecisionState::Expired
        } else {
            ApprovalDecisionState::Unavailable
        };
    }

    if let Some(delivery) = &entry.delivery {
        return delivery.clone();
    }
    if request.is_expired(now) {
        ApprovalDecisionState::Expired
    } else {
        ApprovalDecisionState::Pending
    }
}

pub fn decision(
    room: &RoomId,
    source_event_id: &EventId,
    request: &ApprovalRequest,
    now: u64,
) -> ApprovalDecisionState {
    let owner = crate::sliding_sync::current_user_id();
    let state = STATE.lock().unwrap();
    if owner != state.owner {
        return ApprovalDecisionState::Unavailable;
    }
    decision_locked(&state, &key(room, request), source_event_id, request, now)
}

#[derive(Clone, Debug)]
pub struct Claim {
    owner: OwnedUserId,
    epoch: u64,
    key: Key,
    pub transaction_id: String,
    pub source_event_id: OwnedEventId,
    digest: String,
    grant: model::ClaimGrant,
    expires_at_millis: u64,
    changed: watch::Receiver<()>,
}
pub fn claim(
    room: &RoomId,
    event: &EventTimelineItem,
    request: &ApprovalRequest,
    action: &ApprovalAction,
) -> Result<Claim, &'static str> {
    let owner = crate::sliding_sync::current_user_id().ok_or("Session ended")?;
    if !crate::sliding_sync::get_client()
        .and_then(|c| c.get_room(room))
        .is_some_and(|r| r.state() == matrix_sdk::RoomState::Joined)
    {
        return Err("Approval room is no longer joined");
    }
    let source_event_id = event.event_id().ok_or("Missing request event")?;
    let content = event
        .original_json()
        .and_then(|raw| raw.get_field::<Value>("content").ok())
        .flatten()
        .ok_or("Missing original request")?;
    if super::approval::parse_approval_request(&content).as_ref() != Some(request) {
        return Err("Original request changed or unavailable");
    }
    let mut state = STATE.lock().unwrap();
    let claim = claim_locked(
        &mut state,
        &owner,
        room,
        source_event_id,
        event.sender(),
        request,
        action,
        current_unix_time_millis(),
    )?;
    drop(state);
    makepad_widgets::Cx::post_action(ApprovalStateChanged {
        room: room.to_owned(),
    });
    Ok(claim)
}

fn claim_locked(
    state: &mut Runtime,
    owner: &UserId,
    room: &RoomId,
    source_event_id: &EventId,
    sender: &UserId,
    request: &ApprovalRequest,
    action: &ApprovalAction,
    now: u64,
) -> Result<Claim, &'static str> {
    let key = key(room, request);
    if state.owner.as_deref() != Some(owner)
        || !matches!(
            decision_locked(state, &key, source_event_id, request, now),
            ApprovalDecisionState::Pending
        )
        || request.action(&action.id) != Some(action)
    {
        return Err("This request is expired, unavailable, or already has a decision in progress.");
    }
    if sender.as_str() != state.entries[&key].sender {
        return Err("Approval publisher changed");
    }
    let choice = match action.id.as_str() {
        "approve_once" => model::ApprovalAction::ApproveOnce,
        "approve_task" => model::ApprovalAction::ApproveTask,
        "approve_always" => model::ApprovalAction::ApproveAlways,
        "deny" => model::ApprovalAction::Deny,
        _ => return Err("Unsupported approval action"),
    };
    let grant = state
        .session
        .try_claim(
            claim_key(owner.as_str(), &key, &request.input_digest),
            choice,
            now,
        )
        .map_err(|_| "Approval already claimed or unavailable")?;
    let transaction_id = format!(
        "rinx.{}.{}.{}.{}",
        request.namespace.base(),
        request.request_id,
        action.id,
        &request.input_digest[..16]
    );
    let entry = state.entries.get_mut(&key).unwrap();
    entry.delivery = Some(ApprovalDecisionState::Sending(action.clone()));
    entry.claim_token = Some(grant.token);
    state.changed.send_replace(());
    Ok(Claim {
        owner: owner.to_owned(),
        epoch: state.epoch,
        key,
        transaction_id,
        source_event_id: source_event_id.to_owned(),
        digest: request.input_digest.clone(),
        grant,
        expires_at_millis: request.expires_at_millis,
        changed: state.changed.subscribe(),
    })
}

fn claim_current_locked(state: &Runtime, claim: &Claim, now: u64) -> bool {
    state.epoch == claim.epoch && state.owner.as_ref() == Some(&claim.owner)
        && state.entries.get(&claim.key).is_some_and(|entry| {
            !entry.conflicted && entry.claim_token == Some(claim.grant.token)
                && matches!(&entry.delivery, Some(ApprovalDecisionState::Sending(action)) if action.id == claim.grant.action.name())
        })
        && state.session.claim_is_current(&claim_key(claim.owner.as_str(), &claim.key, &claim.digest),
            claim.grant.token, claim.grant.action, claim.source_event_id.as_str(), now)
}

impl Claim {
    pub fn is_current(&self, client: &Client) -> bool {
        if !crate::matrix_context::is_current(client) || client.user_id() != Some(&self.owner) {
            return false;
        }
        if !client
            .get_room(&self.key.room)
            .is_some_and(|r| r.state() == matrix_sdk::RoomState::Joined)
        {
            return false;
        }
        let state = STATE.lock().unwrap();
        claim_current_locked(&state, self, current_unix_time_millis())
    }

    /// Guard the actual SDK future, including its internal encryption awaits.
    /// Cancellation after polling it is an uncertain outcome: HTTP may already
    /// have started, so callers must not release this claim for a blind retry.
    pub(crate) async fn while_current<F: IntoFuture>(
        &self,
        client: &Client,
        operation: F,
    ) -> Result<F::Output, ClaimInvalidated> {
        while_current(
            operation,
            self.changed.clone(),
            self.expires_at_millis,
            || self.is_current(client),
        )
        .await
    }

    pub(crate) fn complete(&self, result: model::SendResult) {
        let mut state = STATE.lock().unwrap();
        let changed = complete_locked(&mut state, self, result, current_unix_time_millis());
        drop(state);
        if changed {
            makepad_widgets::Cx::post_action(ApprovalStateChanged {
                room: self.key.room.clone(),
            });
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClaimInvalidated;

async fn while_current<F: IntoFuture>(
    operation: F,
    mut changed: watch::Receiver<()>,
    expires_at_millis: u64,
    is_current: impl Fn() -> bool,
) -> Result<F::Output, ClaimInvalidated> {
    let Some(deadline) = tokio::time::Instant::now().checked_add(Duration::from_millis(
        expires_at_millis.saturating_sub(current_unix_time_millis()),
    )) else {
        return Err(ClaimInvalidated);
    };
    let invalidated = async {
        loop {
            if !is_current() {
                return;
            }
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => return,
                result = changed.changed() => if result.is_err() { return; },
            }
        }
    };
    let operation = operation.into_future();
    tokio::pin!(operation);
    let guarded = std::future::poll_fn(|cx| {
        // SDK futures may resume several awaits within one poll. Recheck before
        // every poll so a ready HTTP response cannot win over known revocation.
        if !is_current() || tokio::time::Instant::now() >= deadline {
            Poll::Ready(Err(ClaimInvalidated))
        } else {
            operation.as_mut().poll(cx).map(Ok)
        }
    });
    tokio::select! {
        biased;
        _ = invalidated => Err(ClaimInvalidated),
        result = guarded => result,
    }
}

fn complete_locked(
    state: &mut Runtime,
    claim: &Claim,
    result: model::SendResult,
    now: u64,
) -> bool {
    if state.epoch != claim.epoch
        || state.owner.as_ref() != Some(&claim.owner)
        || !state
            .entries
            .get(&claim.key)
            .is_some_and(|entry| entry.claim_token == Some(claim.grant.token))
    {
        return false;
    }
    let disposition = state.session.record_send_result(
        &claim_key(claim.owner.as_str(), &claim.key, &claim.digest),
        claim.grant.token,
        result,
        now,
    );
    if let Some(entry) = state.entries.get_mut(&claim.key)
        && let Some(ApprovalDecisionState::Sending(action)) = entry.delivery.clone()
    {
        entry.delivery = match result {
            model::SendResult::ConfirmedPreSendFailure
                if disposition == model::ClaimDisposition::Released =>
            {
                entry.claim_token = None;
                None
            }
            model::SendResult::ConfirmedPreSendFailure => Some(ApprovalDecisionState::Unavailable),
            model::SendResult::OutcomeUnknown => {
                Some(ApprovalDecisionState::OutcomeUnknown(action))
            }
            model::SendResult::Sent => Some(ApprovalDecisionState::Sent(action)),
        };
    }
    state.changed.send_replace(());
    true
}

#[cfg(test)]
#[path = "approval_send_tests.rs"]
mod http_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn corpus() -> Vec<Value> {
        let fixture: Value = serde_json::from_str(include_str!("../../crates/hagency-client/native/hagency-store/tests/fixtures/native-approval-wire.json")).unwrap();
        // The native_approval_wire_corpus store test compares this same file
        // against actual DomainRepository output, including IDs and actions.
        fixture["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["content"].clone())
            .collect()
    }
    fn in_namespace(mut content: Value, namespace: Namespace) -> Value {
        let detail = content
            .as_object_mut()
            .unwrap()
            .remove("com.agentchat.approval")
            .unwrap();
        content[namespace.event_key()] = detail;
        content["msgtype"] = json!(namespace.request_msgtype());
        content
    }
    fn blank(request: &ApprovalRequest) -> Runtime {
        Runtime {
            owner: Some(request.owner_mxid.clone().try_into().unwrap()),
            ..Default::default()
        }
    }
    fn room() -> &'static RoomId {
        matrix_sdk::ruma::room_id!("!private:example.test")
    }
    fn source() -> &'static EventId {
        matrix_sdk::ruma::event_id!("$request")
    }
    fn ingest(state: &mut Runtime, event: &EventId, content: &Value, now: u64) -> bool {
        ingest_original(
            state,
            room(),
            event,
            matrix_sdk::ruma::user_id!("@approval:example.test"),
            content,
            now,
        )
    }
    fn take_claim(
        state: &mut Runtime,
        event: &EventId,
        request: &ApprovalRequest,
        action: &str,
        now: u64,
    ) -> Result<Claim, &'static str> {
        let owner = UserId::parse(&request.owner_mxid).unwrap();
        let publisher = UserId::parse(&request.publisher_mxid).unwrap();
        claim_locked(
            state,
            &owner,
            room(),
            event,
            &publisher,
            request,
            request.action(action).unwrap(),
            now,
        )
    }
    fn status(content: &Value, namespace: Namespace) -> Value {
        let mut content = content.clone();
        content["msgtype"] = json!(namespace.status_msgtype());
        let detail = &mut content[namespace.event_key()];
        detail["kind"] = json!("status");
        detail["state"] = json!("denied");
        detail["decision"] = json!("deny");
        detail["revision"] = json!(2);
        detail.as_object_mut().unwrap().remove("actions");
        content
    }

    #[test]
    fn native_generated_cards_reach_current_claim_and_verdict_for_all_offered_actions() {
        for namespace in Namespace::ALL {
            for content in corpus() {
                let content = in_namespace(content, namespace);
                let request = super::super::approval::parse_approval_request(&content).unwrap();
                assert_eq!(
                    request.request_id.strip_prefix("approval_").unwrap().len(),
                    40
                );
                for action in &request.actions {
                    let mut state = blank(&request);
                    assert!(ingest(&mut state, source(), &content, 1000));
                    assert_eq!(
                        decision_locked(&state, &key(room(), &request), source(), &request, 1000),
                        ApprovalDecisionState::Pending
                    );
                    let claim =
                        take_claim(&mut state, source(), &request, &action.id, 1000).unwrap();
                    assert!(claim_current_locked(&state, &claim, 1000));
                    assert!(take_claim(&mut state, source(), &request, &action.id, 1000).is_err());
                    let verdict = request.verdict_content(action, &claim.source_event_id);
                    let detail = &verdict[namespace.event_key()];
                    assert_eq!(
                        detail["request_id"],
                        content[namespace.event_key()]["request_id"]
                    );
                    assert_eq!(
                        detail["input_digest"],
                        content[namespace.event_key()]["input_digest"]
                    );
                    assert_eq!(detail["action"], action.id);
                    assert_eq!(
                        verdict["m.relates_to"]["m.in_reply_to"]["event_id"],
                        source().as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn rejected_siblings_never_borrow_a_current_original_event() {
        for namespace in Namespace::ALL {
            let modern = in_namespace(corpus().remove(0), namespace);
            let request = super::super::approval::parse_approval_request(&modern).unwrap();
            let mut rejected = Vec::new();
            for field in ["owner_mxid", "publisher_mxid", "revision"] {
                let mut missing = modern.clone();
                missing[namespace.event_key()]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                rejected.push(missing);
                let mut null = modern.clone();
                null[namespace.event_key()][field] = Value::Null;
                rejected.push(null);
                let mut wrong = modern.clone();
                wrong[namespace.event_key()][field] = json!({});
                rejected.push(wrong);
            }
            for value in [json!("legacy_v1"), Value::Null, json!("unknown")] {
                let mut migrated = modern.clone();
                migrated[namespace.event_key()]["migration_kind"] = value;
                rejected.push(migrated);
            }
            let mut edited = modern.clone();
            edited["m.relates_to"] = json!({"rel_type":"m.replace","event_id":source().as_str()});
            rejected.push(edited);
            for obsolete in rejected {
                assert!(super::super::approval::parse_approval_request(&obsolete).is_none());
                for old_first in [true, false] {
                    let mut state = blank(&request);
                    let old = matrix_sdk::ruma::event_id!("$old");
                    if old_first {
                        assert!(!ingest(&mut state, old, &obsolete, 1000));
                    }
                    assert!(ingest(&mut state, source(), &modern, 1000));
                    if !old_first {
                        assert!(!ingest(&mut state, old, &obsolete, 1000));
                    }
                    assert_eq!(
                        decision_locked(&state, &key(room(), &request), old, &request, 1000),
                        ApprovalDecisionState::Unavailable
                    );
                    assert!(take_claim(&mut state, old, &request, "approve_once", 1000).is_err());
                    assert!(
                        take_claim(&mut state, source(), &request, "approve_once", 1000).is_ok()
                    );
                }
            }
            let mut state = blank(&request);
            ingest(&mut state, source(), &modern, 1000);
            // Even an identical, valid-looking body on a different event is
            // not the original event retained by the canonical request.
            let alias = matrix_sdk::ruma::event_id!("$alias");
            ingest(&mut state, alias, &modern, 1000);
            assert!(take_claim(&mut state, alias, &request, "approve_once", 1000).is_err());
        }
    }

    #[tokio::test]
    async fn preparation_cannot_send_after_terminal_conflict_expiry_or_session_replacement() {
        for interruption in ["terminal", "conflict", "expired", "relogin", "account"] {
            let content = corpus().remove(0);
            let request = super::super::approval::parse_approval_request(&content).unwrap();
            let mut state = blank(&request);
            ingest(&mut state, source(), &content, 1000);
            let claim = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
            let state = std::sync::Arc::new(Mutex::new(state));
            let task_state = state.clone();
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, resumed) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                assert!(claim_current_locked(
                    &task_state.lock().unwrap(),
                    &claim,
                    1000
                ));
                started.send(()).unwrap();
                let now = resumed.await.unwrap();
                // This is the predicate invoked again after key preparation,
                // immediately before the production Matrix send.
                let may_send = claim_current_locked(&task_state.lock().unwrap(), &claim, now);
                may_send
            });
            ready.await.unwrap();
            let mut now = 1001;
            {
                let mut state = state.lock().unwrap();
                match interruption {
                    "terminal" => {
                        ingest(
                            &mut state,
                            matrix_sdk::ruma::event_id!("$status"),
                            &status(&content, Namespace::DEFAULT),
                            now,
                        );
                    }
                    "conflict" => {
                        let mut changed = content.clone();
                        changed[Namespace::DEFAULT.event_key()]["input_digest"] =
                            json!("f".repeat(64));
                        ingest(
                            &mut state,
                            matrix_sdk::ruma::event_id!("$conflict"),
                            &changed,
                            now,
                        );
                    }
                    "expired" => now = request.expires_at_millis,
                    "relogin" => state.epoch += 1,
                    "account" => {
                        state.owner =
                            Some(matrix_sdk::ruma::user_id!("@other:example.test").to_owned())
                    }
                    _ => unreachable!(),
                }
            }
            release.send(now).unwrap();
            assert!(!task.await.unwrap(), "stale send after {interruption}");
        }
    }

    #[test]
    fn original_claim_token_fences_late_completions_and_unknown_outcomes() {
        let content = corpus().remove(0);
        let request = super::super::approval::parse_approval_request(&content).unwrap();
        let mut state = blank(&request);
        ingest(&mut state, source(), &content, 1000);
        let first = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
        complete_locked(
            &mut state,
            &first,
            model::SendResult::ConfirmedPreSendFailure,
            1000,
        );
        let second = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
        assert_ne!(first.grant.token, second.grant.token);
        assert!(!claim_current_locked(&state, &first, 1000));
        assert!(!complete_locked(
            &mut state,
            &first,
            model::SendResult::ConfirmedPreSendFailure,
            1000
        ));
        assert!(claim_current_locked(&state, &second, 1000));
        complete_locked(&mut state, &second, model::SendResult::OutcomeUnknown, 1000);
        complete_locked(
            &mut state,
            &second,
            model::SendResult::ConfirmedPreSendFailure,
            1000,
        );
        assert!(take_claim(&mut state, source(), &request, "deny", 1000).is_err());
        assert!(matches!(
            decision_locked(&state, &key(room(), &request), source(), &request, 1000),
            ApprovalDecisionState::OutcomeUnknown(_)
        ));
    }

    #[test]
    fn a_verdict_sent_in_time_is_not_shown_as_expired_after_the_deadline() {
        let content = corpus().remove(0);
        let request = super::super::approval::parse_approval_request(&content).unwrap();
        let after = request.expires_at_millis + 1;
        let read = |state: &Runtime| {
            decision_locked(state, &key(room(), &request), source(), &request, after)
        };

        // Nobody decided before the deadline.
        let mut state = blank(&request);
        ingest(&mut state, source(), &content, 1000);
        assert_eq!(read(&state), ApprovalDecisionState::Expired);

        // The verdict left this device in time; the bridge has not confirmed it.
        let mut state = blank(&request);
        ingest(&mut state, source(), &content, 1000);
        let claim = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
        assert!(matches!(read(&state), ApprovalDecisionState::Sending(_)));
        complete_locked(&mut state, &claim, model::SendResult::Sent, 1000);
        assert!(matches!(read(&state), ApprovalDecisionState::Sent(_)));

        // The send may have reached the bridge.
        let mut state = blank(&request);
        ingest(&mut state, source(), &content, 1000);
        let claim = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
        complete_locked(&mut state, &claim, model::SendResult::OutcomeUnknown, 1000);
        assert!(matches!(read(&state), ApprovalDecisionState::OutcomeUnknown(_)));

        // A send that definitely did not happen leaves only the deadline to show.
        let mut state = blank(&request);
        ingest(&mut state, source(), &content, 1000);
        let claim = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
        complete_locked(
            &mut state,
            &claim,
            model::SendResult::ConfirmedPreSendFailure,
            1000,
        );
        assert_eq!(read(&state), ApprovalDecisionState::Expired);
    }

    #[test]
    fn canonical_status_and_exact_owner_publisher_are_required() {
        for namespace in Namespace::ALL {
            let content = in_namespace(corpus().remove(0), namespace);
            let request = super::super::approval::parse_approval_request(&content).unwrap();
            let mut state = blank(&request);
            let wrong = matrix_sdk::ruma::user_id!("@other:example.test");
            assert!(!ingest_original(
                &mut state,
                room(),
                source(),
                wrong,
                &content,
                1000
            ));
            assert!(state.entries.is_empty());
            ingest(&mut state, source(), &content, 1000);
            let action = request.action("approve_once").unwrap();
            assert!(
                claim_locked(
                    &mut state,
                    wrong,
                    room(),
                    source(),
                    wrong,
                    &request,
                    action,
                    1000
                )
                .is_err()
            );
            assert!(
                claim_locked(
                    &mut state,
                    &UserId::parse(&request.owner_mxid).unwrap(),
                    room(),
                    source(),
                    wrong,
                    &request,
                    action,
                    1000
                )
                .is_err()
            );
            let claim = take_claim(&mut state, source(), &request, "approve_once", 1000).unwrap();
            complete_locked(&mut state, &claim, model::SendResult::OutcomeUnknown, 1000);
            let status = status(&content, namespace);
            ingest_original(
                &mut state,
                room(),
                matrix_sdk::ruma::event_id!("$spoof"),
                wrong,
                &status,
                1100,
            );
            assert!(matches!(
                decision_locked(&state, &key(room(), &request), source(), &request, 1100),
                ApprovalDecisionState::OutcomeUnknown(_)
            ));
            ingest(
                &mut state,
                matrix_sdk::ruma::event_id!("$status"),
                &status,
                1100,
            );
            assert_eq!(
                decision_locked(&state, &key(room(), &request), source(), &request, 1100),
                ApprovalDecisionState::Confirmed("Denied".into())
            );
        }
    }
}
