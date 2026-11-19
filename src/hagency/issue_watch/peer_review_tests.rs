//! Live PRs #119–#121 (Nov 18, dev room at Assist, owner Anna) as a user
//! flow (R6): bird-codex, bird's Agent on another server, reviewed Anna's
//! pull request; the room was told it is ready and a person merges it. After
//! that line nothing else may be said or asked for that head.
use super::super::super::loop_notice::{LoopNotice, Target};
use super::super::super::merge_hold::follow_gate;
use super::super::super::reviewer_pick::Pick;
use super::super::{LoopCtx, LoopIo};
use super::{PeerFacts, advance, verdict_of};
use super::super::super::reviewer_pick::{Candidate, Peers};
use ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON;
use matrix_sdk::ruma::{OwnedUserId, owned_user_id};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;

pub(super) const HEAD: &str = "ad395c6d9cdea90a54566da56cda9cae91ddd540";
const MOVED: &str = "bd395c6d9cdea90a54566da56cda9cae91ddd541";

/// The room and the owner API: `due` is what `stepDue` answers for the review step.
pub(super) struct Fake {
    pub posted: RefCell<Vec<LoopNotice>>,
    steps: RefCell<HashSet<String>>,
    pub readiness: RefCell<Value>,
    pub asked_due: RefCell<Vec<Value>>,
    pub due: RefCell<Value>,
}
impl Fake {
    pub(super) fn new(readiness: Value) -> Self {
        Self { posted: RefCell::default(), steps: RefCell::default(), readiness: RefCell::new(readiness), asked_due: RefCell::default(), due: RefCell::new(json!({"taken": true, "again": 0})) }
    }
    pub(super) fn bodies(&self) -> Vec<String> {
        self.posted.borrow().iter().map(LoopNotice::body).collect()
    }
}
impl LoopIo for Fake {
    async fn post(&self, notice: &LoopNotice, _again: u32) -> Result<(), String> {
        self.posted.borrow_mut().push(notice.clone());
        Ok(())
    }
    async fn review(&self, change: Value) -> Result<Value, String> {
        let key = |step: &Value| format!("{}@{}", step["step"].as_str().unwrap(), step["head"].as_str().unwrap());
        if let Some(step) = change.get("stepTaken") {
            return Ok(json!({"taken": self.steps.borrow().contains(&key(step))}));
        }
        if let Some(step) = change.get("stepDue") {
            self.asked_due.borrow_mut().push(step.clone());
            let taken = self.steps.borrow().contains(&key(step));
            return Ok(json!({"due": if taken { self.due.borrow().clone() } else { json!({"taken": false, "again": 0}) }}));
        }
        if let Some(step) = change.get("claimStep") {
            return Ok(json!({"claimed": self.steps.borrow_mut().insert(key(step))}));
        }
        if change.get("stepSaid").is_some() {
            return Ok(json!({}));
        }
        Ok(json!({"readiness": self.readiness.borrow().clone()}))
    }
    fn paused(&self) -> bool {
        false
    }
    fn requester(&self) -> String {
        "@_hagency_agt_anna_claude:localhost:8450".into()
    }
    fn room(&self) -> String {
        "!a3cQXHR8GZ4RP5n71s:localhost:8450".into()
    }
}

