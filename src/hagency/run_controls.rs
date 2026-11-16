//! Run cards admitted by the existing authenticated, retrying Matrix card
//! observer. This store owns presentation and sends only the signed-in user's
//! actions; the executor repeats identity, membership and execution checks.
use ::hagency::agent_run_controls::{self as protocol, Action, Card, State as RunState};
use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, OwnedUserId, RoomId};
use serde_json::Value;
use std::{
    sync::{LazyLock, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub agent: String,
    pub name: String,
    pub card: Card,
    pub draft: String,
    pub hint: String,
    pub sending: bool,
    pub may_append: bool,
    pub may_stop: bool,
    pub recovered_drafts: Vec<String>,
    /// When this app first saw the run, for the card's ticking elapsed time.
    pub started: std::time::Instant,
    sent_event: Option<String>,
    sent_owner: Option<String>,
    pending_drafts: Vec<PendingDraft>,
    retry: Option<Requested>,
}
#[derive(Clone, Debug)]
struct PendingDraft {
    event: String,
    owner: String,
    text: String,
}
impl Entry {
    pub fn unconfirmed_text(&self) -> Option<String> {
        (!self.card.state.active() && !self.pending_drafts.is_empty()).then(||
            self.pending_drafts.iter().map(|draft| draft.text.as_str()).collect::<Vec<_>>().join("\n\n")
        )
    }
}
#[derive(Clone, Debug)]
pub struct Requested {
    generation: u64,
    key: String,
    owner: String,
    agent: String,
    room: String,
    txn: OwnedTransactionId,
    kind: &'static str,
    content: Value,
    text: Option<String>,
}
#[derive(Debug)]
pub struct Sent {
    request: Requested,
    result: Result<String, String>,
}
#[derive(Default)]
struct Store {
    generation: u64,
    owner: String,
    entries: Vec<Entry>,
    edits: Vec<(String, String, String, Card)>,
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);
pub(super) fn reset(generation: u64, owner: &str) {
    *STORE.lock().unwrap_or_else(|e| e.into_inner()) = Store {
        generation,
        owner: owner.into(),
        ..Default::default()
    };
}
fn apply(entry: &mut Entry, card: Card) -> bool {
    if !entry.card.same_identity(&card)
        || card.revision <= entry.card.revision
        || (!entry.card.state.active() && card.state != entry.card.state)
    {
        return false;
    }
    if entry.card.state == RunState::Stopping && card.state == RunState::Running {
        return false;
    }
    apply_receipts(entry, &card);
    entry.card = card;
    true
}
fn apply_receipts(entry: &mut Entry, card: &Card) {
    let mut recovered = false;
    let mut last_acknowledged = false;
    for receipt in card.action_receipts.iter().chain(card.last_action.iter()) {
        if entry.sent_event.as_deref() == Some(&receipt.event_id)
            && entry.sent_owner.as_deref() == Some(&receipt.sender)
        {
            last_acknowledged = true;
            entry.hint = match receipt.outcome.as_str() {
            "accepted" => "The Agent received this action.",
            "queued" => "The Agent queued the instructions for its next turn.",
            "rejected" => "The runtime refused this action.",
            "unsupported" => "This runtime does not support this action.",
            "in_progress" => "A stop request is already being handled.",
            "busy" => "The action queue is busy; submit again shortly.",
            "unavailable" => "This run has ended; the action was not carried out.",
            _ => "Could not confirm the result; it will not be resent automatically, to avoid running it twice.",
        }
            .into();
        }
        let Some(index) = entry.pending_drafts.iter().position(|draft|
            draft.event == receipt.event_id && draft.owner == receipt.sender
        ) else { continue; };
        match receipt.outcome.as_str() {
            "accepted" | "queued" => { entry.pending_drafts.remove(index); }
            "rejected" | "unsupported" | "busy" | "unavailable" => {
                let draft = entry.pending_drafts.remove(index);
                entry.recovered_drafts.push(draft.text);
                recovered = true;
            }
            _ => {} // Unknown application is never retried or restored as unapplied.
        }
    }
    if recovered {
        restore_next_draft(entry);
        entry.hint = "Some instructions were not carried out; their text is kept. Copy and send it, or submit it again while a run is active.".into();
    } else if !card.state.active() && entry.sent_event.is_some() && !last_acknowledged {
        entry.hint = "The run has ended and the result is unconfirmed. Check first; nothing is resent automatically.".into();
    }
}
fn restore_next_draft(entry: &mut Entry) -> bool {
    if !entry.draft.is_empty() || entry.recovered_drafts.is_empty() { return false; }
    entry.draft = entry.recovered_drafts.remove(0);
    true
}
pub fn restore_draft(key: &str) -> bool {
    STORE.lock().unwrap_or_else(|e| e.into_inner()).entries.iter_mut()
        .find(|entry| entry.key == key).is_some_and(restore_next_draft)
}
/// Called only after the shared observer authenticates the Agent device and
/// verifies the event belongs to this room and this signed-in generation.
pub(super) fn authenticated(
    generation: u64,
    room: &str,
    event: &str,
    agent: &str,
    name: &str,
    content: &Value,
) -> bool {
    let edit = content["m.relates_to"]["rel_type"] == "m.replace";
    let Some(card) = Card::from_content(if edit {
        &content["m.new_content"]
    } else {
        content
    }) else {
        return false;
    };
    if card.room_id != room {
        return false;
    }
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if store.generation != generation {
        return false;
    }
    if edit {
        let Some(target) = content["m.relates_to"]["event_id"].as_str() else {
            return false;
        };
        if let Some(entry) = store
            .entries
            .iter_mut()
            .find(|e| e.key == target && e.agent == agent && e.card.room_id == room)
        {
            return apply(entry, card);
        }
        if store.edits.len() >= 64 {
            store.edits.remove(0);
        }
        store
            .edits
            .push((room.into(), target.into(), agent.into(), card));
        return false;
    }
    if card.state != RunState::Running
        || card.revision != 0
        || store
            .entries
            .iter()
            .any(|e| e.key == event || (e.agent == agent && e.card.id == card.id))
    {
        return false;
    }
    let owner = store.owner.clone();
    let mut entry = Entry {
        key: event.into(),
        agent: agent.into(),
        name: name.into(),
        may_append: card.requester == owner,
        may_stop: card.requester == owner || card.owner == owner,
        card,
        draft: String::new(),
        hint: String::new(),
        sending: false,
        sent_event: None,
        sent_owner: None,
        pending_drafts: Vec::new(),
        recovered_drafts: Vec::new(),
        started: std::time::Instant::now(),
        retry: None,
    };
    let mut edits: Vec<_> = store
        .edits
        .iter()
        .filter(|(r, t, a, _)| r == room && t == event && a == agent)
        .map(|(_, _, _, c)| c.clone())
        .collect();
    edits.sort_by_key(|c| c.revision);
    for card in edits {
        apply(&mut entry, card);
    }
    store
        .edits
        .retain(|(r, t, a, _)| r != room || t != event || a != agent);
    if store.entries.len() >= 256 {
        store.entries.remove(0);
    }
    store.entries.push(entry);
    true
}
pub fn for_event(room: &RoomId, event: &str, include_thread: bool) -> Vec<Entry> {
    let store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let mut entries: Vec<_> = store
        .entries
        .iter()
        .filter(|e| {
            e.card.room_id == room.as_str()
                && (e.card.event_id == event
                    || (include_thread && e.card.thread_root == event))
        })
        .cloned()
        .collect();
    // Every run is returned: the message's card slots page through the
    // overflow, so no answer or active run's controls become unreachable.
    entries.sort_by_key(|e| !e.card.state.active());
    entries
}
/// The newest running run in a thread, for its collapsed summary.
pub fn newest_running_in_thread(room: &RoomId, root: &str) -> Option<Entry> {
    let store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    store.entries.iter().rev()
        .find(|e| e.card.room_id == room.as_str() && e.card.thread_root == root && e.card.state == RunState::Running)
        .cloned()
}
pub fn draft(key: &str, value: String) {
    if value.len() > protocol::MAX_INPUT_BYTES {
        return;
    }
    if let Some(entry) = STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .iter_mut()
        .find(|e| e.key == key)
    {
        entry.draft = value;
    }
}
pub fn request(key: &str, stop: bool) -> Option<Requested> {
    let mut store = STORE.lock().ok()?;
    let generation = store.generation;
    let owner = store.owner.clone();
    let entry = store.entries.iter_mut().find(|e| e.key == key)?;
    if entry.sending || entry.card.state != RunState::Running {
        return None;
    }
    if !stop && entry.pending_drafts.len() + entry.recovered_drafts.len() >= protocol::MAX_ACTION_RECEIPTS {
        entry.hint = "First deal with the instructions that are unconfirmed or were not carried out.".into();
        return None;
    }
    // Codex and Claude alike take the text into the running turn.
    let action = if stop {
        Action::Interrupt {}
    } else {
        Action::Steer {
            text: entry.draft.clone(),
        }
    };
    let content = entry.card.action_content(key, &owner, action).ok()?;
    // A failed HTTP send retries this exact transaction. An explicit later
    // action gets a new transaction, even if its text happens to be identical.
    let request = entry
        .retry
        .clone()
        .filter(|r| r.content == content)
        .unwrap_or_else(|| Requested {
            generation,
            key: key.into(),
            owner,
            agent: entry.agent.clone(),
            room: entry.card.room_id.clone(),
            txn: matrix_sdk::ruma::TransactionId::new(),
            kind: protocol::ACTION_EVENT,
            content,
            text: (!stop).then(|| entry.draft.clone()),
        });
    entry.sending = true;
    entry.hint = "Sending…".into();
    Some(request)
}
// TODO(R4): run_controls.rs is over budget; split the send path (request, send, sent) into run_controls/send.rs.
/// The owner's kill switch: send "Stop this run" for every active run the
/// owner may stop, through the same request and send as the card's button.
pub fn stop_all_running() -> usize {
    let keys: Vec<String> = STORE.lock().unwrap_or_else(|e| e.into_inner()).entries.iter()
        .filter(|e| e.may_stop && e.card.state == RunState::Running)
        .map(|e| e.key.clone())
        .collect();
    keys.iter().filter_map(|key| request(key, true)).map(send).count()
}
pub fn sent(value: &Sent) -> bool {
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if store.generation != value.request.generation {
        return false;
    }
    let Some(entry) = store
        .entries
        .iter_mut()
        .find(|e| e.key == value.request.key)
    else {
        return false;
    };
    apply_send_result(entry, value);
    true
}
fn apply_send_result(entry: &mut Entry, value: &Sent) {
    entry.sending = false;
    match &value.result {
        Ok(event) => {
            entry.sent_event = Some(event.clone());
            entry.sent_owner = Some(value.request.owner.clone());
            entry.retry = None;
            entry.hint = "Sent. Waiting for the Agent to confirm.".into();
            if let Some(text) = &value.request.text {
                if entry.draft == *text { entry.draft.clear(); }
                if !entry.pending_drafts.iter().any(|draft| draft.event == *event) {
                    entry.pending_drafts.push(PendingDraft {
                        event: event.clone(), owner: value.request.owner.clone(), text: text.clone(),
                    });
                }
            }
            // The terminal card may arrive before the HTTP send response.
            let card = entry.card.clone();
            apply_receipts(entry, &card);
        }
        Err(_) => {
            entry.retry = Some(value.request.clone());
            entry.hint = "Sending was not confirmed; you can retry the same action.".into();
        }
    }
}
pub fn send(request: Requested) {
    let client = crate::sliding_sync::get_client();
    crate::sliding_sync::spawn_async_task(async move {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let client = client.ok_or("matrix_sign_in_required")?;
            if client.user_id().map(|u| u.as_str()) != Some(request.owner.as_str()) {
                return Err("matrix_identity_changed");
            }
            let room_id =
                OwnedRoomId::try_from(request.room.as_str()).map_err(|_| "room_unavailable")?;
            let room = client.get_room(&room_id).ok_or("room_unavailable")?;
            if room
                .latest_encryption_state()
                .await
                .map_err(|_| "room_state_unavailable")?
                .is_unknown()
            {
                return Err("room_state_unavailable");
            }
            let agent =
                OwnedUserId::try_from(request.agent.as_str()).map_err(|_| "agent_unavailable")?;
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                client.encryption().request_user_identity(&agent),
            )
            .await;
            {
                let store = STORE.lock().map_err(|_| "matrix_identity_changed")?;
                if store.generation != request.generation || store.owner != request.owner {
                    return Err("matrix_identity_changed");
                }
                let entry = store
                    .entries
                    .iter()
                    .find(|e| e.key == request.key)
                    .ok_or("run_unavailable")?;
                if entry.card.state != RunState::Running {
                    return Err("run_unavailable");
                }
            }
            room.send_raw(request.kind, request.content.clone())
                .with_transaction_id(&request.txn)
                .await
                .map(|sent| sent.response.event_id.to_string())
                .map_err(|_| "action_send_failed")
        })
        .await
        .unwrap_or(Err("action_send_failed"))
        .map_err(str::to_owned);
        Cx::post_action(Sent { request, result });
    });
}

