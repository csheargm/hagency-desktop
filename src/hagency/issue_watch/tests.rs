//! The hand-off step with a fake sender: an issue is recorded as handed off
//! only after its post succeeded, and the first failure stops the rest.
use super::handoff::{Handoff, Outcome, hand_off, request_text};
use super::{failed_line, outcome_line};
use ::hagency::issue_watch::Issue;
use std::cell::RefCell;

#[derive(Default)]
struct Fake {
    fail_post: Option<i64>,
    fail_mark: Option<i64>,
    posted: RefCell<Vec<String>>,
    seen: RefCell<Vec<i64>>,
}
impl Handoff for Fake {
    async fn post(&self, _number: i64, text: &str) -> Result<(), String> {
        if self.fail_post.is_some_and(|n| text.ends_with(&format!("/issues/{n}"))) {
            return Err("room not joined".into());
        }
        self.posted.borrow_mut().push(text.to_owned());
        Ok(())
    }
    async fn mark_seen(&self, number: i64) -> Result<(), String> {
        if self.fail_mark == Some(number) {
            return Err("ledger locked".into());
        }
        self.seen.borrow_mut().push(number);
        Ok(())
    }
}
fn issue(number: i64) -> Issue {
    Issue {
        number,
        url: format!("https://github.com/acme/app/issues/{number}"),
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
    assert_eq!(fake.posted.borrow()[0], "please fix https://github.com/acme/app/issues/1");
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
    assert_eq!(request_text(&issue(5)), "please fix https://github.com/acme/app/issues/5");
}

#[test]
fn a_retried_issue_reuses_its_transaction_id() {
    use super::live::handoff_txn_id;
    assert_eq!(handoff_txn_id("a", "acme/app", 7), handoff_txn_id("a", "acme/app", 7), "send ok + save lost: the retry is deduplicated");
    assert_ne!(handoff_txn_id("a", "acme/app", 7), handoff_txn_id("a", "acme/app", 8));
    assert_ne!(handoff_txn_id("a", "acme/app", 7), handoff_txn_id("b", "acme/app", 7));
}

#[test]
fn the_hand_off_mentions_the_agent_with_a_real_pill() {
    use matrix_sdk::ruma::events::room::message::MessageType;
    let user: &matrix_sdk::ruma::UserId = "@fixer:example.org".try_into().unwrap();
    let content = crate::hagency::room_send::message_content("please fix x", user, "Fixer");
    let MessageType::Text(text) = &content.msgtype else { panic!("text message") };
    assert!(text.formatted.is_some(), "rendered like the composer's mention pill");
    assert_eq!(content.mentions.unwrap().user_ids.len(), 1, "a real m.mentions entry");
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
fn issue_watch_modes_map_to_saved_modes() {
    use super::choices::{MODE_LABELS, hint, index};
    assert_eq!(MODE_LABELS[index("all")], "Fully automatic: every new issue");
    assert_eq!(index(""), 0, "never set means Off");
    assert_eq!(index("labelled"), 1);
    assert_eq!(index("trusted"), 2);
    let hints: std::collections::HashSet<_> = (0..4).map(hint).collect();
    assert_eq!(hints.len(), 4, "every mode explains itself");
}

#[test]
fn issue_watch_change_needs_a_repo_and_room_unless_off() {
    use super::choices::change;
    use serde_json::json;
    let saved = change(2, " acme/app ", Some("!r:s")).unwrap();
    assert_eq!(saved, json!({"issueWatch": {"mode": "trusted", "label": "agent-fix", "repo": "acme/app", "roomId": "!r:s"}}));
    assert_eq!(change(0, "", None).unwrap()["issueWatch"]["mode"], "off");
    assert!(change(3, "acme", Some("!r:s")).is_err());
    assert!(change(3, "acme/app/x", Some("!r:s")).is_err());
    assert!(change(1, "acme/app", None).is_err());
}

#[test]
fn only_active_room_bindings_are_offered() {
    use serde_json::json;
    let bindings = [
        json!({"roomId": "!a:s", "state": "active"}),
        json!({"roomId": "!b:s", "state": "paused"}),
        json!({"roomId": "#alias:s", "state": "active"}),
    ];
    let rooms = super::choices::rooms(&bindings, |b| format!("name {}", b["roomId"].as_str().unwrap()));
    assert_eq!(rooms, vec![("!a:s".to_owned(), "name !a:s".to_owned())]);
}

#[test]
fn the_token_comes_from_the_environment_or_the_error_says_how() {
    use super::{TOKEN_VARIABLES, github_token};
    if TOKEN_VARIABLES.iter().all(|name| std::env::var(name).is_err()) {
        assert!(github_token().unwrap_err().contains("GH_TOKEN"));
    }
}

fn section(cx: &mut makepad_widgets::Cx) -> super::section::IssueWatchSection {
    use makepad_widgets::*;
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.IssueWatchSection {}});
        let section = super::section::IssueWatchSection::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        section
    })
}

#[test]
fn saving_issue_watching_says_saved() {
    use super::section::test_support::{answer, stale_answer};
    use makepad_widgets::*;
    use serde_json::json;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = section(&mut cx);
    let rooms = vec![("!r:s".to_owned(), "Project".to_owned())];
    panel.set_context(&mut cx, "agt_1", true, rooms);
    let saved = json!({"issueWatch": {"mode": "trusted", "label": "agent-fix", "repo": "acme/app", "roomId": "!r:s", "enabledAt": 5}, "issueSeen": []});
    answer(&mut panel, &mut cx, false, Ok(saved.clone()));
    assert_eq!(panel.label(&mut cx, ids!(watch_note)).text(), "", "a read is not a save");
    assert_eq!(panel.text_input(&mut cx, ids!(watch_repo)).text(), "acme/app");
    assert!(panel.button(&mut cx, ids!(save_watch)).borrow().unwrap().enabled());
    stale_answer(&mut panel, &mut cx, Ok(saved.clone()));
    assert_eq!(panel.label(&mut cx, ids!(watch_note)).text(), "", "a stale reply shows nothing");
    answer(&mut panel, &mut cx, true, Ok(saved));
    assert_eq!(panel.label(&mut cx, ids!(watch_note)).text(), "Saved");
    answer(&mut panel, &mut cx, true, Err("ledger locked".into()));
    assert_eq!(panel.label(&mut cx, ids!(watch_note)).text(), "Could not save issue watching: ledger locked");
}

#[test]
fn another_device_cannot_edit_the_watch() {
    use makepad_widgets::*;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = section(&mut cx);
    panel.set_context(&mut cx, "agt_1", false, Vec::new());
    assert!(!panel.button(&mut cx, ids!(save_watch)).borrow().unwrap().enabled());
    assert!(panel.label(&mut cx, ids!(watch_status)).text().contains("runs on the device"));
}
