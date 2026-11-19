//! e2e scenario 8 (Nov 18, 18:07): in an Off room, Anna (not bird-codex's
//! owner) asks bird-codex to review a PR on the room's linked repository and
//! post the review. Nothing lets the verdict post card-free, so it must ask
//! the owner on a card, exactly as bird-claude's `Bash` call does, and run
//! the pinned post once approved; never be refused outright.
use super::*;
use crate::codex::repo_steps::{Call, Running, Turn, start};
use crate::watch_trust::fix_flow_tests::{OWNER, REPO};
use crate::{Scope, ToolPolicy};
use tokio::sync::oneshot;

const ROOM: &str = "!e2e-room-card:example.org";
const ANNA: &str = "@zz-anna:example.org";
const HEAD: &str = "4987f0d18ef940fc5ebf9a1cf53df384a9a6af0c";

/// bird-codex's ledger under `high_risk` (the e2e room asked: AskOwner),
/// watching nothing, the room Off; the host recorded `REPO` as linked.
fn ledger_under(dir: &std::path::Path, high_risk: ToolPolicy) -> (Ledger, Scope) {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    let scope = Scope { agent: "bird-codex".into(), binding: "b".into(), room: ROOM.into(), requester: ANNA.into(), thread: "t".into() };
    ledger.register_binding(OWNER, &scope).unwrap();
    let policy = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk,
    };
    for layer in [crate::Layer::Agent, crate::Layer::Room, crate::Layer::Requester] {
        ledger.set_policy(OWNER, &scope, layer, 0, &policy).unwrap();
    }
    ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
    crate::watch_trust::room_scope::record(ROOM, ANNA, vec![REPO.into()]);
    (ledger, scope)
}

fn verdict() -> String {
    verdict_as("--comment", REPO)
}
fn verdict_as(mode: &str, repo: &str) -> String {
    format!("gh pr review 107 --repo {repo} {mode} -b \"Hagency verdict: APPROVE {HEAD} - Single-line e2e fixture is in scope; no issues found. On request from {ANNA}\"")
}

/// Send `command` as a `watched_repo_step` call; the answer comes later.
async fn send(ledger: &mut Ledger, turn: &Turn<'_>, command: &str, running: &mut Running) -> oneshot::Receiver<(String, bool)> {
    let (reply, answer) = oneshot::channel();
    start(ledger, turn, Call { command: command.into(), reply }, running).await;
    answer
}

/// The turn loop's part: take the card's answer and act on it.
async fn answer_card(ledger: &mut Ledger, turn: &Turn<'_>, running: &mut Running) {
    let decided_card = next(running).await.expect("a card waits");
    decided(ledger, turn, decided_card, running);
}

#[tokio::test]
async fn a_members_review_verdict_asks_the_owner_and_runs_the_pinned_post_once_approved() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger_under(db.path(), ToolPolicy::AskOwner);
    let (queue, mut cards) = crate::codex::approval_queue(4).unwrap();
    let workspace = db.path().to_string_lossy().into_owned();
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true, queue: Some(&queue) };
    let mut running = Running::default();
    let mut answer = send(&mut ledger, &turn, &verdict(), &mut running).await;
    assert!(answer.try_recv().is_err(), "not refused: it waits for the owner");
    let card = cards.recv().await.expect("the owner gets a card");
    assert_eq!((card.proposal.tool.as_str(), card.proposal.scope.requester.as_str()), (crate::watch_trust::codex_step::TOOL, ANNA));
    assert_eq!(card.proposal.arguments["command"], verdict(), "the card shows the exact command");
    card.decide(true);
    answer_card(&mut ledger, &turn, &mut running).await;
    let (out, failed) = answer.await.unwrap();
    // It ran: with no GitHub token in this test, the pinned `gh` step stops
    // at the token check, after the owner's approval was spent.
    assert!(failed && out.contains(crate::watch_trust::step_env::NO_TOKEN) && !out.contains("not_card_free"), "{out}");
    let consumed: i64 = ledger.db.query_row("SELECT count(*) FROM approvals WHERE consumed=1", [], |r| r.get(0)).unwrap();
    assert_eq!(consumed, 1, "the owner's approval is recorded and spent once");
}