/// A run as the store holds it, for view tests outside this module.
#[cfg(test)]
pub(crate) fn fixture(key: &str, state: RunState, answer: &str) -> Entry {
    let mut entry = tests::entry();
    entry.key = key.into();
    entry.card.state = state;
    entry.card.answer = answer.into();
    entry
}

/// Review fix: no run is dropped for a message with many runs.
#[cfg(test)]
#[test]
fn every_run_of_a_message_is_listed() {
    let room: OwnedRoomId = "!room:test".try_into().unwrap();
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::mem::take(&mut store.entries);
    store.entries = (0..5).map(|i| fixture(&format!("$run{i}"), if i == 4 { RunState::Running } else { RunState::Completed }, "a")).collect();
    drop(store);
    let listed = for_event(&room, "$trigger", false);
    STORE.lock().unwrap_or_else(|e| e.into_inner()).entries = saved;
    assert_eq!(listed.len(), 5);
    assert_eq!(listed[0].key, "$run4", "the active run comes first");
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn entry() -> Entry {
        let card: Card = serde_json::from_value(serde_json::json!({
            "version":1,"id":"run_fixture","dispatchId":"dispatch","executionId":"execution",
            "roomId":"!room:test","eventId":"$trigger","threadRoot":"!room:test",
            "requester":"@requester:test","owner":"@owner:test","runtime":"codex","state":"running","revision":0
        })).unwrap();
        Entry {
            key: "$card".into(),
            agent: "@_hagency_agt_test:test".into(),
            name: "Agent".into(),
            card,
            draft: "draft survives edits".into(),
            hint: String::new(),
            sending: false,
            may_append: true,
            may_stop: true,
            recovered_drafts: Vec::new(),
            started: std::time::Instant::now(),
            sent_event: Some("$action".into()),
            sent_owner: Some("@requester:test".into()),
            pending_drafts: Vec::new(),
            retry: None,
        }
    }
    #[test]
    fn a_thread_summary_finds_the_newest_running_run_in_that_thread_only() {
        let room = matrix_sdk::ruma::room_id!("!summary:test");
        let run = |key: &str, root: &str, state: RunState| {
            let mut e = entry();
            e.key = key.into();
            e.card.room_id = room.as_str().into();
            e.card.thread_root = root.into();
            e.card.state = state;
            e
        };
        STORE.lock().unwrap().entries.extend([
            run("$old", "$root", RunState::Running),
            run("$new", "$root", RunState::Running),
            run("$done", "$root", RunState::Completed),
            run("$other", "$elsewhere", RunState::Running),
        ]);
        assert_eq!(newest_running_in_thread(room, "$root").map(|e| e.key).as_deref(), Some("$new"));
        assert!(newest_running_in_thread(room, "$quiet").is_none());
        STORE.lock().unwrap().entries.retain(|e| e.card.room_id != room.as_str());
    }
    #[test]
    fn reordered_edits_cannot_change_identity_or_reactivate_a_finished_run() {
        let mut e = entry();
        let mut update = e.card.clone();
        update.revision = 2;
        update.preview = "progress".into();
        assert!(apply(&mut e, update.clone()));
        assert_eq!(e.draft, "draft survives edits");
        update.revision = 1;
        assert!(!apply(&mut e, update.clone()));
        update.revision = 3;
        update.execution_id = "different".into();
        assert!(!apply(&mut e, update));
        let mut update = e.card.clone();
        update.revision = 4;
        update.state = RunState::Stopping;
        assert!(apply(&mut e, update.clone()));
        update.revision = 5;
        update.state = RunState::Running;
        assert!(!apply(&mut e, update.clone()));
        update.state = RunState::Interrupted;
        assert!(apply(&mut e, update.clone()));
        update.revision = 6;
        update.state = RunState::Running;
        assert!(!apply(&mut e, update));
        assert_eq!(e.card.state, RunState::Interrupted);
    }
    #[test]
    fn late_terminal_receipt_recovers_text_without_reopening_or_changing_the_run() {
        let mut e = entry();
        e.draft.clear();
        e.pending_drafts.push(PendingDraft {
            event: "$late".into(), owner: "@requester:test".into(), text: "keep this instruction".into(),
        });
        let mut card = e.card.clone();
        card.state = RunState::Completed;
        card.revision = 1;
        assert!(apply(&mut e, card.clone()));
        card.revision = 2;
        card.action_receipts.push(protocol::Receipt {
            event_id: "$late".into(), sender: "@requester:test".into(), outcome: "unavailable".into(),
        });
        assert!(apply(&mut e, card.clone()));
        assert_eq!(e.draft, "keep this instruction");
        assert_eq!(e.card.state, RunState::Completed);
        assert!(!apply(&mut e, card.clone()), "duplicate revision");
        card.revision = 3;
        card.state = RunState::Interrupted;
        assert!(!apply(&mut e, card.clone()), "the terminal outcome cannot change");
        card.state = RunState::Running;
        assert!(!apply(&mut e, card), "a receipt never reopens the run");
    }

    #[test]
    fn native_ack_changes_only_the_matching_action_and_not_completion() {
        let mut e = entry();
        let mut update = e.card.clone();
        update.revision = 1;
        update.last_action = Some(protocol::Receipt {
            event_id: "$someone-else".into(),
            sender: "@requester:test".into(),
            outcome: "accepted".into(),
        });
        assert!(apply(&mut e, update.clone()));
        assert!(e.hint.is_empty());
        update.revision = 2;
        update.last_action.as_mut().unwrap().event_id = "$action".into();
        assert!(apply(&mut e, update));
        assert_eq!(e.hint, "The Agent received this action.");
        assert_eq!(e.card.state, RunState::Running);
    }
    #[test]
    fn terminal_receipt_restores_each_unapplied_draft_and_checks_its_sender() {
        let mut e = entry();
        e.draft.clear();
        for (event, text) in [("$one", "first"), ("$two", "second"), ("$three", "third")] {
            e.pending_drafts.push(PendingDraft {
                event: event.into(), owner: "@requester:test".into(), text: text.into(),
            });
        }
        let mut card = e.card.clone();
        card.revision = 1;
        card.state = RunState::Completed;
        card.action_receipts = [("$one", "@requester:test", "unavailable"),
            ("$two", "@requester:test", "busy"), ("$three", "@other:test", "unavailable")]
            .into_iter().map(|(event, sender, outcome)| protocol::Receipt {
                event_id: event.into(), sender: sender.into(), outcome: outcome.into(),
            }).collect();
        assert!(apply(&mut e, card));
        assert_eq!(e.draft, "first");
        assert_eq!(e.recovered_drafts, ["second"]);
        assert_eq!(e.pending_drafts.len(), 1);
        assert_eq!(e.pending_drafts[0].event, "$three");
        assert!(!restore_next_draft(&mut e), "Do not replace an edited or restored draft");
        e.draft.clear();
        assert!(restore_next_draft(&mut e));
        assert_eq!(e.draft, "second");
    }
    #[test]
    fn completion_before_send_response_keeps_newer_edits_and_recovers_the_sent_text() {
        let mut e = entry();
        e.draft = "newer edit".into();
        e.card.state = RunState::Completed;
        e.card.action_receipts.push(protocol::Receipt {
            event_id: "$action".into(), sender: "@requester:test".into(), outcome: "unavailable".into(),
        });
        let sent = Sent {
            request: Requested {
                generation: 1, key: e.key.clone(), owner: "@requester:test".into(),
                agent: e.agent.clone(), room: e.card.room_id.clone(),
                txn: matrix_sdk::ruma::TransactionId::new(), kind: protocol::ACTION_EVENT,
                content: serde_json::json!({"action":{"kind":"steer","text":"sent text"}}),
                text: Some("sent text".into()),
            },
            result: Ok("$action".into()),
        };
        apply_send_result(&mut e, &sent);
        assert_eq!(e.draft, "newer edit");
        assert_eq!(e.recovered_drafts, ["sent text"]);
        assert!(e.pending_drafts.is_empty());
    }
    #[test]
    fn unknown_application_is_not_restored_as_an_unapplied_draft() {
        let mut e = entry();
        e.draft.clear();
        e.pending_drafts.push(PendingDraft {
            event: "$action".into(), owner: "@requester:test".into(), text: "possibly applied".into(),
        });
        let mut card = e.card.clone();
        card.revision = 1;
        card.state = RunState::Unknown;
        card.action_receipts.push(protocol::Receipt {
            event_id: "$action".into(), sender: "@requester:test".into(), outcome: "unknown".into(),
        });
        assert!(apply(&mut e, card));
        assert!(e.draft.is_empty() && e.recovered_drafts.is_empty());
        assert_eq!(e.pending_drafts.len(), 1);
        assert_eq!(e.unconfirmed_text().as_deref(), Some("possibly applied"));
    }
    #[test]
    fn terminal_without_exact_receipt_exposes_unknown_text_for_both_delivery_orders() {
        for completion_first in [false, true] {
            let mut e = entry();
            e.draft = "an unsent newer edit".into();
            let mut terminal = e.card.clone();
            terminal.revision = 1;
            terminal.state = RunState::Completed;
            if completion_first { assert!(apply(&mut e, terminal.clone())); }
            let sent = Sent {
                request: Requested {
                    generation: 1, key: e.key.clone(), owner: "@requester:test".into(),
                    agent: e.agent.clone(), room: e.card.room_id.clone(),
                    txn: matrix_sdk::ruma::TransactionId::new(), kind: protocol::ACTION_EVENT,
                    content: serde_json::json!({"action":{"kind":"steer","text":"exact sent text"}}),
                    text: Some("exact sent text".into()),
                }, result: Ok("$missing-receipt".into()),
            };
            apply_send_result(&mut e, &sent);
            if !completion_first { assert!(apply(&mut e, terminal.clone())); }
            assert!(e.hint.contains("result is unconfirmed"));
            assert!(!e.hint.contains("Waiting"));
            assert_eq!(e.unconfirmed_text().as_deref(), Some("exact sent text"));
            assert_eq!(e.draft, "an unsent newer edit");
            assert!(e.recovered_drafts.is_empty() && e.retry.is_none());
            terminal.revision = 2;
            terminal.action_receipts.push(protocol::Receipt {
                event_id: "$missing-receipt".into(), sender: "@requester:test".into(), outcome: "accepted".into(),
            });
            assert!(apply(&mut e, terminal));
            assert!(e.unconfirmed_text().is_none());
            assert_eq!(e.draft, "an unsent newer edit");
        }
    }

    #[test]
    fn claude_input_joins_the_running_turn_and_waits_for_its_receipt_like_codex() {
        // The entry below sits in the shared store until `request` has read it.
        let _guard = super::super::pending_questions::test_serial();
        for runtime in ["codex", "claude", "octos"] {
            let mut e = entry();
            e.key = format!("$card-{runtime}");
            e.card.runtime = runtime.into();
            e.draft = "also check the tests".into();
            (e.sent_event, e.sent_owner) = (None, None);
            {
                let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
                store.owner = "@requester:test".into();
                store.entries.push(e.clone());
            }
            let request = request(&e.key, false).unwrap();
            STORE.lock().unwrap_or_else(|e| e.into_inner()).entries.retain(|entry| entry.key != e.key);
            // One per-run action bound to this card: not a separate room message.
            assert_eq!(request.kind, protocol::ACTION_EVENT, "{runtime}");
            assert!(matches!(
                e.card.receive(&e.key, "@requester:test", &request.content),
                Some(Action::Steer { text }) if text == "also check the tests"
            ), "{runtime}");
            // Its receipt is awaited, and text the run did not take comes back.
            apply_send_result(&mut e, &Sent { request, result: Ok("$steer".into()) });
            assert_eq!(e.hint, "Sent. Waiting for the Agent to confirm.");
            assert!(e.draft.is_empty());
            assert_eq!(e.pending_drafts.len(), 1);
            let mut accepted = e.clone();
            let mut card = e.card.clone();
            card.revision = 1;
            card.action_receipts.push(protocol::Receipt {
                event_id: "$steer".into(), sender: "@requester:test".into(), outcome: "accepted".into(),
            });
            assert!(apply(&mut accepted, card.clone()));
            assert_eq!(accepted.hint, "The Agent received this action.");
            assert!(accepted.pending_drafts.is_empty() && accepted.draft.is_empty());
            card.action_receipts[0].outcome = "unavailable".into();
            assert!(apply(&mut e, card));
            assert_eq!(e.draft, "also check the tests", "{runtime}");
            assert!(e.pending_drafts.is_empty());
        }
    }
    #[test]
    fn stop_all_running_requests_an_interrupt_only_for_runs_the_owner_may_stop() {
        reset(7, "@owner:test");
        let mut a = entry(); a.key = "$a".into();
        let mut b = entry(); b.key = "$b".into(); b.may_stop = false;
        let mut c = entry(); c.key = "$c".into(); c.card.state = RunState::Interrupted;
        STORE.lock().unwrap().entries = vec![a, b, c];
        let keys: Vec<String> = STORE.lock().unwrap().entries.iter()
            .filter(|e| e.may_stop && e.card.state == RunState::Running).map(|e| e.key.clone()).collect();
        assert_eq!(keys, vec!["$a".to_owned()]);
        let request = request("$a", true).expect("an interrupt for the running run");
        assert!(request.text.is_none(), "an interrupt carries no instructions");
        assert!(STORE.lock().unwrap().entries[0].sending, "the card shows Sending… like its own Stop button");
    }
}
