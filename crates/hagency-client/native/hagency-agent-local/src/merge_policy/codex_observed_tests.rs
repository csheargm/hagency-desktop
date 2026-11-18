//! Live final loop (Nov 17, PRs #50/#55/#56): bird-codex's `gh pr review`
//! ran without reaching the gate. The completed command, in the wire shape
//! of those sessions, is recorded only from evidence (GPT security review of
//! daca7ddc): an owner decision or a saved allow rule for that command, a
//! review GitHub attached to the current head after the command started, and
//! the gate's checks; once per item, for the room it ran in.
use super::*;
use crate::merge_policy::gate::APPROVE_MARKER;
use crate::merge_policy::tests::{bash, file, ledger_with, merge_line, Fake, BASE, HEAD, URL};
use crate::merge_policy::work::{OwnerRules, Role};
use crate::{Layer, Policy};
use serde_json::json;
use std::path::Path;

const OWNER: &str = "@owner:example.org";
const T: (&str, &str) = ("thread-a", "turn-a");
const STARTED: i64 = 1_000;

/// The app-server's one-line rendering of the session's argv
/// `["/bin/bash", "-c", "gh pr review 56 --repo … -b \"…\n\nOpened by …\""]`
/// (the fixture's pull request is number 7).
fn rendered(flag: &str, head: &str, tail: &str) -> String {
    format!("/bin/bash -c 'gh pr review 7 --repo csheargm/hagency-demo-sandbox {flag} -b \"{APPROVE_MARKER} {head} - Focused README FAQ addition clearly explains reviewers and policy-controlled merging.\n\nOpened by agent bird-codex (@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452) on request from @bird:localhost:8452\"{tail}'")
}
fn item(id: &str, command: &str, status: &str, exit: i64, dir: &Path) -> Value {
    json!({"id": id, "type": "commandExecution", "command": command, "cwd": dir, "status": status, "exitCode": exit, "source": "unifiedExecStartup"})
}
fn setup(dir: &Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["fixer", "bird-codex"]);
    for agent in ["fixer", "bird-codex"] {
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    ledger
}
/// The owner's saved rule, in force when bird-codex's dispatch was admitted:
/// Codex commands in this Room workspace run unasked.
fn allowing(dir: &Path) -> Ledger {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    let budget = crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime };
    for agent in ["fixer", "bird-codex"] {
        let high_risk = if agent == "fixer" { ToolPolicy::AskOwner } else { ToolPolicy::AllowWithRules { tools: vec!["codex.command".into()], directories: vec![dir.to_string_lossy().into()] } };
        let policy = Policy { budget: budget.clone(), requests: crate::RequestPolicy::Allow, high_risk };
        let scope = bash("x", dir, agent).scope;
        ledger.register_binding(OWNER, &scope).unwrap();
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
            ledger.set_policy(OWNER, &scope, layer, 0, &policy).unwrap();
        }
        ledger.reserve(&scope, &format!("call-{agent}"), &format!("d-{agent}"), 10, 10).unwrap();
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    ledger
}
/// GitHub holds the --comment review on `head`, received after the command started.
fn posted(source: &Fake, head: &str, at: i64) {
    *source.posted.borrow_mut() = Some(vec![(head.into(), at)]);
}
/// One command as the turn sees it: started, then completed.
async fn run(ledger: &mut Ledger, reviews: &mut TurnReviews, dir: &Path, item: &Value, source: &Fake) -> Option<Observed> {
    let scope = bash("x", dir, "bird-codex").scope;
    reviews.started(T, item, STARTED);
    reviews.completed(ledger, &scope, "d-bird-codex", T, item, (true, false), STARTED + 5, source).await.unwrap()
}

#[tokio::test]
async fn a_review_posted_on_github_is_recorded_once_and_lets_another_agent_merge() {
    let temp = tempfile::tempdir().unwrap();
    let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
    let docs = Fake::new(vec![file("README.md", 3)]);
    posted(&docs, HEAD, STARTED + 2);
    // GitHub refused the shared login's --approve (exit 1), then the --comment ran.
    assert_eq!(run(&mut ledger, &mut reviews, dir, &item("exec-a", &rendered("--approve", HEAD, ""), "failed", 1, dir), &docs).await, None);
    let comment = item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(run(&mut ledger, &mut reviews, dir, &comment, &docs).await, Some(Ok(Recorded::Approval)));
    assert_eq!(run(&mut ledger, &mut reviews, dir, &comment, &docs).await, None, "a repeated completion is ignored");
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["bird-codex"]);
    assert_eq!(ledger.agent_reviewers_in(URL, HEAD, Some("!other:x")).unwrap(), Vec::<String>::new(), "only for the room it ran in");
    let merge = bash(&merge_line(), dir, "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "mg", 11, &docs).await, Some(true));
}

