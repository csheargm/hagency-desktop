//! The owner's flow, with the Agent runtime and the server faked by their
//! answers: the Agent asks → the window lists it wherever the owner is →
//! the owner approves → the decision is routed to the waiting runtime; and
//! a request nobody answered stays listed as expired.
use super::*;
use crate::hagency::local_approvals;
use crate::hagency::pending_approvals::{PendingApprovalDecided, PendingApprovalsUpdate, TEST_TURN};
use ::hagency::native_owner::{Command, LocalAction};
use serde_json::{Value, json};

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const THREAD: &str = "$handoff";

/// The runtime's `ApprovalView` of bird-claude's first shell step in the
/// dev room (a room on another server than the owner's).
fn runtime_view(proposal: &str, command: &str, expires_s: u64) -> Value {
    let folder = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap().display().to_string();
    json!({"proposalId": proposal, "argsDigest": "a".repeat(64), "proposal": {
        "scope": {"agent": "agt_bird", "binding": "bnd", "room": ROOM, "requester": "@bird:localhost:8452", "thread": THREAD},
        "dispatch": "evt", "tool": "claude.Bash", "arguments": {"command": command},
        "canonical_directory": folder, "risk": "high", "policy_revision": [1, 0, 0], "expires": expires_s}})
}

/// What the app-wide poll posts: the management answer (here: the owner's
/// server has no requests) with this app's own Agent's queue appended.
fn poll_answer(views: &[Value]) -> PendingApprovalsUpdate {
    let rows: Vec<Value> = views.iter().filter_map(|v| local_approvals::to_row(v, "bird-claude")).collect();
    PendingApprovalsUpdate { identity: "@bird:localhost:8452".into(), result: Ok(json!({"approvals": rows})) }
}

fn inbox(cx: &mut Cx) -> OwnerApprovalsInbox {
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.OwnerApprovalsInbox {}});
        let inbox = OwnerApprovalsInbox::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty(), "the inbox's script must evaluate cleanly");
        inbox
    })
}

fn now_s() -> u64 {
    now_ms() / 1000
}

#[test]
fn a_request_from_a_federated_room_reaches_the_inbox_and_the_approval_reaches_the_runtime() {
    let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
    pending_approvals::forget_identity();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut inbox = inbox(&mut cx);
    inbox.refresh(&mut cx);
    assert!(!inbox.view.visible(), "nothing waits: the inbox stays out of the way");

    // 1. The Agent asks; the next poll reads it.
    let view = runtime_view("p1", "git -C repo fetch origin; cat repo/README.md", now_s() + 280);
    assert!(pending_approvals::apply(&mut cx, &poll_answer(std::slice::from_ref(&view))));
    assert_eq!(pending_approvals::total(), 1, "the Agents rail counts it");
    assert_eq!(pending_approvals::count_for_agent("agt_bird"), 1, "so does the Agent's card");

    // 2. The window shows it, whatever room is open.
    inbox.refresh(&mut cx);
    assert!(inbox.view.visible());
    assert_eq!(inbox.view.label(&mut cx, ids!(card.header)).text(), "1 approval waiting for you");
    let row = inbox.view.view(&mut cx, ids!(row0));
    assert!(row.label(&mut cx, ids!(title)).text().contains("bird-claude wants to use Bash"));
    assert!(row.label(&mut cx, ids!(details)).text().contains("git -C repo fetch origin"), "the exact command");
    assert!(row.label(&mut cx, ids!(place)).text().contains(ROOM), "which room asked");
    for (id, shown) in [(ids!(once), true), (ids!(deny), true), (ids!(open_room), true), (ids!(dismiss), false)] {
        assert_eq!(row.button(&mut cx, id).visible(), shown);
    }
    assert!(!inbox.view.view(&mut cx, ids!(row1)).visible());
    // The thread's own timeline shows the card on its root message too: this
    // app's Agent names only its thread, never a message of its own.
    let room = matrix_sdk::ruma::RoomId::parse(ROOM).unwrap();
    assert!(pending_approvals::for_event(&room, THREAD, false).is_some());
    assert!(pending_approvals::for_event(&room, THREAD, true).is_some());

    // 3. The owner approves once from the inbox.
    let row_value = pending_approvals::waiting()[0].row.clone();
    let Some(Command::Local { agent_id, action: LocalAction::Decision { approval_id }, input, .. }) =
        local_approvals::decision_command(&row_value, "approve_once") else { panic!("not routed to the runtime") };
    assert_eq!((agent_id.as_str(), approval_id.as_str()), ("agt_bird", "p1"));
    assert_eq!(input.unwrap()["approved"], true);
    assert!(inbox.decide(&mut cx, 0, "approve_once"));
    assert_eq!(pending_approvals::local_decision(row_value["approval"]["id"].as_str().unwrap()), LocalDecision::Sending("approve_once".into()));
    assert_eq!(pending_approvals::total(), 0, "an answered request no longer counts");
    assert!(!inbox.decide(&mut cx, 0, "deny"), "answered once is enough");

    // 4. The runtime took it; the Agent continues and the request leaves the list.
    let id = row_value["approval"]["id"].as_str().unwrap().to_owned();
    pending_approvals::apply_decided(&PendingApprovalDecided { id: id.clone(), decision: "approve_once".into(), result: Ok(()) });
    inbox.refresh(&mut cx);
    assert!(inbox.view.view(&mut cx, ids!(row0)).label(&mut cx, ids!(status)).text().contains("Approved once"));
    pending_approvals::apply(&mut cx, &poll_answer(&[]));
    inbox.refresh(&mut cx);
    assert!(expired_approvals::listed(now_ms()).is_empty(), "an answered request is not recorded as expired");
    assert!(!inbox.view.visible(), "nothing left to show");
    pending_approvals::forget_identity();
}

