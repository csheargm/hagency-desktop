//! The hand-off step with a fake sender: an issue is recorded as handed off
//! only after its post succeeded, and the first failure stops the rest.
use super::handoff::{Handoff, Outcome, hand_off};
use super::{failed_line, outcome_line};
use ::hagency::issue_watch::Issue;
use std::cell::RefCell;

#[derive(Default)]
struct Fake {
    fail_post: Option<i64>,
    fail_mark: Option<i64>,
    posted: RefCell<Vec<String>>,
    seen: RefCell<Vec<i64>>,
    /// Paused from the start, or by Stop All right after this issue's post.
    paused: std::cell::Cell<bool>,
    pause_after_post: Option<i64>,
}
impl Handoff for Fake {
    async fn post(&self, issue: &Issue) -> Result<(), String> {
        if self.fail_post == Some(issue.number) {
            return Err("room not joined".into());
        }
        self.posted.borrow_mut().push(issue.url.clone());
        if self.pause_after_post == Some(issue.number) {
            self.paused.set(true);
        }
        Ok(())
    }
    async fn mark_seen(&self, number: i64) -> Result<(), String> {
        if self.fail_mark == Some(number) {
            return Err("ledger locked".into());
        }
        self.seen.borrow_mut().push(number);
        Ok(())
    }
    fn paused(&self) -> bool {
        self.paused.get()
    }
}
fn issue(number: i64) -> Issue {
    Issue {
        number,
        url: format!("https://github.com/acme/app/issues/{number}"),
        title: format!("Issue {number}"),
        labels: vec![],
        is_pr: false,
        author_association: "MEMBER".into(),
        created_at: 10,
    }
}
fn run(fake: &Fake, issues: &[Issue]) -> Outcome {
    let picks: Vec<&Issue> = issues.iter().collect();
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(hand_off(fake, &picks))
}

#[test]
fn successful_posts_are_marked_seen() {
    let fake = Fake::default();
    let outcome = run(&fake, &[issue(1), issue(2)]);
    assert_eq!(outcome, Outcome { handed: 2, error: None });
    assert_eq!(*fake.seen.borrow(), vec![1, 2]);
    assert_eq!(fake.posted.borrow()[0], "https://github.com/acme/app/issues/1");
}

#[test]
fn a_failed_post_is_not_marked_seen_and_stops_the_rest() {
    let fake = Fake { fail_post: Some(2), ..Default::default() };
    let outcome = run(&fake, &[issue(1), issue(2), issue(3)]);
    assert_eq!(outcome.handed, 1);
    assert_eq!(*fake.seen.borrow(), vec![1], "issue 2 is retried next check; 3 waits");
    assert!(outcome.error.unwrap().contains("#2: room not joined"));
}

#[test]
fn a_failed_record_is_reported_not_swallowed() {
    let fake = Fake { fail_mark: Some(1), ..Default::default() };
    let outcome = run(&fake, &[issue(1), issue(2)]);
    assert_eq!(outcome.handed, 0);
    assert_eq!(fake.posted.borrow().len(), 1, "nothing more is posted after a failed record");
    assert!(outcome.error.unwrap().contains("handed off but not recorded"));
}

#[test]
fn nothing_to_hand_off_posts_nothing() {
    let fake = Fake::default();
    assert_eq!(run(&fake, &[]), Outcome::default());
    assert!(fake.posted.borrow().is_empty());
}

#[test]
fn status_lines_say_what_happened() {
    assert_eq!(outcome_line("14:05", &Outcome { handed: 2, error: None }), "Last check 14:05: 2 issues handed off");
    assert_eq!(outcome_line("14:05", &Outcome { handed: 1, error: None }), "Last check 14:05: 1 issue handed off");
    assert_eq!(outcome_line("14:05", &Outcome::default()), "Last check 14:05: no new issues");
    let partial = Outcome { handed: 1, error: Some("Could not hand off issue #2: x".into()) };
    assert_eq!(outcome_line("9:00", &partial), "Last check 9:00: 1 issue handed off · Could not hand off issue #2: x");
    assert_eq!(failed_line("9:00", "GitHub answered 401: bad"), "Last check 9:00 failed: GitHub answered 401: bad");
}

#[test]
fn a_retried_issue_reuses_its_transaction_id() {
    use super::live::handoff_txn_id;
    assert_eq!(handoff_txn_id("a", "acme/app", 7, 1, 0), handoff_txn_id("a", "acme/app", 7, 1, 0), "send ok + save lost: the retry is deduplicated");
    assert_ne!(handoff_txn_id("a", "acme/app", 7, 1, 0), handoff_txn_id("a", "acme/app", 8, 1, 0));
    assert_ne!(handoff_txn_id("a", "acme/app", 7, 1, 0), handoff_txn_id("b", "acme/app", 7, 1, 0));
    assert_ne!(handoff_txn_id("a", "acme/app", 7, 1, 0), handoff_txn_id("a", "acme/app", 7, 2, 0), "a retry is a new message");
    assert_ne!(handoff_txn_id("a", "acme/app", 7, 3, 0), handoff_txn_id("a", "acme/app", 7, 3, 1), "a re-post after a restart is a new message");
    assert_ne!(handoff_txn_id("a", "acme/app", 7, 3, 1), handoff_txn_id("a", "acme/app", 7, 3, 2));
}