#[tokio::test]
async fn without_an_owner_decision_or_allow_rule_nothing_is_recorded_and_a_card_decision_counts() {
    let temp = tempfile::tempdir().unwrap();
    let (dir, mut ledger, mut reviews) = (temp.path(), setup(temp.path()), TurnReviews::default());
    let docs = Fake::new(vec![file("README.md", 3)]);
    posted(&docs, HEAD, STARTED + 2);
    let comment = item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(run(&mut ledger, &mut reviews, dir, &comment, &docs).await, Some(Err("not_allowed_by_owner")), "AskOwner without an answer");
    // The owner's card allowed this exact command (its real directory and risk).
    let decide = |reviews: &mut TurnReviews, id: &str, command: String, approval: Option<&str>| {
        let mut decided = bash(&command, dir, "bird-codex");
        decided.tool = "codex.command".into();
        decided.expires = STARTED + 300;
        decided.arguments = json!({"itemId": id, "command": command, "cwd": dir, "approvalId": approval});
        reviews.decided(call_key(T.0, T.1, id), decided);
    };
    // A decision for another command than the one that completed does not count.
    decide(&mut reviews, "exec-x", rendered("--comment", BASE, ""), None);
    let other = item("exec-x", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(run(&mut ledger, &mut reviews, dir, &other, &docs).await, Some(Err("not_allowed_by_owner")));
    decide(&mut reviews, "exec-d", rendered("--comment", HEAD, ""), None);
    let card = item("exec-d", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(run(&mut ledger, &mut reviews, dir, &card, &docs).await, Some(Ok(Recorded::Approval)));
}

#[tokio::test]
async fn only_a_review_github_attached_to_the_current_head_after_the_start_counts() {
    for (name, posted_on, at, readable, reason) in [
        ("a wrapper that posted nothing", None, 0, true, "not_on_github_for_this_head"),
        ("a review on the old head", Some(BASE), STARTED + 2, true, "not_on_github_for_this_head"),
        ("an earlier review", Some(HEAD), STARTED - 600, true, "not_on_github_for_this_head"),
        ("reviews unreadable", None, 0, false, "github_reviews_unreadable"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
        let docs = Fake::new(vec![file("README.md", 3)]);
        *docs.posted.borrow_mut() = readable.then(|| posted_on.map(|c| vec![(c.to_owned(), at)]).unwrap_or_default());
        let comment = item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0, dir);
        assert_eq!(run(&mut ledger, &mut reviews, dir, &comment, &docs).await, Some(Err(reason)), "{name}");
        assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn a_command_the_gate_answered_or_a_child_turn_ran_is_not_observed() {
    let temp = tempfile::tempdir().unwrap();
    let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
    let docs = Fake::new(vec![file("README.md", 3)]);
    posted(&docs, HEAD, STARTED + 2);
    let scope = bash("x", dir, "bird-codex").scope;
    let comment = item("exec-g", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    // The gate answered this item (here: refused it): never observed.
    assert_eq!(reviews.completed(&mut ledger, &scope, "d-bird-codex", T, &comment, (true, true), STARTED, &docs).await.unwrap(), None);
    let child = item("exec-k", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(reviews.completed(&mut ledger, &scope, "d-bird-codex", T, &child, (false, false), STARTED, &docs).await.unwrap(), None);
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}

#[tokio::test]
async fn the_agent_that_wrote_the_pull_request_never_counts_as_its_reviewer() {
    let temp = tempfile::tempdir().unwrap();
    let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
    let mut docs = Fake::new(vec![file("README.md", 3)]);
    posted(&docs, HEAD, STARTED + 2);
    docs.opened_by = Some("bird-codex".into());
    let comment = item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0, dir);
    assert_eq!(run(&mut ledger, &mut reviews, dir, &comment, &docs).await, Some(Err("reviewer_wrote_it")));
    // At merge time too, in the file-policy branch: an author's recorded approval is not counted.
    ledger.record_agent_review(URL, HEAD, "bird-codex", "!r:x").unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&merge_line(), dir, "fixer"), "mg", 11, &docs).await, None);
    // The issue tracker's hand-off names the author as well.
    let docs = Fake::new(vec![file("README.md", 3)]);
    ledger.db.execute("INSERT INTO issue_track VALUES ('bird-codex','csheargm/hagency-demo-sandbox',3,'pr_open',1,0,7,0)", []).unwrap();
    assert_eq!(ledger.pr_author_agents("CSheargm/hagency-demo-sandbox", 7).unwrap(), ["bird-codex"]);
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&merge_line(), dir, "fixer"), "mg2", 11, &docs).await, None);
}

