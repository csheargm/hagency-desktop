//! Live PR #121 (Nov 18, dev room at Assist, owner Anna) as a user flow
//! (R6): anna-claude's pull request, reviewed by bird-codex (bird's Agent,
//! the other server) through bird's own Hagency; its "Hagency verdict:
//! APPROVE" is on GitHub under the shared login, and Anna's ledger has no
//! approval. The room gets ONE plain line; nothing merges.
use super::super::loop_notice::LoopNotice;
use super::super::merge_hold::{Gate, Missing};
use super::super::merge_hold_tests::{Fake, HEAD, check};
use super::super::reviewer_pick::Pick;
use super::{Reviewer, approval_line, asked_again, for_hold, own_reviewer, plain_name};
use ::hagency::issue_watch::APPROVE_VERDICT;
use ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON;
use matrix_sdk::ruma::owned_user_id;
use serde_json::{Value, json};

const URL: &str = "https://github.com/csheargm/hagency-demo-sandbox/pull/36";

/// The gate's answer as Anna's device gave it: no approval counted, and
/// one more would leave only the room's Assist level.
fn live(one_more: Value) -> Value {
    json!({"decision": "human", "head": HEAD, "reason": "needs 1 approval(s) from another Agent, has 0", "kind": "approvals", "approvers": 0, "recorded": 0, "oneMore": one_more})
}
fn assist() -> Value {
    json!({"decision": "human", "head": HEAD, "reason": ASSIST_REASON, "kind": "autonomy"})
}
/// bird-codex's verdict of this head, posted as the shared login.
fn verdict() -> Vec<(String, String, String)> {
    vec![("csheargm".to_owned(), APPROVE_VERDICT.to_owned(), HEAD.to_owned())]
}
fn bird_codex(owner: Option<&str>) -> Reviewer {
    Reviewer { name: "bird-codex".into(), owner: owner.map(str::to_owned), own: false }
}
/// What the room is told after three checks of the same head.
fn told(readiness: Value) -> (Gate, Vec<String>, Vec<LoopNotice>) {
    let fake = Fake::new(readiness);
    let gate = check(&fake, Some("admin"), true);
    check(&fake, Some("admin"), true);
    check(&fake, Some("admin"), true);
    let posted = fake.posted.borrow().clone();
    (gate, fake.bodies(), posted)
}

#[test]
fn pr_121_another_owners_review_at_assist_is_one_ready_line_and_never_a_merge() {
    let readiness = for_hold(live(assist()), (&verdict(), "csheargm"), Some(&bird_codex(Some("bird"))));
    let (gate, bodies, posted) = told(readiness);
    assert_eq!(gate, Gate::Hold(vec![(Missing::Autonomy, ASSIST_REASON.into())]), "a person merges: never Gate::Merge");
    assert_eq!(bodies, [format!("✅ PR #36 in csheargm/hagency-demo-sandbox was reviewed by bird-codex (bird's agent) and is ready. Agent work in this room is Assist, so a person merges it: {URL}")], "one line per head");
    assert!(matches!(&posted[0], LoopNotice::AwaitsPerson { step, .. } if step == "hold-autonomy-assist"), "the Assist notice's own step: {:?}", posted[0]);
}

#[test]
fn pr_121_with_autonomy_the_hold_names_the_reviewer_and_why_it_does_not_count() {
    // Autonomous: one more approval would merge, so the approval is what holds it.
    let readiness = for_hold(live(json!({"decision": "merge", "head": HEAD})), (&verdict(), "csheargm"), Some(&bird_codex(Some("bird"))));
    let (gate, bodies, _) = told(readiness);
    let Gate::Hold(missing) = gate else { panic!("{gate:?}") };
    assert_eq!(missing[0].0, Missing::Approval);
    assert_eq!(bodies.len(), 1);
    let line = "an Agent approval that counts here. bird-codex (bird's agent) was asked to review it and an approval of this version is on GitHub, but another owner's Agent's review is recorded only on that owner's device, so it does not count here. Merge it yourself, or ask one of your own Agents to review it.";
    assert!(bodies[0].contains(line), "{}", bodies[0]);
    for jargon in ["merge gate", "head", "…", "was not posted through", "needs 1 approval(s)"] {
        assert!(!bodies[0].contains(jargon), "{jargon}: {}", bodies[0]);
    }
}

