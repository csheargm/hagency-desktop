//! The daily digest as user flows, on fakes (R6): a day of tracker moves
//! (recorded with the ledger's own rule, `apply_change`) becomes one notice
//! with the right counts and links; it is sent once per day even across a
//! restart; a quiet day says nothing; only the owner's own window sends.
use super::digest::{self, MAX_ITEMS, SEND_HOUR, Window, summarize, window_at};
use super::digest_live::{DigestIo, own_window, send_due};
use ::hagency::issue_watch::issue_track::{Track, TrackChange, apply_change};
use chrono::{DateTime, FixedOffset, TimeZone};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

const REPO: &str = "acme/app";
const ROOM: &str = "!report:example.org";

fn at(day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(8 * 3600).unwrap().with_ymd_and_hms(2026, 11, day, hour, minute, 0).unwrap()
}
fn today() -> Window {
    window_at(&at(18, SEND_HOUR, 5)).unwrap()
}
/// One record moved through `steps` (`(at, change)`) with the ledger's rule.
fn track(number: i64, steps: &[(i64, Value)]) -> Track {
    let mut current = None;
    for (now, extra) in steps {
        let change: TrackChange = serde_json::from_value(super::tracker::change_of(REPO, number, extra.clone())).unwrap();
        current = Some(apply_change(current, &change, *now).unwrap());
    }
    current.unwrap()
}
/// A day: #33 and #34 fixed by one PR, #35 fixed by its own, #36 held, #37 given up,
/// #20 merged yesterday (before the window), and another repository's merge.
fn day_of_moves(w: &Window) -> Vec<Track> {
    let t = w.start + 3600;
    let merged = |pr: u64| vec![(t, json!({"handed": 1})), (t + 60, json!({"state": "pr_opened", "pr": pr})), (t + 120, json!({"state": "merged", "notified": true}))];
    let mut other = track(1, &merged(9));
    other.repo = "acme/other".into();
    vec![
        track(33, &merged(41)),
        track(34, &merged(41)),
        track(35, &merged(42)),
        track(36, &[(t, json!({"handed": 1})), (t + 60, json!({"state": "pr_opened", "pr": 43})), (t + 90, json!({"state": "reviewed", "notified": true}))]),
        track(37, &[(t, json!({"handed": 1})), (t + 60, json!({"state": "failed", "notified": true}))]),
        track(20, &[(w.start - 10, json!({"handed": 1})), (w.start - 5, json!({"state": "merged", "pr": 30}))]),
        other,
    ]
}
fn unanswered_request(number: i64, received: i64) -> Value {
    json!({"issue": {"repo": REPO, "number": number}, "receivedAt": received, "outcome": "answered", "unanswered": true})
}

#[test]
fn the_window_opens_at_the_send_hour_and_covers_the_day_since_yesterdays() {
    assert_eq!(window_at(&at(18, SEND_HOUR - 1, 59)), None, "nothing before the send hour");
    let w = window_at(&at(18, 23, 0)).expect("a start after the hour still sends");
    assert_eq!(w.day, "2026-11-18");
    assert_eq!(w.end, at(18, SEND_HOUR, 0).timestamp());
    assert_eq!(w.start, at(17, SEND_HOUR, 0).timestamp());
}

#[test]
fn counts_fixed_merged_and_need_you_from_the_ledger() {
    let w = today();
    let requests = json!([unanswered_request(38, w.start + 10), unanswered_request(36, w.start + 20), unanswered_request(39, w.start - 1)]);
    let unanswered = digest::unanswered_issues(&requests, REPO, &w);
    assert_eq!(unanswered, vec![36, 38], "outside the window does not count");
    let day = summarize(REPO, &day_of_moves(&w), &unanswered, &w).unwrap();
    // Fixed: three issues; merged: two pull requests (#41 closed two issues).
    assert_eq!((day.fixed, day.merged), (3, 2));
    // Need you: #36 held, #37 given up, #38's card (#36's card is the held PR, counted once).
    assert_eq!(day.need, 3);
    let text = digest::text(REPO, &day);
    assert!(text.starts_with("📋 acme/app daily digest — Today: 3 fixed, 2 merged, 3 need you"), "{text}");
    for line in ["#33 → PR #41 merged", "#36 → PR #43 needs you: https://github.com/acme/app/pull/43", "#37 gave up: https://github.com/acme/app/issues/37", "#38 approval card unanswered: https://github.com/acme/app/issues/38"] {
        assert!(text.contains(line), "{line} in {text}");
    }
    assert!(!text.contains("#20") && !text.contains("PR #9"), "yesterday's and other repositories' moves stay out: {text}");
}

#[test]
fn a_quiet_day_says_nothing_but_a_held_pr_alone_is_not_activity() {
    let w = today();
    let old_hold = track(36, &[(w.start - 99, json!({"handed": 1})), (w.start - 90, json!({"state": "reviewed", "pr": 43, "notified": true}))]);
    assert_eq!(summarize(REPO, &[old_hold], &[], &w), None);
    assert_eq!(summarize(REPO, &[], &[], &w), None);
}

#[test]
fn a_long_day_is_cut_to_the_item_limit() {
    let w = today();
    let many: Vec<Track> = (1..=(MAX_ITEMS as i64 + 3)).map(|n| track(n, &[(w.start + n, json!({"handed": 1}))])).collect();
    let text = digest::text(REPO, &summarize(REPO, &many, &[], &w).unwrap());
    assert_eq!(text.lines().count(), 1 + MAX_ITEMS + 1, "{text}");
    assert!(text.ends_with("And 3 more"), "{text}");
}