#[test]
fn a_malformed_agents_reply_is_an_error_not_an_empty_list() {
    use super::active_agents;
    use serde_json::json;
    for bad in [json!({}), json!({"agents": null}), json!({"agents": "x"})] {
        assert!(active_agents(&bad).is_err(), "{bad}");
    }
    let listed = json!({"agents": [{"id": "a", "state": "active"}, {"id": "b", "state": "paused"}]});
    assert_eq!(active_agents(&listed).unwrap().len(), 1);
}

#[test]
fn a_check_from_an_ended_session_records_nothing_and_stale_agents_are_pruned() {
    use super::{prune, record, status_line, still_current};
    let epoch = crate::account_session::epoch();
    record(epoch + 1, "old-session", Some("stale".into()));
    assert_eq!(status_line("old-session"), None, "an old check cannot write into this session");
    assert!(still_current(epoch + 1).is_err(), "an old check stops before sending");
    record(epoch, "gone", Some("Last check".into()));
    record(epoch, "kept", Some("Last check".into()));
    prune(epoch, &std::collections::HashSet::from(["kept".to_owned()]));
    assert_eq!(status_line("gone"), None);
    assert_eq!(status_line("kept").as_deref(), Some("Last check"));
}

#[test]
fn the_agents_token_wins_then_the_environment_then_the_error_says_where() {
    use super::{NO_TOKEN, choose_token};
    use ::hagency::github_access::Token;
    let agent = || Ok(Some(Token::parse("agent-token").unwrap()));
    let env = |name: &str| (name == "GITHUB_TOKEN").then(|| "env-token".to_owned());
    let none = |_: &str| None;
    assert_eq!(choose_token(agent(), env).unwrap(), "agent-token");
    assert_eq!(choose_token(Ok(None), env).unwrap(), "env-token");
    let gh_first = |name: &str| Some(format!("{name}-value"));
    assert_eq!(choose_token(Ok(None), gh_first).unwrap(), "GH_TOKEN-value");
    assert_eq!(choose_token(Ok(None), none).unwrap_err(), crate::i18n::tr(NO_TOKEN));
    assert_eq!(choose_token(Err("store locked".into()), env).unwrap(), "env-token");
    assert_eq!(choose_token(Err("store locked".into()), none).unwrap_err(), "store locked");
}

#[test]
fn a_paused_agent_gets_no_post_and_nothing_is_marked_seen() {
    let fake = Fake { paused: true.into(), ..Default::default() };
    let outcome = run(&fake, &[issue(1), issue(2)]);
    assert_eq!(outcome.handed, 0);
    assert!(fake.posted.borrow().is_empty() && fake.seen.borrow().is_empty());
    assert!(outcome.error.unwrap().contains("issue #1 waits until it is resumed"));
}

#[test]
fn stop_all_between_post_and_record_leaves_the_issue_for_after_resume() {
    let fake = Fake { pause_after_post: Some(2), ..Default::default() };
    let outcome = run(&fake, &[issue(1), issue(2), issue(3)]);
    assert_eq!(outcome.handed, 1);
    assert_eq!(*fake.seen.borrow(), vec![1], "#2 was posted but stays unseen, so it is handed off again after Resume");
    assert_eq!(fake.posted.borrow().len(), 2, "#3 is never posted");
}

#[test]
fn only_unreachable_checks_count_toward_a_fresh_client() {
    let unreachable = ::hagency::issue_watch::FetchError::Unreachable("dns error".into()).to_string();
    assert_eq!(super::next_streak(0, &Err(&unreachable)), 1);
    assert_eq!(super::next_streak(2, &Err(&format!("#33: {unreachable}"))), 3);
    assert_eq!(super::next_streak(2, &Err("GitHub answered 401: bad")), 2);
    assert_eq!(super::next_streak(2, &Ok(())), 0);
}

#[test]
fn an_unreachable_github_reads_plainly_in_the_status_line() {
    let error = ::hagency::issue_watch::FetchError::Unreachable("error sending request: dns error: failed to lookup".into());
    let line = failed_line("9:00", &error.to_string());
    assert_eq!(line, "Last check 9:00 failed: Can't reach GitHub (network/DNS): error sending request: dns error: failed to lookup \u{2014} retrying");
}

#[test]
fn a_repository_on_the_work_list_is_not_handed_off_again_by_the_single_watch() {
    use ::hagency::issue_watch::{IssueWatch, WatchMode, WatchSetting};
    let watch = |mode| IssueWatch { setting: WatchSetting { mode, label: String::new(), repo: "Acme/App".into(), room_id: "!r:x".into(), push_trusted: false }, enabled_at: 0 };
    let listed = serde_json::json!({"work": [{"repo": "acme/app"}]});
    let elsewhere = serde_json::json!({"work": [{"repo": "acme/other"}]});
    assert!(!super::single_repo_watch_runs(&watch(WatchMode::All), &listed), "the work list's claims and limits decide");
    assert!(super::single_repo_watch_runs(&watch(WatchMode::All), &elsewhere));
    assert!(super::single_repo_watch_runs(&watch(WatchMode::All), &serde_json::json!({})));
    assert!(!super::single_repo_watch_runs(&watch(WatchMode::Off), &elsewhere));
}