#[test]
fn a_request_nobody_answered_stays_listed_as_expired_and_cannot_be_approved() {
    let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
    pending_approvals::forget_identity();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut inbox = inbox(&mut cx);
    // Read for the last time five seconds before it ends.
    let view = runtime_view("p2", "git fetch origin", now_s() + 5);
    assert!(pending_approvals::apply(&mut cx, &poll_answer(std::slice::from_ref(&view))));
    // The next reading no longer lists it: it expired unanswered.
    assert!(pending_approvals::apply(&mut cx, &poll_answer(&[])), "the inbox redraws for the expiry");
    let expired = expired_approvals::listed(now_ms());
    assert_eq!(expired.len(), 1);
    inbox.refresh(&mut cx);
    assert!(inbox.view.visible());
    let row = inbox.view.view(&mut cx, ids!(row0));
    assert!(row.label(&mut cx, ids!(title)).text().starts_with("Expired, not answered: bird-claude wants to use Bash"));
    assert!(!row.button(&mut cx, ids!(once)).visible() && row.button(&mut cx, ids!(dismiss)).visible());
    // The card on the thread root keeps saying so, and cannot be answered.
    let room = matrix_sdk::ruma::RoomId::parse(ROOM).unwrap();
    let card = pending_approvals::for_event(&room, THREAD, true).expect("the card keeps its record");
    assert!(card.expires_at_ms().is_some_and(|t| t <= now_ms()));
    assert!(!pending_approvals::decide(&card.row, "approve_once"));
    // A failed reading is said, not swallowed.
    assert!(pending_approvals::apply(&mut cx, &PendingApprovalsUpdate { identity: "@bird:localhost:8452".into(), result: Err("approvals_read_timeout".into()) }));
    inbox.refresh(&mut cx);
    assert!(inbox.view.label(&mut cx, ids!(card.header)).text().contains("Could not check for approvals (approvals_read_timeout)"));
    // Dismissed from the inbox; a good reading clears the error.
    expired_approvals::dismiss(expired[0].id());
    pending_approvals::apply(&mut cx, &poll_answer(&[]));
    inbox.refresh(&mut cx);
    assert!(!inbox.view.visible());
    pending_approvals::forget_identity();
}

#[test]
fn an_unreadable_agent_queue_is_named_in_the_inbox() {
    let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
    pending_approvals::forget_identity();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut inbox = inbox(&mut cx);
    let update = PendingApprovalsUpdate { identity: "@bird:localhost:8452".into(), result: Ok(json!({"approvals": [],
        "unreadableAgents": [{"agent": "bird-claude", "code": "local_state_unavailable"}]})) };
    assert!(pending_approvals::apply(&mut cx, &update));
    inbox.refresh(&mut cx);
    assert!(inbox.view.visible());
    assert!(inbox.view.label(&mut cx, ids!(card.header)).text().contains("Could not read bird-claude's requests (local_state_unavailable)"));
    pending_approvals::forget_identity();
}