pub(super) fn target(head: &str) -> Target {
    Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 121, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/121".into(), head: head.into(), issues: Vec::new() }
}
/// GitHub and the room as the remote path reads them: `answered` is bird-codex's verdict of the head.
#[derive(Default)]
pub(super) struct Facts {
    pub answered: bool,
}
impl PeerFacts for Facts {
    async fn answered(&self, _target: &Target) -> Result<bool, String> {
        Ok(self.answered)
    }
    async fn label(&self, _peer: &Pick) -> String {
        "bird-codex (bird's agent)".into()
    }
}
pub(super) fn bird_codex() -> Pick {
    Pick { reviewer: owned_user_id!("@_hagency_agt_bird_codex:localhost:8452"), notice: None, own: false }
}
fn anna_claude() -> OwnedUserId {
    owned_user_id!("@_hagency_agt_anna_claude:localhost:8450")
}
/// The gate as Anna's device answers: no approval counted, one more leaves only Assist.
pub(super) fn at_assist(head: &str, one_more: &str) -> Value {
    let one_more = if one_more == "merge" { json!({"decision": "merge", "head": head}) } else { json!({"decision": "human", "head": head, "reason": ASSIST_REASON, "kind": "autonomy"}) };
    json!({"decision": "human", "head": head, "reason": "needs 1 approval(s) from another Agent, has 0", "kind": "approvals", "approvers": 0, "recorded": 0, "oneMore": one_more})
}
pub(super) fn block<T>(f: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(f)
}
/// The 23:19 line: the merge step's Assist notice for `head`, naming bird-codex.
fn ready_line_posted(fake: &Fake, head: &str) {
    let mut ready = at_assist(head, "assist")["oneMore"].clone();
    ready["reviewedBy"] = "bird-codex (bird's agent)".into();
    let agent = anna_claude();
    let ctx = LoopCtx { agent: &agent, name: "anna-claude", maintainer: true, permission: Some("admin") };
    block(follow_gate(fake, &target(head), &ready, &ctx, true)).unwrap();
}

#[test]
fn pr_121_once_the_ready_line_is_said_no_review_step_runs_for_that_head() {
    let fake = Fake::new(at_assist(HEAD, "assist"));
    // bird-codex was asked at 23:0x, then the ready line went out at 23:19.
    assert!(!block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap());
    ready_line_posted(&fake, HEAD);
    // The review step's "could not start" judgement (23:24) and the rotation (23:26) came from stepDue.
    *fake.due.borrow_mut() = json!({"taken": false, "again": 1, "say": {"why": "unseen", "gaveUp": true, "round": 1}});
    let asked = fake.asked_due.borrow().len();
    for _ in 0..3 {
        assert!(block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap(), "the head is settled");
    }
    assert_eq!(fake.asked_due.borrow().len(), asked, "the review step is never judged again for a settled head");
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 2, "the review request and the ready line, nothing after: {bodies:?}");
    assert!(bodies[1].contains("was reviewed by bird-codex (bird's agent) and is ready"), "{bodies:?}");
    assert!(!bodies.iter().any(|b| b.contains("could not start")), "{bodies:?}");
}

#[test]
fn a_settled_head_is_followed_again_when_the_level_or_the_head_changes() {
    let fake = Fake::new(at_assist(HEAD, "assist"));
    ready_line_posted(&fake, HEAD);
    assert!(block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap());
    // Autonomous now: one more approval would merge, so the review loop goes on.
    *fake.readiness.borrow_mut() = at_assist(HEAD, "merge");
    assert!(!block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap());
    // A new push: its head was never settled, so its review is requested.
    *fake.readiness.borrow_mut() = at_assist(MOVED, "assist");
    assert!(!block(advance(&fake, &target(MOVED), Some(&bird_codex()), &Facts::default())).unwrap());
    assert!(matches!(fake.posted.borrow().last(), Some(LoopNotice::ReviewRequested { target, .. }) if target.head == MOVED));
}

