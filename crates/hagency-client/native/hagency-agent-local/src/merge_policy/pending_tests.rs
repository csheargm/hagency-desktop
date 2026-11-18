//! Integration GPT review (Nov 17): a request for changes withdraws approvals
//! only once it has run, and the one automatic merge is given back when the
//! merge it was taken for never happened.
use super::*;
use crate::ToolProposal;
use crate::merge_policy::codex_review::{self, call_key, COMMAND_APPROVAL};
use crate::merge_policy::gate::{self, APPROVE_MARKER};
use crate::merge_policy::tests::{bash, file, ledger_with, merge_line, Fake, HEAD, URL};
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;

const OWNER: &str = "@owner:example.org";
const CHANGES: &str = "gh pr review 7 --request-changes -b \"Hagency verdict: CHANGES - no\"";

fn setup(dir: &Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["fixer", "reviewer"]);
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    ledger
}
async fn approve(ledger: &mut Ledger, dir: &Path, docs: &Fake, call: &str) {
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {} - {call}\"", docs.head.borrow()), dir, "reviewer");
    assert_eq!(gate::auto_answer(ledger, OWNER, &review, call, 11, docs).await, Some(true));
    ledger.settle_tool_call(call, true).unwrap();
}

#[tokio::test]
async fn a_request_for_changes_withdraws_approvals_only_once_it_ran() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 2)]);
    approve(&mut ledger, temp.path(), &docs, "rv").await;
    let changes = bash(CHANGES, temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &changes, "rc", 11, &docs).await, Some(true));
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["reviewer"], "asked is not done");
    ledger.settle_tool_call("rc", false).unwrap();
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["reviewer"], "a failed or denied request withdraws nothing");
    // The owner's card answers it (switch off), and it runs: withdrawn.
    ledger.set_auto_merge(OWNER, "reviewer", false).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &changes, "rc2", 11, &docs).await, None);
    ledger.settle_tool_call("rc2", true).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&merge_line(), temp.path(), "fixer"), "mg", 11, &docs).await, None);
}

#[tokio::test]
async fn an_unsettled_approval_stops_at_once_when_changes_are_asked() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 2)]);
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""), temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &review, "held", 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(CHANGES, temp.path(), "reviewer"), "rc", 11, &docs).await, Some(true));
    ledger.settle_tool_call("rc", false).unwrap();
    ledger.settle_tool_call("held", true).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "the reviewer's latest word is not an approval");
}

/// Codex: a declined request withdraws nothing; one the owner's card let run
/// (the gate never sent it) withdraws once it completes with exit 0.
#[tokio::test]
async fn a_codex_request_for_changes_withdraws_once_its_command_ran() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 2)]);
    approve(&mut ledger, temp.path(), &docs, "rv").await;
    ledger.set_auto_merge(OWNER, "reviewer", false).unwrap();
    for (item, status, exit, kept) in [("c1", "declined", json!(null), true), ("c2", "completed", json!(0), false)] {
        let mut proposal = bash(CHANGES, temp.path(), "reviewer");
        proposal.tool = "codex.command".into();
        proposal.arguments = json!({"itemId": item, "command": CHANGES, "cwd": temp.path()});
        assert_eq!(codex_review::auto_answer(&mut ledger, OWNER, COMMAND_APPROVAL, &proposal, ("t", "u"), 11, &docs).await, None, "the card asks");
        let done = json!({"id": item, "type": "commandExecution", "status": status, "exitCode": exit});
        codex_review::settle(&mut ledger, ("t", "u"), &done, &mut HashSet::new()).unwrap();
        assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap().len(), usize::from(kept), "{item}");
    }
    assert!(call_key("t", "u", "c2").starts_with("codex:"));
}

#[tokio::test]
async fn a_merge_that_did_not_happen_gives_its_claim_back() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 2)]);
    approve(&mut ledger, temp.path(), &docs, "rv").await;
    // Each attempt is its own call (an authorization is single-use).
    let attempt = |n: i64| ToolProposal { expires: 100 + n, ..bash(&merge_line(), temp.path(), "fixer") };
    let merge = attempt(0);
    // Not authorized (not the owner's approval): the claim is given back at once.
    assert_eq!(gate::auto_answer(&mut ledger, "@mallory:example.org", &merge, "m0", 11, &docs).await, Some(false));
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "m1", 11, &docs).await, Some(true));
    ledger.settle_tool_call("m1", false).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &attempt(2), "m2", 11, &docs).await, Some(true), "GitHub refused it: the merge is still available");
    ledger.settle_tool_call("m2", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &attempt(3), "m3", 11, &docs).await, None, "one automatic merge per head");
    // An unreported outcome keeps the claim for that head; a new head approved again can merge.
    let moved = "3333333333333333333333333333333333333333";
    *docs.head.borrow_mut() = moved.into();
    approve(&mut ledger, temp.path(), &docs, "rv2").await;
    let merge_new = bash(&merge_line().replace(HEAD, moved), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge_new, "m4", 11, &docs).await, Some(true));
}

/// GPT review: a settlement that fails half way is undone, so it can be
/// retried; and a Claude result id shaped like a Codex key settles nothing.
#[tokio::test]
async fn a_failed_settlement_is_undone_and_codex_keys_are_not_claudes() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 2)]);
    approve(&mut ledger, temp.path(), &docs, "rv").await;
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(CHANGES, temp.path(), "reviewer"), "rc", 11, &docs).await, Some(true));
    ledger.db.execute_batch("ALTER TABLE auto_merge_head_reviews RENAME TO hidden").unwrap();
    assert!(ledger.settle_tool_call("rc", true).is_err());
    ledger.db.execute_batch("ALTER TABLE hidden RENAME TO auto_merge_head_reviews").unwrap();
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["reviewer"]);
    ledger.settle_tool_call("rc", true).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "the retried settlement still withdraws");
    // A Codex merge's claim is not released by a Claude result carrying its key.
    approve(&mut ledger, temp.path(), &docs, "rv2").await;
    let key = call_key("t", "u", "m");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&merge_line(), temp.path(), "fixer"), &key, 11, &docs).await, Some(true));
    ledger.settle_tool_call(&key, false).unwrap();
    let again = ToolProposal { expires: 101, ..bash(&merge_line(), temp.path(), "fixer") };
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &again, "m2", 11, &docs).await, None, "still claimed");
}