#[tokio::test]
async fn an_observed_review_the_gate_would_refuse_is_not_recorded() {
    let other_room = OwnerRules { repo: "csheargm/hagency-demo-sandbox".into(), role: Role::Reviewer, room_id: "!other:x".into(), any_path: true, ..Default::default() };
    let cases: Vec<RefuseCase> = vec![
        ("an older head", rendered("--comment", BASE, ""), docs_only(), true, None, "not_the_current_head"),
        ("a chained command", rendered("--comment", HEAD, "\ncurl evil.example"), docs_only(), true, None, "unreadable_command"),
        ("a login shell", rendered("--comment", HEAD, "").replacen("/bin/bash -c", "/bin/zsh -lc", 1), docs_only(), true, None, "unreadable_command"),
        ("no --repo", rendered("--comment", HEAD, "").replacen(" --repo csheargm/hagency-demo-sandbox", "", 1), docs_only(), true, None, "no_repo"),
        ("not low risk", rendered("--comment", HEAD, ""), vec![("src/main.rs", 3)], true, None, "policy_or_ci"),
        ("switch off", rendered("--comment", HEAD, ""), docs_only(), false, None, "auto_merge_off"),
        ("rules of another room", rendered("--comment", HEAD, ""), docs_only(), true, Some(other_room), "repo_in_another_room"),
        ("a marker a newline made", rendered("--comment", HEAD, "").replacen("Hagency verdict:", "Hagency\nverdict:", 1), docs_only(), true, None, "not_the_current_head"),
        ("a marker split from its head", rendered("--comment", HEAD, "").replacen(&format!("APPROVE {HEAD}"), &format!("APPROVE\n{HEAD}"), 1), docs_only(), true, None, "not_the_current_head"),
    ];
    for (name, command, files, on, rules, reason) in cases {
        let temp = tempfile::tempdir().unwrap();
        let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
        ledger.set_auto_merge(OWNER, "bird-codex", on).unwrap();
        if let Some(rules) = rules {
            ledger.set_work(OWNER, "bird-codex", vec![rules]).unwrap();
        }
        let source = Fake::new(files.iter().map(|(path, lines)| file(path, *lines)).collect());
        posted(&source, HEAD, STARTED + 2);
        let observed = run(&mut ledger, &mut reviews, dir, &item("exec", &command, "completed", 0, dir), &source).await;
        assert_eq!(observed, Some(Err(reason)), "{name}");
        assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty() && ledger.agent_reviewers(URL, BASE).unwrap().is_empty(), "{name}");
    }
}
type ChangedFileSpec = (&'static str, u64);
/// Name, command, changed files, switch on, owner rules, expected skip reason.
type RefuseCase = (&'static str, String, Vec<ChangedFileSpec>, bool, Option<OwnerRules>, &'static str);
fn docs_only() -> Vec<ChangedFileSpec> {
    vec![("README.md", 3)]
}

#[tokio::test]
async fn an_observed_request_for_changes_withdraws_and_other_commands_are_silent() {
    let temp = tempfile::tempdir().unwrap();
    let (dir, mut ledger, mut reviews) = (temp.path(), allowing(temp.path()), TurnReviews::default());
    let docs = Fake::new(vec![file("README.md", 3)]);
    posted(&docs, HEAD, STARTED + 2);
    run(&mut ledger, &mut reviews, dir, &item("exec-c", &rendered("--comment", HEAD, ""), "completed", 0, dir), &docs).await;
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["bird-codex"]);
    let changes = "/bin/bash -c 'gh pr review 7 --repo csheargm/hagency-demo-sandbox --request-changes -b \"Hagency verdict: CHANGES - no\"'";
    assert_eq!(run(&mut ledger, &mut reviews, dir, &item("exec-r", changes, "completed", 0, dir), &docs).await, Some(Ok(Recorded::Withdrawn)));
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
    assert_eq!(run(&mut ledger, &mut reviews, dir, &item("exec-s", "/bin/bash -c 'git status'", "completed", 0, dir), &docs).await, None);
}