/// 23:24: "Review of PR #121 … could not start — no run of it reached this
/// device" was said of bird-codex, whose runs only bird's device sees.
#[test]
fn pr_121_another_owners_reviewer_is_judged_by_its_github_verdict_never_by_this_device() {
    let fake = Fake::new(at_assist(HEAD, "merge"));
    assert!(!block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap());
    assert_eq!(fake.asked_due.borrow()[0]["remote"], true, "the ledger is told it is another owner's Agent");
    // Its verdict of the head is on GitHub: answered, so its review step is not judged at all.
    let asked = fake.asked_due.borrow().len();
    block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts { answered: true })).unwrap();
    assert_eq!(fake.asked_due.borrow().len(), asked);
    // No verdict after the window: the ledger's one line, in plain words.
    *fake.due.borrow_mut() = json!({"taken": true, "again": 0, "say": {"why": "unanswered", "gaveUp": true, "round": 0}});
    block(advance(&fake, &target(HEAD), Some(&bird_codex()), &Facts::default())).unwrap();
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 2, "the request and one line, never a re-send: {bodies:?}");
    assert_eq!(bodies[1], "⏳ bird-codex (bird's agent) has not answered the review request for PR #121 yet: https://github.com/csheargm/hagency-demo-sandbox/pull/121");
    assert!(!bodies.iter().any(|b| b.contains("this device") || b.contains("could not start")), "{bodies:?}");
}

/// 23:26: bird-claude (bird's other Agent) was asked after bird-codex was
/// passed over; its verdicts cannot be recorded here either.
#[test]
fn after_another_owners_agent_is_passed_over_only_own_agents_are_asked() {
    let agent = |id: &str, own: bool| Candidate { puppet: OwnedUserId::try_from(format!("@_hagency_agt_{id}:localhost:8452")).unwrap(), name: id.into(), runtime: Some("codex"), own };
    let mut peers = Peers { candidates: vec![agent("bird_codex", false), agent("bird_claude", false)], author: Some("claude"), prefer_different: true, complete: true };
    let passed = ["@_hagency_agt_bird_codex:localhost:8452".to_owned()];
    assert_eq!(peers.pick(&[]).map(|p| p.reviewer.localpart().to_owned()).as_deref(), Some("_hagency_agt_bird_codex"));
    assert_eq!(peers.pick(&passed), None, "no own Agent: stop, never bird-claude");
    peers.candidates.push(agent("anna_codex", true));
    let next = peers.pick(&passed).expect("an own Agent");
    assert!(next.own && next.reviewer.localpart() == "_hagency_agt_anna_codex", "{next:?}");
}

#[test]
fn a_verdict_of_the_head_is_an_approval_or_a_change_request_of_that_head() {
    let review = |state: &str, commit: &str| vec![("csheargm".to_owned(), state.to_owned(), commit.to_owned())];
    assert!(verdict_of(&review(::hagency::issue_watch::APPROVE_VERDICT, HEAD), HEAD));
    assert!(verdict_of(&review("CHANGES_REQUESTED", HEAD), HEAD));
    assert!(!verdict_of(&review("COMMENTED", HEAD), HEAD), "a plain comment answers nothing");
    assert!(!verdict_of(&review("APPROVED", MOVED), HEAD), "another head's verdict");
}

