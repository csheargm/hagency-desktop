//! Live PR #115 (Nov 18) as a user flow (R6): bird-claude, which may work
//! in the dev room, opened the pull request; the owner's loop asked
//! bird-codex, a reviewer the owner never let work there, to review it. Its
//! verdict, sent as its `watched_repo_step` (the exact text from its Codex
//! session), reached the merge gate, which refused it for not working in the
//! room; push trust then posted it card-free and nothing was recorded, so the
//! loop asked again and again. A review the owner's loop asked for, in that
//! very request, of exactly that head, is recorded on that head; the author's
//! own never counts, and an unasked one still needs an Agent that works there.
use super::gate::{self, GhPr};
use super::github::{HeadRef, PrSource, PrView};
use super::tests::{bash, file, ledger_with, set_room_rules, ROOM, SANDBOX};
use super::{Decision, PrFacts};
use crate::room_autonomy::{AutonomySnapshot, Level, ASSIST_REASON};
use crate::{Ledger, ToolProposal};

const OWNER: &str = "@owner:example.org";
const HEAD: &str = "8b0c8e9dbd2a0bbcf6a6f2b507058299328923cd";
const URL: &str = "https://github.com/csheargm/hagency-demo-sandbox/pull/115";
/// The room's rules as recorded in bird's ledger at the time.
const LIVE_RULES: &str = "low_risk_paths: [\"**/*.md\", \"docs/**\", \"*.txt\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: false\n";
/// bird-codex's Matrix ID, as the host registers it.
const CODEX_MXID: &str = "@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452";
/// The loop's request bird-codex received at 20:12:59 (its first lines).
const ASKED: &str = "🔎 Review requested — @_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452 please review PR #115 in csheargm/hagency-demo-sandbox at head commit 8b0c8e9dbd2a0bbcf6a6f2b507058299328923cd: https://github.com/csheargm/hagency-demo-sandbox/pull/115\nTo review: read it only with these plain commands";
/// bird-codex's step at 20:13:09, as its Codex session sent it.
const VERDICT: &str = "gh pr review 115 --repo csheargm/hagency-demo-sandbox --comment -b \"Hagency verdict: APPROVE 8b0c8e9dbd2a0bbcf6a6f2b507058299328923cd - Scoped README thank-you addition is correct. Opened by agent bird-codex (@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452) on request from @bird:localhost:8452\"";

/// GitHub as the gate read PR #115: two README lines, no checks at all.
struct Pr115;
impl PrSource for Pr115 {
    async fn pull_request(&self, _: &GhPr) -> Option<PrView> {
        let facts = PrFacts { open: true, files: vec![file("README.md", 2)], checks: vec![] };
        let head_ref = Some(HeadRef { branch: "bird-claude/issue-114-readme-thanks".into(), created_at: 1_795_061_485 });
        Some(PrView { base_ref: "main".into(), url: URL.into(), base_sha: "2".repeat(40), head_sha: HEAD.into(), author: "csheargm".into(), opened_by: Some("bird-claude".into()), head_ref, facts })
    }
}

/// bird's ledger: bird-claude may work in the Assist room, bird-codex may not.
fn birds_ledger(dir: &std::path::Path, paused: bool) -> Ledger {
    let mut ledger = ledger_with(dir, &["bird-claude", "bird-codex"]);
    set_room_rules(&mut ledger, SANDBOX, Some(LIVE_RULES));
    ledger.set_may_work(OWNER, "bird-claude", ROOM, true).unwrap();
    let snapshot = AutonomySnapshot { room_id: ROOM.into(), level: Some(Level::Assist), paused, unread: false, owner_maintainer: true };
    ledger.set_room_autonomy(OWNER, &snapshot, super::room_rules::unix_now()).unwrap();
    crate::attribution::register_agent_identity("bird-codex", "bird-codex", CODEX_MXID);
    ledger
}

/// bird-claude's create of PR #115 at 20:11:24 by `agent`, held and run, as the ledger records it.
fn created_by(ledger: &mut Ledger, dir: &std::path::Path, agent: &str) {
    let create = format!("gh pr create --repo {SANDBOX} --head bird-claude/issue-114-readme-thanks --draft --title 'docs: add a Thanks line to README' --body 'Closes #114'");
    ledger.hold_create("toolu_create", &bash(&create, dir, agent), 1_795_061_484).unwrap();
    ledger.settle_tool_call("toolu_create", true).unwrap();
}

/// Request `dispatch` received `body`, tagged as the ledger tags loop notices.
fn received(ledger: &Ledger, dispatch: &str, body: &str) {
    let (step, pr, head, to) = super::step_resend::loop_step_of(body).unwrap();
    ledger.db.execute("INSERT INTO loop_step_dispatches VALUES (?,?,?,?,?)", (dispatch, step, pr, head, to)).unwrap();
}

/// `line` sent by `agent` as its Codex `watched_repo_step`.
fn repo_step(line: &str, dir: &std::path::Path, agent: &str) -> ToolProposal {
    ToolProposal { tool: crate::watch_trust::codex_step::TOOL.into(), ..bash(line, dir, agent) }
}

