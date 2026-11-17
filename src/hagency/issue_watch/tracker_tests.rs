//! The issue loop's follow-through as user flows, on fakes (R6): a lost run
//! is retried across a restart and then given up; an Agent's pull request is
//! followed to review and its merge announced once; a give-up stops retries.
//! The fake store applies changes with the ledger's own rule (`apply_change`),
//! and a "restart" is a fresh fake built from the stored records alone.
use super::tracker::{Followed, Inputs, TrackIo, follow, links_issue};
use ::hagency::issue_watch::issue_track::{HANDOFF_TIMEOUT_SECS, IssueFact, MAX_ATTEMPTS, State, Track, TrackChange, apply_change};
use ::hagency::issue_watch::{Issue, PullRequest};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

const REPO: &str = "acme/app";
const T0: i64 = 1_700_000_000;

#[derive(Default)]
struct Fake {
    now: Cell<i64>,
    store: RefCell<HashMap<i64, Track>>,
    /// Every room line, keyed as posted; a repeated key is what the homeserver drops.
    said: RefCell<Vec<(i64, String, String)>>,
    handed: RefCell<Vec<(i64, u32)>>,
    followed: RefCell<Vec<u64>>,
    closed: RefCell<HashMap<i64, IssueFact>>,
    cannot_fix: RefCell<HashSet<i64>>,
    peer: bool,
}
impl Fake {
    /// The same stored records, nothing else: what survives a restart.
    fn restarted(&self) -> Self {
        Fake { store: RefCell::new(self.store.borrow().clone()), peer: self.peer, ..Fake::default() }
    }
    fn tracks(&self) -> Vec<Track> {
        let mut all: Vec<Track> = self.store.borrow().values().cloned().collect();
        all.sort_by_key(|t| t.number);
        all
    }
    fn keys(&self) -> Vec<String> {
        self.said.borrow().iter().map(|(n, k, _)| format!("{n}:{k}")).collect()
    }
}
impl TrackIo for Fake {
    async fn record(&self, change: Value) -> Result<(), String> {
        let change: TrackChange = serde_json::from_value(change).map_err(|e| e.to_string())?;
        let current = self.store.borrow().get(&change.number).cloned();
        let next = apply_change(current, &change, self.now.get()).map_err(|e| e.to_string())?;
        self.store.borrow_mut().insert(change.number, next);
        Ok(())
    }
    async fn say(&self, number: i64, key: &str, text: String) -> Result<(), String> {
        if !self.said.borrow().iter().any(|(n, k, _)| *n == number && k == key) {
            self.said.borrow_mut().push((number, key.to_owned(), text));
        }
        Ok(())
    }
    async fn hand_off(&self, issue: &Issue, attempt: u32) -> Result<(), String> {
        self.handed.borrow_mut().push((issue.number, attempt));
        self.record(serde_json::json!({"repo": REPO, "number": issue.number, "handed": attempt})).await
    }
    async fn closed_state(&self, number: i64) -> Result<IssueFact, String> {
        self.closed.borrow().get(&number).cloned().ok_or_else(|| "not found".to_owned())
    }
    async fn cannot_fix(&self, track: &Track) -> Result<bool, String> {
        Ok(self.cannot_fix.borrow().contains(&track.number))
    }
    async fn follow_pr(&self, pr: &PullRequest) -> Result<Followed, String> {
        self.followed.borrow_mut().push(pr.number);
        Ok(if self.peer { Followed::Requested } else { Followed::NeedsPerson })
    }
    fn paused(&self) -> bool {
        false
    }
}

fn issue(number: i64) -> Issue {
    Issue { number, url: format!("https://github.com/{REPO}/issues/{number}"), title: String::new(), labels: vec![], is_pr: false, author_association: "OWNER".into(), created_at: T0 }
}
fn pull(number: u64, body: &str) -> PullRequest {
    PullRequest { number, url: format!("https://github.com/{REPO}/pull/{number}"), draft: false, head_sha: "1".repeat(40), author: "anna".into(), created_at: T0, body: body.into(), head_ref: "docs/x".into() }
}
fn check(fake: &Fake, now: i64, open: &[Issue], pulls: &[PullRequest]) -> Vec<String> {
    fake.now.set(now);
    let tracks = fake.tracks();
    let seen: HashSet<i64> = tracks.iter().map(|t| t.number).collect();
    let inputs = Inputs { repo: REPO, now, tracks: &tracks, seen: &seen, open_issues: open, own_pulls: pulls };
    let out = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow(fake, &inputs));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    out.lines
}
fn first_hand_off(fake: &Fake, number: i64) {
    fake.now.set(T0);
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(fake.hand_off(&issue(number), 1)).unwrap();
    fake.handed.borrow_mut().clear(); // only the tracker's own hand-offs are counted below
}

