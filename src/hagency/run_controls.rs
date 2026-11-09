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
    sent_event: Option<String>,
    sent_owner: Option<String>,
    sent_queued: bool,
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
    queued: bool,
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
            "accepted" => "Agent 已接收此操作。",
            "queued" => "Agent 已将指令加入下一轮。",
            "rejected" => "运行时拒绝了此操作。",
            "unsupported" => "当前运行时不支持此操作。",
            "in_progress" => "停止请求已在处理中。",
            "busy" => "操作队列繁忙，请稍后重新提交。",
            "unavailable" => "本次运行已结束，操作未执行。",
            _ => "未能确认操作结果；为避免重复执行，不会自动重发。",
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
        entry.hint = "有指令未执行，原文已保留。可复制后发送，或在运行中重新提交。".into();
    } else if !card.state.active() && entry.sent_event.is_some() && !entry.sent_queued && !last_acknowledged {
        entry.hint = "运行已结束，操作结果未确认。请先核对；不会自动重发。".into();
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
        sent_queued: false,
        pending_drafts: Vec::new(),
        recovered_drafts: Vec::new(),
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
    entries.sort_by_key(|e| !e.card.state.active());
    entries.truncate(3);
    entries
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
        entry.hint = "请先处理尚未确认或未执行的指令。".into();
        return None;
    }
    let action = if stop {
        Action::Interrupt {}
    } else if entry.card.runtime == "claude" {
        Action::Queue {
            text: entry.draft.clone(),
        }
    } else {
        Action::Steer {
            text: entry.draft.clone(),
        }
    };
    let queued = matches!(&action, Action::Queue { .. });
    let content = if let Action::Queue { text } = &action {
        entry
            .card
            .queued_message(key, &owner, &entry.agent, text)
            .ok()?
    } else {
        entry.card.action_content(key, &owner, action).ok()?
    };
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
            kind: if queued {
                "m.room.message"
            } else {
                protocol::ACTION_EVENT
            },
            content,
            queued,
            text: (!stop).then(|| entry.draft.clone()),
        });
    entry.sending = true;
    entry.hint = "正在发送…".into();
    Some(request)
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
            entry.sent_queued = value.request.queued;
            entry.retry = None;
            entry.hint = if value.request.queued {
                "已发送，等待下一轮处理。"
            } else {
                "已发送，等待 Agent 确认。"
            }
            .into();
            if let Some(text) = &value.request.text {
                if entry.draft == *text { entry.draft.clear(); }
                if !value.request.queued && !entry.pending_drafts.iter().any(|draft| draft.event == *event) {
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
            entry.hint = "发送未获确认，可重试同一操作。".into();
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

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
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
            sent_event: Some("$action".into()),
            sent_owner: Some("@requester:test".into()),
            sent_queued: false,
            pending_drafts: Vec::new(),
            retry: None,
        }
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
        assert_eq!(e.hint, "Agent 已接收此操作。");
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
                queued: false, text: Some("sent text".into()),
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
                    queued: false, text: Some("exact sent text".into()),
                }, result: Ok("$missing-receipt".into()),
            };
            apply_send_result(&mut e, &sent);
            if !completion_first { assert!(apply(&mut e, terminal.clone())); }
            assert!(e.hint.contains("结果未确认"));
            assert!(!e.hint.contains("等待"));
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
    fn a_queued_chat_message_does_not_wait_for_a_per_run_control_receipt() {
        let mut e = entry();
        e.draft = "next turn".into();
        let sent = Sent {
            request: Requested {
                generation: 1, key: e.key.clone(), owner: "@requester:test".into(),
                agent: e.agent.clone(), room: e.card.room_id.clone(),
                txn: matrix_sdk::ruma::TransactionId::new(), kind: "m.room.message",
                content: serde_json::json!({"body":"next turn"}),
                queued: true, text: Some("next turn".into()),
            },
            result: Ok("$queued".into()),
        };
        apply_send_result(&mut e, &sent);
        assert!(e.draft.is_empty() && e.pending_drafts.is_empty());
        let mut card = e.card.clone();
        card.revision = 1;
        card.state = RunState::Completed;
        assert!(apply(&mut e, card));
        assert!(e.recovered_drafts.is_empty());
        assert!(e.unconfirmed_text().is_none());
        assert_eq!(e.hint, "已发送，等待下一轮处理。");
    }
}