#[test]
fn without_a_verdict_or_a_reviewer_nothing_is_said_ready() {
    let other = for_hold(live(assist()), (&[], "csheargm"), Some(&bird_codex(None)));
    assert_eq!(other["approvalLine"], "an Agent approval that counts here. bird-codex (another owner's agent) was asked to review it, but another owner's Agent's review is recorded only on that owner's device, so it would not count here. Merge it yourself, or ask one of your own Agents to review it.");
    assert_eq!(other["kind"], "approvals", "no verdict on GitHub: still a missing approval");
    let nobody = for_hold(live(assist()), (&verdict(), "csheargm"), None);
    assert_eq!(nobody["kind"], "approvals", "nobody asked: the verdict is not attributed");
    let elsewhere = json!({"decision": "human", "head": "2".repeat(40), "reason": ASSIST_REASON});
    assert_eq!(for_hold(live(elsewhere), (&verdict(), "csheargm"), Some(&bird_codex(Some("bird"))))["kind"], "approvals", "another head's answer never counts");
    let mallory = for_hold(live(assist()), (&verdict(), "mallory"), Some(&bird_codex(Some("bird"))));
    assert!(mallory["approvalLine"].as_str().unwrap().contains("was asked to review it, but"), "a verdict by another login is not the reviewer's");
    let ci = json!({"decision": "human", "head": HEAD, "kind": "ci", "reason": "CI is red"});
    assert_eq!(for_hold(ci.clone(), (&verdict(), "csheargm"), Some(&bird_codex(Some("bird")))), ci, "only a missing approval");
}

#[test]
fn this_owners_own_reviewer_is_named_plainly() {
    let anna_codex = Reviewer { name: "anna-codex".into(), owner: None, own: true };
    let ready = for_hold(live(assist()), (&verdict(), "csheargm"), Some(&anna_codex));
    assert_eq!(ready["reviewedBy"], "anna-codex");
    assert_eq!(approval_line(Some(&anna_codex), true, 0), "an Agent approval that counts here. An approval of this version is on GitHub, but none that counts here is recorded, even after anna-codex was asked again. Merge it yourself, or ask anna-codex to review it again.");
    assert!(approval_line(Some(&anna_codex), false, 1).starts_with("more Agent approvals (1 counted so far). anna-codex was asked to review it"));
    // GPT review P2: a counted approval is never contradicted by "none is recorded".
    assert_eq!(approval_line(None, false, 1), "more Agent approvals (1 counted so far). No further approval of this version is recorded yet. Merge it yourself, or ask an Agent in the room to review it.");
}

#[test]
fn pr_121_another_owners_reviewer_is_never_asked_again() {
    let pick = |own| Pick { reviewer: owned_user_id!("@_hagency_agt_bird_codex:localhost:8452"), notice: None, own };
    assert_eq!(asked_again(Some(&pick(false))), None, "asking again cannot help: only bird's device records it");
    assert_eq!(asked_again(Some(&pick(true))), Some(&pick(true)));
    assert_eq!(asked_again(None), None);
}

#[test]
fn a_room_name_stays_on_one_short_line() {
    assert_eq!(plain_name("bird\n✅ merged\tnow"), "bird ✅ merged now");
    assert_eq!(plain_name(&"é".repeat(60)).chars().count(), 48);
}

/// The own reviewer's recorded approval leaves only Assist: the line names it too (Nov 19).
#[test]
fn an_own_reviewer_is_named_on_the_assist_line() {
    let anna_codex = Reviewer { name: "anna-codex".into(), owner: None, own: true };
    let held = json!({"decision": "human", "head": HEAD, "kind": "autonomy", "reason": ASSIST_REASON});
    assert_eq!(own_reviewer(held.clone(), Some(&anna_codex))["reviewedBy"], "anna-codex");
    assert_eq!(own_reviewer(held.clone(), Some(&bird_codex(Some("bird")))), held, "another owner's Agent is not taken as this approval");
    assert_eq!(own_reviewer(held.clone(), None), held);
    let named = json!({"reviewedBy": "bird-codex (bird's agent)"});
    assert_eq!(own_reviewer(named.clone(), Some(&anna_codex)), named, "an already named reviewer stays");
    assert_eq!(own_reviewer(Value::Bool(true), Some(&anna_codex)), Value::Bool(true), "never indexes a non-object");
}
