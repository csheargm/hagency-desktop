//! The turn side of `watched_repo_step`: a call outside push freely is
//! refused at once and runs nothing; one inside runs on the host and its
//! output reaches the model; a room without native commands refuses.
use super::*;
use crate::watch_trust::fix_flow_tests::{OWNER, REPO, ROOM, git, proposal, run_under};
use crate::ToolPolicy;

async fn ask(ledger: &mut Ledger, turn: &Turn<'_>, command: &str, running: &mut Running) -> (String, bool) {
    let (reply, answer) = oneshot::channel();
    start(ledger, turn, Call { command: command.into(), reply }, running).await;
    answer.await.unwrap()
}

#[tokio::test]
async fn a_turn_runs_only_card_free_steps() {
    let mut r = run_under(ToolPolicy::Deny);
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&r.root.join("app"), &["remote", "set-url", "origin", &format!("https://github.com/{REPO}.git")]);
    let scope = proposal(&r.root, ROOM, "codex.repo_step", serde_json::json!({})).scope;
    let workspace = r.root.to_string_lossy().into_owned();
    let mut turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
    let mut running = Running::default();
    let (out, failed) = ask(&mut r.ledger, &turn, "git -C app status --short --branch", &mut running).await;
    assert!(!failed && out.starts_with("## main"), "{out}");
    let marker = r.root.join("ran");
    let (out, failed) = ask(&mut r.ledger, &turn, &format!("git -C app status; touch {}", marker.display()), &mut running).await;
    assert!(failed && out.contains("not_card_free"), "{out}");
    assert!(!marker.exists(), "a refused call runs nothing");
    // A call given up while queued (its MCP wait expired) never starts.
    let (reply, answer) = oneshot::channel();
    drop(answer);
    start(&mut r.ledger, &turn, Call { command: "git -C app checkout -b late origin/main".into(), reply }, &mut running).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(std::process::Command::new("git").args(["-C", r.root.join("app").to_str().unwrap(), "rev-parse", "--verify", "-q", "late"])
        .status().is_ok_and(|s| !s.success()), "the abandoned step ran");
    turn.enabled = false;
    let (out, failed) = ask(&mut r.ledger, &turn, "git -C app status", &mut running).await;
    assert!(failed && out.contains("runs no commands"), "{out}");
}

#[tokio::test]
async fn forward_hands_the_call_to_its_turn_and_stops_with_it() {
    let (_turn, cancel) = tokio::sync::watch::channel(false);
    let wait = std::time::Duration::from_secs(5);
    let args = serde_json::json!({"command": "git status"});
    assert_eq!(forward(None, &args, cancel.clone(), wait).await, Err("repo_steps_unavailable"));
    let (sender, mut calls) = channel();
    assert_eq!(forward(Some(sender.clone()), &serde_json::json!({"cmd": 1}), cancel.clone(), wait).await, Err("arguments_invalid"));
    let answered = tokio::spawn(async move {
        let call = calls.recv().await.unwrap();
        assert_eq!(call.command, "git status");
        let _ = call.reply.send(("clean".into(), false));
        calls
    });
    assert_eq!(forward(Some(sender.clone()), &args, cancel.clone(), wait).await, Ok(("clean".into(), false)));
    let mut calls = answered.await.unwrap();
    // The turn ends while the step waits: the call is refused, not left hanging.
    let (end, cancel) = tokio::sync::watch::channel(false);
    let waiting = tokio::spawn(async move { forward(Some(sender), &serde_json::json!({"command": "git status"}), cancel, wait).await });
    let _held = calls.recv().await.unwrap();
    end.send(true).unwrap();
    assert_eq!(waiting.await.unwrap(), Err("turn_ended"));
}

/// GitHub as the gate reads it for PR #50: a README change on head [`H50`],
/// CI green, the repository's policy asking one other Agent's approval.
struct Pr50 {
    opened_by: Option<String>,
}
const H50: &str = "4fe966ad790d85f5120ccf172f29eb6a4b3fc005";
impl crate::merge_policy::github::PrSource for Pr50 {
    async fn pull_request(&self, _: &crate::merge_policy::gate::GhPr) -> Option<crate::merge_policy::github::PrView> {
        let facts = crate::merge_policy::PrFacts { open: true, files: vec![crate::merge_policy::ChangedFile { path: "README.md".into(), previous_path: None, additions: 4, deletions: 0, opaque: false }], checks: vec![crate::merge_policy::Check::Passed] };
        Some(crate::merge_policy::github::PrView { url: format!("https://github.com/{REPO}/pull/50"), base_sha: "2".repeat(40), head_sha: H50.into(), author: "csheargm".into(), opened_by: self.opened_by.clone(), facts })
    }
    async fn policy(&self, _: &crate::merge_policy::gate::GhPr, _: &str, _: &str) -> Option<String> {
        Some("low_risk_paths: [\"**/*.md\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: true\n".into())
    }
}
fn review(mode: &str, head: &str) -> String {
    format!("gh pr review 50 --repo {REPO} {mode} -b \"Hagency verdict: APPROVE {head} - Focused README FAQ addition. Opened by agent bird (@_hagency_agt_x:localhost) on request from @bird:localhost\"")
}

