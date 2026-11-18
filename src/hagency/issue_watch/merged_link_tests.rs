//! An issue whose pull request was opened and merged between two checks
//! (integration GPT review): the merge is still announced, recorded with its
//! pull request and counted as the owner's.
use super::merged_link::pick;
use super::tracker_tests::{Fake, REPO, T0, check, first_hand_off};
use ::hagency::issue_watch::PullRequest;
use ::hagency::issue_watch::issue_track::{IssueFact, State, Track};

fn pull(number: u64, author: &str, body: &str) -> PullRequest {
    PullRequest { number, url: format!("https://github.com/{REPO}/pull/{number}"), draft: false, head_sha: "a".repeat(40), author: author.into(), created_at: T0 + 30, body: body.into(), head_ref: "main-fix".into() }
}
fn track(number: i64) -> Track {
    Track { repo: REPO.into(), number, state: State::HandedOff, attempts: 1, handed_at: T0, pr: None, notified: false, free_retries: 0, changed_at: T0 }
}

#[test]
fn a_pull_request_merged_before_any_check_saw_it_is_still_the_fix() {
    let fake = Fake::default();
    first_hand_off(&fake, 33);
    fake.closed.borrow_mut().insert(33, IssueFact::Done);
    fake.merged_link.borrow_mut().insert(33, 12);
    fake.merged.borrow_mut().insert(12);
    check(&fake, T0 + 60, &[], &[]);
    let track = &fake.tracks()[0];
    assert_eq!((track.state, track.pr), (State::Merged, Some(12)), "not \"closed\" with no pull request");
    assert!(fake.said.borrow().iter().any(|(n, k, text)| *n == 33 && k == "fixed" && text.contains("PR #12 merged")));
}

#[test]
fn an_issue_closed_by_hand_with_no_linked_merge_is_closed() {
    let fake = Fake::default();
    first_hand_off(&fake, 34);
    fake.closed.borrow_mut().insert(34, IssueFact::Done);
    check(&fake, T0 + 60, &[], &[]);
    assert_eq!((fake.tracks()[0].state, fake.tracks()[0].pr), (State::Closed, None));
}

#[test]
fn only_the_agents_own_pull_request_that_links_the_issue_is_picked() {
    let pulls = [pull(20, "mallory", "Closes #33"), pull(19, "betty", "Fixes #32"), pull(18, "Betty", "Closes #33"), pull(17, "betty", "Closes #33")];
    assert_eq!(pick(&pulls, (REPO, "betty"), &track(33)), Some(18), "the newest of the Agent's own");
    assert_eq!(pick(&pulls, (REPO, "betty"), &track(31)), None);
    // A reopened issue: the earlier fix predates this hand-off (GPT review).
    let old = [PullRequest { created_at: T0 - 86_400, ..pull(9, "betty", "Closes #33") }];
    assert_eq!(pick(&old, (REPO, "betty"), &track(33)), None, "an earlier hand-off's fix never counts again");
}
