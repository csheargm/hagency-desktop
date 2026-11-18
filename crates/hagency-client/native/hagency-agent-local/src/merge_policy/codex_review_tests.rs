//! Live report (Nov 17, PR #36): a Codex reviewer posted "Hagency verdict:
//! APPROVE <head>", yet the merge gate kept saying "needs 1 approval(s) from
//! another Agent, has 0". A Codex command approval now reaches the same gate
//! as a Claude `Bash` call, under the same rules.
use super::*;
use crate::merge_policy::gate::APPROVE_MARKER;
use crate::merge_policy::tests::{bash, file, ledger_with, merge_line, Fake, BASE, HEAD, URL};
use serde_json::json;
use std::path::Path;

const OWNER: &str = "@owner:example.org";
const THREAD: &str = "thread-a";
const TURN: &str = "turn-a";

/// The proposal `codex.rs` builds for `item/commandExecution/requestApproval`:
/// the request's params as arguments, the command as one shell line.
fn codex(line: &str, dir: &Path, agent: &str, item: &str) -> ToolProposal {
    let mut proposal = bash(line, dir, agent);
    proposal.tool = "codex.command".into();
    proposal.arguments = json!({"threadId": THREAD, "turnId": TURN, "itemId": item, "command": line, "cwd": dir, "startedAtMs": 1});
    proposal
}
fn review_line(head: &str) -> String {
    format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {head} - docs only\"")
}
fn done(item: &str, status: &str, exit: Value) -> Value {
    json!({"id": item, "type": "commandExecution", "status": status, "exitCode": exit})
}
async fn ask(ledger: &mut Ledger, proposal: &ToolProposal, source: &Fake) -> Option<bool> {
    auto_answer(ledger, OWNER, COMMAND_APPROVAL, proposal, (THREAD, TURN), 11, source).await
}
fn setup(dir: &Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["fixer", "bird-codex"]);
    for agent in ["fixer", "bird-codex"] {
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    ledger
}

#[tokio::test]
async fn a_codex_reviewers_approval_of_the_head_lets_another_agent_merge() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    let merge = bash(&merge_line(), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "m0", 11, &docs).await, None, "no approval yet: the card asks");
    let review = codex(&review_line(HEAD), temp.path(), "bird-codex", "call_review");
    assert_eq!(ask(&mut ledger, &review, &docs).await, Some(true), "answered without a card, as for Claude");
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "nothing counts before the command completes");
    settle(&mut ledger, THREAD, TURN, &done("call_review", "completed", json!(0))).unwrap();
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["bird-codex"]);
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "m1", 11, &docs).await, Some(true), "the merge gate passes");
}

#[tokio::test]
async fn a_failed_declined_or_foreign_completion_never_counts() {
    for (thread, turn, item) in [
        (THREAD, TURN, done("call_review", "failed", json!(1))),
        (THREAD, TURN, done("call_review", "declined", Value::Null)),
        (THREAD, TURN, done("call_review", "completed", json!(2))),
        (THREAD, TURN, json!({"id": "call_review", "type": "agentMessage", "text": "done"})),
        (THREAD, "turn-b", done("call_review", "completed", json!(0))),
        ("thread-b", TURN, done("call_review", "completed", json!(0))),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = setup(temp.path());
        let docs = Fake::new(vec![file("README.md", 3)]);
        assert_eq!(ask(&mut ledger, &codex(&review_line(HEAD), temp.path(), "bird-codex", "call_review"), &docs).await, Some(true));
        settle(&mut ledger, thread, turn, &item).unwrap();
        assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "{thread} {turn} {item}");
    }
}

#[tokio::test]
async fn an_approval_of_an_older_head_does_not_count() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    let stale = codex(&review_line(BASE), temp.path(), "bird-codex", "call_stale");
    assert_eq!(ask(&mut ledger, &stale, &docs).await, None, "it names another head: the card asks");
    settle(&mut ledger, THREAD, TURN, &done("call_stale", "completed", json!(0))).unwrap();
    assert!(ledger.agent_reviewers(URL, BASE).unwrap().is_empty() && ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
    // Approved at the head, which then moves: the merge of the new head needs a new approval.
    assert_eq!(ask(&mut ledger, &codex(&review_line(HEAD), temp.path(), "bird-codex", "call_ok"), &docs).await, Some(true));
    settle(&mut ledger, THREAD, TURN, &done("call_ok", "completed", json!(0))).unwrap();
    let moved = "3333333333333333333333333333333333333333";
    *docs.head.borrow_mut() = moved.into();
    let merge = bash(&merge_line().replace(HEAD, moved), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "m", 11, &docs).await, None, "a new push voids the approval");
}

#[tokio::test]
async fn the_author_agent_cannot_approve_its_own_pull_request() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    // A Codex fixer approves its own pull request, then asks to merge it.
    assert_eq!(ask(&mut ledger, &codex(&review_line(HEAD), temp.path(), "fixer", "call_self"), &docs).await, Some(true));
    settle(&mut ledger, THREAD, TURN, &done("call_self", "completed", json!(0))).unwrap();
    let merge = codex(&merge_line(), temp.path(), "fixer", "call_merge");
    assert_eq!(ask(&mut ledger, &merge, &docs).await, None, "its own approval does not count: the card asks");
}

#[tokio::test]
async fn another_room_requester_or_owner_cannot_record_an_approval() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    let review = codex(&review_line(HEAD), temp.path(), "bird-codex", "call_review");
    let mut other_room = review.clone();
    other_room.scope.room = "!other:x".into();
    let mut stranger = review.clone();
    stranger.scope.requester = "@mallory:example.org".into();
    for proposal in [other_room, stranger] {
        assert_ne!(ask(&mut ledger, &proposal, &docs).await, Some(true), "{:?}", proposal.scope);
        settle(&mut ledger, THREAD, TURN, &done("call_review", "completed", json!(0))).unwrap();
    }
    let not_owner = auto_answer(&mut ledger, "@mallory:example.org", COMMAND_APPROVAL, &review, (THREAD, TURN), 11, &docs).await;
    assert_ne!(not_owner, Some(true), "only the owner's own approval answers the card");
    settle(&mut ledger, THREAD, TURN, &done("call_review", "completed", json!(0))).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}

#[tokio::test]
async fn only_a_codex_shell_command_reaches_the_gate() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    let review = codex(&review_line(HEAD), temp.path(), "bird-codex", "call_review");
    let mut network = review.clone();
    network.tool = "codex.network".into();
    assert_eq!(ask(&mut ledger, &network, &docs).await, None);
    let file_change = auto_answer(&mut ledger, OWNER, "item/fileChange/requestApproval", &review, (THREAD, TURN), 11, &docs).await;
    assert_eq!(file_change, None);
    // Without an item id the review is still judged, but can never be settled.
    let mut unnamed = review.clone();
    unnamed.arguments["itemId"] = Value::Null;
    assert_eq!(ask(&mut ledger, &unnamed, &docs).await, Some(true));
    settle(&mut ledger, THREAD, TURN, &json!({"id": "", "type": "commandExecution", "status": "completed", "exitCode": 0})).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}