/// What the gate counts toward bird-claude's merge (Assist: approvals pass, a person merges).
async fn merge_decision(ledger: &mut Ledger, dir: &std::path::Path) -> Option<Decision> {
    let merge = format!("gh pr merge 115 --squash --match-head-commit {HEAD} -R {SANDBOX}");
    let command = gate::parse_command(&bash(&merge, dir, "bird-claude")).unwrap();
    super::review_loop::merge_readiness(ledger, &command, ("bird-claude", ROOM), &Pr115).await.map(|(_, decision)| decision)
}

#[tokio::test]
async fn pr_115_the_loops_reviewer_is_recorded_on_the_exact_head_it_was_asked_for() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = birds_ledger(temp.path(), false);
    let step = repo_step(VERDICT, temp.path(), "bird-codex");
    assert!(ledger.loop_may_work("bird-codex", ROOM).is_err(), "the live setting: bird-codex does not work here");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &step, "unasked", 11, &Pr115).await, None, "not asked by the loop: the card asks");
    received(&ledger, &step.dispatch, ASKED);
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &step, "unknown", 11, &Pr115).await, None, "who created it is unknown: the card asks");
    created_by(&mut ledger, temp.path(), "bird-claude");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &step, "codex:step-1", 11, &Pr115).await, Some(true), "the gate judges and allows it");
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty(), "nothing counts before the post ran");
    ledger.settle_agent_review("codex:step-1", true).unwrap();
    assert_eq!(ledger.agent_reviewers_in(URL, HEAD, Some(ROOM)).unwrap(), ["bird-codex"]);
    assert_eq!(merge_decision(&mut ledger, temp.path()).await, Some(Decision::NeedsHuman(ASSIST_REASON.into())), "the approval counts: only Assist holds");
}

/// The request binds the review: another request, another head, another
/// addressee, a room member's request, or the asked Agent being the
/// pull request's own creator records nothing.
#[tokio::test]
async fn pr_115_a_request_for_another_head_turn_or_agent_records_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = birds_ledger(temp.path(), false);
    let step = repo_step(VERDICT, temp.path(), "bird-codex");
    received(&ledger, "d-other", ASKED);
    received(&ledger, "d-head", &ASKED.replace(HEAD, &"3".repeat(40)));
    received(&ledger, "d-agent", &ASKED.replace("agt_01m7k6ws4k2z06ysr6shrxeqzf", "agt_someone_else"));
    created_by(&mut ledger, temp.path(), "bird-claude");
    for dispatch in ["d-bird-codex", "d-head", "d-agent"] {
        let asked = ToolProposal { dispatch: dispatch.into(), ..step.clone() };
        assert_eq!(gate::auto_answer(&mut ledger, OWNER, &asked, dispatch, 11, &Pr115).await, None, "{dispatch}");
    }
    received(&ledger, "d-member", ASKED);
    let member = ToolProposal { dispatch: "d-member".into(), scope: crate::Scope { requester: "@mallory:example.org".into(), ..step.scope.clone() }, ..step };
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &member, "d-member", 11, &Pr115).await, None, "only the owner's loop asks");
    // GPT review: the asked reviewer that created the pull request itself is no reviewer.
    let own = tempfile::tempdir().unwrap();
    let mut authored = birds_ledger(own.path(), false);
    created_by(&mut authored, own.path(), "bird-codex");
    let step = repo_step(VERDICT, own.path(), "bird-codex");
    received(&authored, &step.dispatch, ASKED);
    assert_eq!(gate::auto_answer(&mut authored, OWNER, &step, "self", 11, &Pr115).await, None, "its own pull request");
    for call in ["d-bird-codex", "d-head", "d-agent", "d-member"] {
        ledger.settle_agent_review(call, true).unwrap();
    }
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}

#[tokio::test]
async fn pr_115_the_authors_own_verdict_and_a_paused_room_record_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = birds_ledger(temp.path(), false);
    let own = repo_step(&VERDICT.replace("bird-codex", "bird-claude"), temp.path(), "bird-claude");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &own, "own", 11, &Pr115).await, Some(true));
    ledger.settle_agent_review("own", true).unwrap();
    let Some(Decision::NeedsHuman(reason)) = merge_decision(&mut ledger, temp.path()).await else { panic!("the author's approval merged") };
    assert_eq!(super::reason_kind(&reason), "approvals", "no self-approval counts: {reason}");
    let paused = tempfile::tempdir().unwrap();
    let mut ledger = birds_ledger(paused.path(), true);
    let step = repo_step(VERDICT, paused.path(), "bird-codex");
    received(&ledger, &step.dispatch, ASKED);
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &step, "p", 11, &Pr115).await, None, "a paused room: the card asks");
    ledger.settle_agent_review("p", true).unwrap();
    assert!(ledger.agent_reviewers(URL, HEAD).unwrap().is_empty());
}