/// The PR #50 review sent through `watched_repo_step`: the shared login's
/// `--approve` is refused by GitHub (recorded nothing), the `--comment` that
/// follows counts, exactly as a Claude reviewer's: held under the step, on
/// the exact head, for this room, once the pinned command succeeded.
#[tokio::test]
async fn a_review_sent_as_a_repo_step_is_judged_and_recorded_by_the_merge_gate() {
    let mut r = run_under(ToolPolicy::AskOwner);
    r.ledger.set_auto_merge(OWNER, "bird", true).unwrap();
    let scope = proposal(&r.root, ROOM, "codex.repo_step", serde_json::json!({})).scope;
    let workspace = r.root.to_string_lossy().into_owned();
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
    let mut running = Running::default();
    let source = Pr50 { opened_by: None };
    // (Author Agents never counting at merge time: merge_policy::codex_observed tests.)
    let url = format!("https://github.com/{REPO}/pull/50");
    for (mode, ran) in [("--approve", false), ("--comment", true)] {
        let command = review(mode, H50);
        let step_proposal = super::proposal(&r.ledger, &turn, &command).unwrap();
        let key = running.key(turn.dispatch);
        let step = gated_review(&mut r.ledger, &turn, &step_proposal, &command, &key, &source).await;
        let Some(TrustedStep::Command(pinned)) = step else { panic!("{mode}: {step:?}") };
        assert!(pinned.contains(&format!("gh pr review 50 {mode} -R")), "the pinned verdict runs: {pinned}");
        assert!(r.ledger.agent_reviewers(&url, H50).unwrap().is_empty(), "nothing counts before it ran");
        running.outcomes.lock().unwrap().push((key, ran));
        running.settle(&mut r.ledger).unwrap();
    }
    assert_eq!(r.ledger.agent_reviewers(&url, H50).unwrap(), ["bird"], "one approval, from the --comment");
    assert_eq!(r.ledger.agent_reviewers_in(&url, H50, Some(ROOM)).unwrap(), ["bird"]);
    assert!(r.ledger.agent_reviewers_in(&url, H50, Some("!elsewhere:x")).unwrap().is_empty());
}

#[tokio::test]
async fn a_repo_step_review_of_a_stale_head_is_never_recorded() {
    let mut r = run_under(ToolPolicy::AskOwner);
    r.ledger.set_auto_merge(OWNER, "bird", true).unwrap();
    let scope = proposal(&r.root, ROOM, "codex.repo_step", serde_json::json!({})).scope;
    let workspace = r.root.to_string_lossy().into_owned();
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
    let mut running = Running::default();
    let url = format!("https://github.com/{REPO}/pull/50");
    // A verdict naming another head: the gate leaves it to push trust, never records it.
    let stale = review("--comment", &"3".repeat(40));
    let p = super::proposal(&r.ledger, &turn, &stale).unwrap();
    let key = running.key(turn.dispatch);
    assert!(gated_review(&mut r.ledger, &turn, &p, &stale, &key, &Pr50 { opened_by: None }).await.is_none());
    running.outcomes.lock().unwrap().push((key, true));
    running.settle(&mut r.ledger).unwrap();
    assert!(r.ledger.agent_reviewers(&url, H50).unwrap().is_empty());
}

/// GPT review of 2a7b1ed9: a merge or ready sent here never reaches the gate
/// (no claim taken), and a room member's verdict needs the room's own review
/// grant before the gate may run it.
#[tokio::test]
async fn only_a_verdict_reaches_the_gate_and_a_members_needs_the_rooms_review_grant() {
    let mut r = run_under(ToolPolicy::AskOwner);
    r.ledger.set_auto_merge(OWNER, "bird", true).unwrap();
    let scope = proposal(&r.root, ROOM, "codex.repo_step", serde_json::json!({})).scope;
    let workspace = r.root.to_string_lossy().into_owned();
    let mut running = Running::default();
    let source = Pr50 { opened_by: None };
    let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
    let merge = format!("gh pr merge 50 --squash --match-head-commit {H50} --repo {REPO}");
    let p = super::proposal(&r.ledger, &turn, &merge).unwrap();
    let key = running.key(turn.dispatch);
    assert!(gated_review(&mut r.ledger, &turn, &p, &merge, &key, &source).await.is_none());
    assert!(r.ledger.claim_auto_merge(&format!("https://github.com/{REPO}/pull/50"), H50, "bird").unwrap(), "no claim was taken");
    let member = crate::Scope { requester: "@carl:example.org".into(), ..scope.clone() };
    let as_member = Turn { scope: &member, ..turn };
    let comment = review("--comment", H50);
    let p = super::proposal(&r.ledger, &as_member, &comment).unwrap();
    let key = running.key(as_member.dispatch);
    assert!(gated_review(&mut r.ledger, &as_member, &p, &comment, &key, &source).await.is_none(), "no review grant for this room");
}