/// Bird's window, Nov 18 00:12: the waiting line under the review request
/// stayed after the review was recorded. GPT batch review, Nov 19: only a
/// review recorded by the asked reviewer answers it, never another Agent's
/// approval, the room's level, or a switched reviewer.
#[test]
fn only_the_asked_reviewers_recorded_review_answers_its_request() {
    use super::super::super::loop_notice::answered;
    const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
    let codex = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-codex:localhost:8452"), notice: None, own: true };
    let claude = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-claude:localhost:8452"), notice: None, own: true };
    let pr = Target { number: 122, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/122".into(), ..target(HEAD) };
    let request = |t: &Target, peer: &Pick| LoopNotice::ReviewRequested { target: t.clone(), reviewer: peer.reviewer.clone() }.body();
    let by = |head: &str, who: &[&Pick]| {
        let mut r = at_assist(head, "assist");
        r["recorded"] = json!(who.len());
        r["recordedBy"] = json!(who.iter().map(|p| p.reviewer.as_str()).collect::<Vec<_>>());
        r
    };
    super::note_review(ROOM, &pr, &by(HEAD, &[]));
    assert!(!answered(ROOM, &request(&pr, &codex)), "asked, nothing recorded: still waiting");
    super::note_review(ROOM, &pr, &by(HEAD, &[&claude]));
    assert!(!answered(ROOM, &request(&pr, &codex)), "another Agent approved the head: bird-codex still waits");
    let level_only = json!({"decision": "human", "head": HEAD, "reason": ASSIST_REASON, "kind": "autonomy", "recorded": 1, "recordedBy": [claude.reviewer.as_str()]});
    super::note_review(ROOM, &pr, &level_only);
    assert!(!answered(ROOM, &request(&pr, &codex)), "only the level holds, approved by another: still waiting");
    super::note_review(ROOM, &Target { head: MOVED.into(), ..pr.clone() }, &by(HEAD, &[&codex]));
    assert!(!answered(ROOM, &request(&Target { head: MOVED.into(), ..pr.clone() }, &codex)), "a record of another head answers nothing");
    super::note_review(ROOM, &pr, &by(HEAD, &[&codex]));
    assert!(answered(ROOM, &request(&pr, &codex)), "bird-codex's own review recorded: answered");
    assert!(answered(ROOM, &request(&pr, &claude)), "bird-claude's own recorded review answers its own request");
    let gemini = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-gemini:localhost:8452"), notice: None, own: true };
    assert!(!answered(ROOM, &request(&pr, &gemini)), "a switched reviewer with no recorded review still waits");
    assert!(!answered("!other:localhost:8450", &request(&pr, &codex)), "another room's same request is its own");
}

/// The Assist-settled head notes only the asked reviewer whose review is recorded.
#[test]
fn a_settled_head_answers_only_the_reviewer_whose_review_is_recorded() {
    use super::super::super::loop_notice::answered;
    const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
    let pr = target(HEAD);
    let codex = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-codex:localhost:8452"), notice: None, own: true };
    let mut ready = at_assist(HEAD, "assist");
    ready["recordedBy"] = json!(["@_hagency_bird_bird-claude:localhost:8452"]);
    let fake = Fake::new(ready);
    ready_line_posted(&fake, HEAD);
    assert!(block(advance(&fake, &pr, Some(&codex), &Facts::default())).unwrap(), "settled");
    let request = LoopNotice::ReviewRequested { target: pr.clone(), reviewer: codex.reviewer.clone() }.body();
    assert!(!answered(ROOM, &request), "settled by another's review: bird-codex is not answered");
    fake.readiness.borrow_mut()["recordedBy"] = json!([codex.reviewer.as_str()]);
    assert!(block(advance(&fake, &pr, Some(&codex), &Facts::default())).unwrap());
    assert!(answered(ROOM, &request), "its own review recorded: answered");
}

/// Polish round 3 (Nov 19): the loop rotated from bird-codex to bird-claude,
/// then bird-codex's late review is recorded. Its original request is
/// answered; bird-claude's request still waits.
#[test]
fn a_rotated_away_reviewers_late_review_answers_its_own_request() {
    use super::super::super::loop_notice::answered;
    const ROOM: &str = "!rotated:localhost:8450";
    let codex = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-codex:localhost:8452"), notice: None, own: true };
    let claude = Pick { reviewer: owned_user_id!("@_hagency_bird_bird-claude:localhost:8452"), notice: None, own: true };
    let pr = Target { number: 123, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/123".into(), ..target(HEAD) };
    let request = |peer: &Pick| LoopNotice::ReviewRequested { target: pr.clone(), reviewer: peer.reviewer.clone() }.body();
    let mut late = at_assist(HEAD, "assist");
    late["recorded"] = json!(1);
    late["recordedBy"] = json!([codex.reviewer.as_str()]);
    super::note_review(ROOM, &pr, &late);
    assert!(answered(ROOM, &request(&codex)), "bird-codex's late review recorded: its line is gone");
    assert!(!answered(ROOM, &request(&claude)), "bird-claude, the current pick, still waits");
}