#[test]
fn a_lost_run_is_retried_across_a_restart_then_given_up_once() {
    let fake = Fake::default();
    first_hand_off(&fake, 33);
    let open = [issue(33)];
    assert_eq!(check(&fake, T0 + 60, &open, &[]), ["#33 waiting for a pull request"]);
    // The run died (servers restarted): no pull request after the timeout.
    let fake = fake.restarted();
    assert_eq!(check(&fake, T0 + HANDOFF_TIMEOUT_SECS, &open, &[]), ["#33 retrying (2/3)"]);
    assert_eq!(*fake.handed.borrow(), [(33, 2)]);
    assert!(fake.said.borrow()[0].2.contains("Retrying #33 (attempt 2 of 3)"));
    // Restart again mid-attempt: the stored attempt resumes, no double retry.
    let fake = fake.restarted();
    let t2 = fake.tracks()[0].handed_at;
    assert_eq!(check(&fake, t2 + HANDOFF_TIMEOUT_SECS, &open, &[]), ["#33 retrying (2/3)"]);
    assert!(fake.handed.borrow().is_empty(), "the second attempt waits twice as long");
    check(&fake, t2 + 2 * HANDOFF_TIMEOUT_SECS, &open, &[]);
    let t3 = fake.tracks()[0].handed_at;
    assert_eq!(check(&fake, t3 + 4 * HANDOFF_TIMEOUT_SECS, &open, &[]), ["#33 gave up after 3 attempts"]);
    for later in 1..5 {
        check(&fake, t3 + later * 100 * HANDOFF_TIMEOUT_SECS, &open, &[]);
    }
    assert_eq!(*fake.handed.borrow(), [(33, 3)], "never more than MAX_ATTEMPTS hand-offs");
    assert_eq!(fake.keys(), ["33:retry-3", "33:failed"]);
    assert_eq!(fake.tracks()[0].state, State::Failed);
    assert_eq!(fake.tracks()[0].attempts, MAX_ATTEMPTS);
}

#[test]
fn the_agents_pull_request_is_followed_to_review_and_its_merge_announced_once() {
    let fake = Fake { peer: true, ..Fake::default() };
    first_hand_off(&fake, 31);
    let pr = pull(32, "Adds CONTRIBUTING.md\n\nCloses #31");
    let lines = check(&fake, T0 + 10 * HANDOFF_TIMEOUT_SECS, &[issue(31)], std::slice::from_ref(&pr));
    assert_eq!(lines, ["#31 PR #32 in review"]);
    assert!(fake.handed.borrow().is_empty(), "an open pull request is never retried");
    let fake = fake.restarted();
    check(&fake, T0 + 11 * HANDOFF_TIMEOUT_SECS, &[issue(31)], std::slice::from_ref(&pr));
    assert_eq!(*fake.followed.borrow(), [32], "each check carries it on (the review loop posts once per head)");
    assert_eq!((fake.tracks()[0].state, fake.tracks()[0].pr), (State::Reviewed, Some(32)));
    // Merged: the issue closed as completed and left the open listing.
    fake.closed.borrow_mut().insert(31, IssueFact::Done);
    assert!(check(&fake, T0 + 12 * HANDOFF_TIMEOUT_SECS, &[], &[]).is_empty());
    check(&fake, T0 + 13 * HANDOFF_TIMEOUT_SECS, &[], &[]);
    assert_eq!(fake.said.borrow().iter().map(|s| s.2.as_str()).collect::<Vec<_>>(), ["✅ #31 fixed → PR #32 merged"]);
    assert_eq!(fake.tracks()[0].state, State::Merged);
}

#[test]
fn without_another_agent_a_person_is_asked_once() {
    let fake = Fake::default();
    first_hand_off(&fake, 31);
    let pr = pull(32, "fixes acme/app#31");
    for minute in 1..4 {
        assert_eq!(check(&fake, T0 + minute * 60, &[issue(31)], std::slice::from_ref(&pr)), ["#31 PR #32 waiting for your review"]);
    }
    assert_eq!(fake.keys(), ["31:needs-review-32"]);
    assert!(fake.said.borrow()[0].2.contains("PR #32 for #31 needs your review and merge approval: https://github.com/acme/app/pull/32"));
}

#[test]
fn a_cannot_fix_comment_stops_the_loop_for_a_person() {
    let fake = Fake::default();
    first_hand_off(&fake, 40);
    fake.cannot_fix.borrow_mut().insert(40);
    assert_eq!(check(&fake, T0 + 60, &[issue(40)], &[]), ["#40 needs a person"]);
    check(&fake, T0 + 100 * HANDOFF_TIMEOUT_SECS, &[issue(40)], &[]);
    assert!(fake.handed.borrow().is_empty(), "no retry after a give-up");
    assert_eq!(fake.keys(), ["40:needs-human"]);
}

#[test]
fn older_hand_offs_are_adopted_and_closed_issues_end_quietly() {
    let fake = Fake::default();
    fake.now.set(T0);
    let seen = HashSet::from([33, 34]);
    let open = [issue(33)];
    let inputs = Inputs { repo: REPO, now: T0, tracks: &[], seen: &seen, open_issues: &open, own_pulls: &[] };
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow(&fake, &inputs));
    assert_eq!(fake.tracks().iter().map(|t| (t.number, t.attempts, t.handed_at)).collect::<Vec<_>>(), [(33, 1, T0)], "only open seen issues");
    fake.closed.borrow_mut().insert(33, IssueFact::ClosedOther);
    assert!(check(&fake, T0 + 60, &[], &[]).is_empty());
    assert_eq!(fake.tracks()[0].state, State::Closed);
    assert!(fake.said.borrow().is_empty());
}

#[test]
fn pull_requests_link_by_closing_keyword_or_issue_branch_only() {
    let with = |body: &str, branch: &str| PullRequest { head_ref: branch.into(), ..pull(1, body) };
    assert!(links_issue(&with("Closes #33", "x"), REPO, 33));
    assert!(links_issue(&with("", "fix/issue-33"), REPO, 33));
    assert!(links_issue(&with("", "issue33-readme"), REPO, 33));
    assert!(links_issue(&with("", "agent/33-readme"), REPO, 33));
    assert!(!links_issue(&with("", "fix-3-typos"), REPO, 3));
    assert!(!links_issue(&with("", "issue-333"), REPO, 33));
    assert!(!links_issue(&with("Closes #330", "x"), REPO, 33));
}