/// e2e scenario 8 (2026-11-17): bird-codex, which watches NOTHING, is asked
/// in a room to review a PR on a repository linked to that room (bird-claude
/// watches it). Its read-only `gh pr view|diff` sent as a `watched_repo_step`
/// runs without a card, exactly as Claude's `Bash` read does (ADR 0023); an
/// unlinked repository (e2e 5b) and any write are refused here.
mod room_repo_reads {
    use super::*;
    const LINKED_ROOM: &str = "!codex-room-reads:example.org";
    const ANNA: &str = "@anna:example.org";

    /// A Deny ledger (a hosted Agent's default) for bird-codex, which watches
    /// nothing; the host recorded `REPO` as linked to the room for Anna.
    fn watcherless(dir: &std::path::Path) -> (Ledger, Scope) {
        let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
        let scope = Scope { agent: "bird-codex".into(), binding: "b".into(), room: LINKED_ROOM.into(), requester: ANNA.into(), thread: "t".into() };
        ledger.register_binding(OWNER, &scope).unwrap();
        let deny = crate::Policy {
            budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
            requests: crate::RequestPolicy::Allow,
            high_risk: ToolPolicy::Deny,
        };
        for layer in [crate::Layer::Agent, crate::Layer::Room, crate::Layer::Requester] {
            ledger.set_policy(OWNER, &scope, layer, 0, &deny).unwrap();
        }
        ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
        crate::watch_trust::room_scope::record(LINKED_ROOM, ANNA, vec![REPO.into()]);
        (ledger, scope)
    }

    #[test]
    fn a_watcherless_codex_reads_a_room_linked_repo_card_free() {
        let db = tempfile::tempdir().unwrap();
        let (mut ledger, scope) = watcherless(db.path());
        let workspace = db.path().to_string_lossy().into_owned();
        let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
        for read in [format!("gh pr view 70 --repo {REPO} --json title,body,files"), format!("gh pr diff 70 --repo {REPO}"), format!("gh issue list --repo {REPO}")] {
            let p = super::super::proposal(&ledger, &turn, &read).unwrap();
            let step = ledger.trusted_publish(OWNER, &p, crate::codex::now()).unwrap();
            let Some(TrustedStep::Command(pinned)) = step else { panic!("{read}: {step:?}") };
            assert!(pinned.contains("GH_HOST='github.com'") && pinned.contains(&read), "{pinned}");
        }
        // Its verdict on that PR is the room's review grant (ADR 0025), watch or not.
        let verdict = format!("gh pr review 70 --repo {REPO} --comment -b \"Hagency verdict: APPROVE {H50} - ok\"");
        let p = super::super::proposal(&ledger, &turn, &verdict).unwrap();
        let step = ledger.trusted_publish(OWNER, &p, crate::codex::now()).unwrap();
        let Some(TrustedStep::Command(pinned)) = step else { panic!("{verdict}: {step:?}") };
        assert!(pinned.contains("gh pr review 70 --comment -R"), "{pinned}");
    }

    #[tokio::test]
    async fn an_unlinked_repo_or_a_write_on_the_room_repo_is_refused() {
        let db = tempfile::tempdir().unwrap();
        let (mut ledger, scope) = watcherless(db.path());
        let workspace = db.path().to_string_lossy().into_owned();
        let turn = Turn { owner: OWNER, scope: &scope, dispatch: "d", workspace: &workspace, token: None, enabled: true };
        let mut running = Running::default();
        let marker = db.path().join("ran");
        // (A review verdict on a room repository is ADR 0025's own grant,
        // judged by the merge gate: see the review tests above.)
        for refused in [
            "gh pr view 70 --repo other/unlinked --json title".to_owned(),
            format!("gh pr comment 70 --repo {REPO} -b hi"),
            format!("gh pr create --repo {REPO} --head fix --title t --body b"),
            format!("gh repo clone {REPO} ./app"),
            "git -C app push -u origin fix".to_owned(),
            format!("gh pr view 70 --repo {REPO}; touch {}", marker.display()),
        ] {
            let (out, failed) = ask(&mut ledger, &turn, &refused, &mut running).await;
            assert!(failed && out.contains("not_card_free"), "{refused}: {out}");
        }
        assert!(!marker.exists(), "a refused call runs nothing");
    }
}