#[tokio::test]
async fn a_declined_card_refuses_and_runs_nothing() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger_under(db.path(), ToolPolicy::AskOwner);
    let (queue, mut cards) = crate::codex::approval_queue(4).unwrap();
    let workspace = db.path().to_string_lossy().into_owned();
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true, queue: Some(&queue) };
    let mut running = Running::default();
    let answer = send(&mut ledger, &turn, &verdict(), &mut running).await;
    cards.recv().await.unwrap().decide(false);
    answer_card(&mut ledger, &turn, &mut running).await;
    let (out, failed) = answer.await.unwrap();
    assert!(failed && out.contains("did not approve"), "{out}");
    let approvals: i64 = ledger.db.query_row("SELECT count(*) FROM approvals", [], |r| r.get(0)).unwrap();
    assert_eq!(approvals, 0, "nothing approved");
}

#[tokio::test]
async fn deny_or_a_non_verdict_write_is_refused_without_a_card() {
    let db = tempfile::tempdir().unwrap();
    let workspace = db.path().to_string_lossy().into_owned();
    let (queue, mut cards) = crate::codex::approval_queue(4).unwrap();
    let (mut ledger, scope) = ledger_under(db.path(), ToolPolicy::Deny);
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true, queue: Some(&queue) };
    let mut running = Running::default();
    let (out, failed) = send(&mut ledger, &turn, &verdict(), &mut running).await.await.unwrap();
    assert!(failed && out.contains("rules refuse"), "{out}");
    let other = tempfile::tempdir().unwrap();
    let (mut asks, scope) = ledger_under(other.path(), ToolPolicy::AskOwner);
    let turn = Turn { scope: &scope, ..turn };
    let (out, failed) = send(&mut asks, &turn, &format!("gh pr comment 107 --repo {REPO} -b hi"), &mut running).await.await.unwrap();
    assert!(failed && out.contains("not_card_free"), "{out}");
    assert!(cards.try_recv().is_err(), "no card for either");
}

/// GPT review: a card's request for changes holds the withdrawal of this
/// Agent's earlier approval under its step, settled with what ran (here the
/// step fails: no token, so the approval stays); a repository outside the
/// room never asks.
#[tokio::test]
async fn a_carded_request_for_changes_holds_its_withdrawal_and_other_repos_are_refused() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger_under(db.path(), ToolPolicy::AskOwner);
    let url = format!("https://github.com/{REPO}/pull/107");
    ledger.record_agent_review(&url, HEAD, "bird-codex", ROOM).unwrap();
    let (queue, mut cards) = crate::codex::approval_queue(4).unwrap();
    let workspace = db.path().to_string_lossy().into_owned();
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true, queue: Some(&queue) };
    let mut running = Running::default();
    let (out, failed) = send(&mut ledger, &turn, &verdict_as("--comment", "other/unlinked"), &mut running).await.await.unwrap();
    assert!(failed && out.contains("not one of this room's"), "{out}");
    assert!(cards.try_recv().is_err(), "no card for another repository");
    let answer = send(&mut ledger, &turn, &verdict_as("--request-changes", REPO), &mut running).await;
    cards.recv().await.unwrap().decide(true);
    answer_card(&mut ledger, &turn, &mut running).await;
    let held: i64 = ledger.db.query_row("SELECT count(*) FROM auto_merge_pending_withdrawals WHERE pr=?", [&url], |r| r.get(0)).unwrap();
    assert_eq!(held, 1, "the withdrawal waits for the step");
    let (out, failed) = answer.await.unwrap();
    assert!(failed && out.contains(crate::watch_trust::step_env::NO_TOKEN), "{out}");
    running.settle(&mut ledger).unwrap();
    assert_eq!(ledger.agent_reviewers(&url, HEAD).unwrap(), ["bird-codex"], "a failed post withdraws nothing");
}

/// The waits nest: the card inside the proposal's lifetime, the step after
/// it inside the Room MCP wait, and Codex's own call timeout past both.
#[test]
fn the_card_fits_the_proposal_and_the_call_waits() {
    assert!((CARD_WAIT.as_secs() as i64) < crate::codex::repo_steps::PROPOSAL_LIFETIME_SECS);
    let step = crate::watch_trust::codex_step::STEP_TIMEOUT;
    assert!(crate::codex::room_mcp::REPO_STEP_WAIT > CARD_WAIT + step);
    assert!(crate::codex::room_mcp::TOOL_TIMEOUT_SECS > crate::codex::room_mcp::REPO_STEP_WAIT.as_secs());
    // GPT review: the connection outlives the call, never the other way round.
    assert!(crate::codex::room_mcp::CONNECTION_LIFETIME.as_secs() > crate::codex::room_mcp::TOOL_TIMEOUT_SECS);
}