#[test]
fn every_watch_has_its_report_room_once() {
    let saved = json!({
        "issueWatch": {"mode": "all", "label": "", "repo": "Acme/App", "roomId": "!single:x", "enabledAt": 1},
        "work": [{"repo": "acme/app", "roomId": ROOM}, {"repo": "acme/docs", "roomId": "!docs:x"}],
    });
    assert_eq!(digest::report_rooms(&saved), vec![(REPO.into(), ROOM.into()), ("acme/docs".into(), "!docs:x".into())]);
    let off = json!({"issueWatch": {"mode": "off", "repo": REPO, "roomId": ROOM, "enabledAt": 0}});
    assert!(digest::report_rooms(&off).is_empty());
}

/// The digest's I/O on fakes; `sent` is the ledger's once-a-day record.
#[derive(Default)]
struct Fake {
    not_own: bool,
    requests: Value,
    posts: RefCell<Vec<(String, String, String)>>,
    sent: RefCell<BTreeMap<String, String>>,
    fail_post: Cell<bool>,
}
impl DigestIo for Fake {
    async fn own_window(&self) -> Result<bool, String> {
        Ok(!self.not_own)
    }
    async fn own_requests(&self) -> Result<Value, String> {
        Ok(self.requests.clone())
    }
    async fn post(&self, room: &str, repo: &str, day: &str, text: String) -> Result<(), String> {
        if self.fail_post.get() {
            return Err("homeserver unreachable".into());
        }
        // A fixed transaction id: the homeserver drops a repeat of the same day.
        if !self.posts.borrow().iter().any(|(r, _, d)| r == repo && d == day) {
            self.posts.borrow_mut().push((repo.into(), text, day.into()));
        }
        assert_eq!(room, ROOM);
        Ok(())
    }
    async fn claim(&self, repo: &str, day: &str) -> Result<(), String> {
        self.sent.borrow_mut().insert(repo.to_ascii_lowercase(), day.into());
        Ok(())
    }
    fn paused(&self) -> bool {
        false
    }
}
/// What one check reads from the owner API: the watch, the records, the sent days.
fn saved(fake: &Fake, tracks: &[Track]) -> Value {
    json!({"work": [{"repo": REPO, "roomId": ROOM}], "track": tracks, "digestSent": fake.sent.borrow().clone()})
}
fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future)
}

#[test]
fn the_digest_is_sent_once_a_day_even_across_a_restart() {
    let w = today();
    let tracks = day_of_moves(&w);
    let fake = Fake { requests: json!([]), ..Fake::default() };
    assert!(run(send_due(&fake, &saved(&fake, &tracks), window_at(&at(18, 17, 59)))).is_empty());
    assert!(fake.posts.borrow().is_empty(), "nothing before the send hour");
    // A failed post is not recorded, so the next check tries again.
    fake.fail_post.set(true);
    assert_eq!(run(send_due(&fake, &saved(&fake, &tracks), Some(w.clone()))).len(), 1);
    assert!(fake.sent.borrow().is_empty());
    fake.fail_post.set(false);
    for _ in 0..3 {
        assert!(run(send_due(&fake, &saved(&fake, &tracks), Some(w.clone()))).is_empty());
    }
    assert_eq!(fake.posts.borrow().len(), 1);
    // A restart keeps only the ledger's record: still nothing more today.
    let restarted = Fake { requests: json!([]), sent: RefCell::new(fake.sent.borrow().clone()), ..Fake::default() };
    run(send_due(&restarted, &saved(&restarted, &tracks), window_at(&at(18, 22, 0))));
    assert!(restarted.posts.borrow().is_empty());
    // The next day is a new digest.
    let tomorrow = window_at(&at(19, SEND_HOUR, 1)).unwrap();
    let next = track(50, &[(tomorrow.start + 1, json!({"handed": 1})), (tomorrow.start + 2, json!({"state": "pr_opened", "pr": 51})), (tomorrow.start + 3, json!({"state": "merged"}))]);
    run(send_due(&restarted, &saved(&restarted, &[next]), Some(tomorrow)));
    assert_eq!(restarted.posts.borrow().len(), 1);
    assert!(restarted.posts.borrow()[0].1.contains("Today: 1 fixed, 1 merged, 0 need you"));
}

#[test]
fn a_quiet_day_posts_nothing_and_is_not_asked_again() {
    let fake = Fake { requests: json!([]), ..Fake::default() };
    assert!(run(send_due(&fake, &saved(&fake, &[]), Some(today()))).is_empty());
    assert!(fake.posts.borrow().is_empty());
    assert_eq!(fake.sent.borrow().get(REPO).map(String::as_str), Some("2026-11-18"));
}

#[test]
fn only_the_owners_own_window_sends() {
    let w = today();
    let fake = Fake { not_own: true, requests: json!([]), ..Fake::default() };
    run(send_due(&fake, &saved(&fake, &day_of_moves(&w)), Some(w)));
    assert!(fake.posts.borrow().is_empty() && fake.sent.borrow().is_empty());
    let devices = json!({"currentDeviceId": "d1", "devices": [{"id": "d1", "revoked": false}, {"id": "d2", "revoked": false}]});
    let mine = json!({"id": "a1", "state": "active", "executionDeviceId": "d1"});
    assert!(own_window(&mine, Some("@chris:x"), &devices));
    assert!(!own_window(&mine, None, &devices), "signed out");
    let shared = json!({"id": "a1", "state": "active", "executionDeviceId": "d1", "ownerMxid": "@anna:x"});
    assert!(!own_window(&shared, Some("@chris:x"), &devices), "another owner's Agent");
    let elsewhere = json!({"id": "a1", "state": "active", "executionDeviceId": "d2"});
    assert!(!own_window(&elsewhere, Some("@chris:x"), &devices), "the owner's other device sends it");
}
