use super::wire::{FetchError, parse_issues};
use super::*;
use serde_json::json;

const OWNER: &str = "@alice:example.org";
const ROOM: &str = "!project:example.org";

fn ledger() -> (tempfile::TempDir, Ledger) {
    let temp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    (temp, ledger)
}
fn setting(mode: WatchMode, label: &str) -> WatchSetting {
    WatchSetting { mode, label: label.into(), repo: "acme/app".into(), room_id: ROOM.into() }
}
fn issue(number: i64, labels: &[&str], is_pr: bool, created_at: i64) -> Issue {
    by("NONE", number, labels, is_pr, created_at)
}
fn by(author: &str, number: i64, labels: &[&str], is_pr: bool, created_at: i64) -> Issue {
    Issue { author_association: author.into(), number, url: format!("https://github.com/acme/app/issues/{number}"), labels: labels.iter().map(|s| s.to_string()).collect(), is_pr, created_at }
}
fn numbers(picked: Vec<&Issue>) -> Vec<i64> {
    picked.into_iter().map(|i| i.number).collect()
}

#[test]
fn watch_is_off_until_set_and_survives_reopen() {
    let (temp, mut ledger) = ledger();
    assert_eq!(ledger.issue_watch("a").unwrap().setting.mode, WatchMode::Off);
    let stored = ledger.set_issue_watch(OWNER, "a", setting(WatchMode::Labelled, ""), 100).unwrap();
    assert_eq!(stored.setting.label, DEFAULT_LABEL);
    assert_eq!(stored.enabled_at, 100);
    // Changing only the mode keeps enabled_at; switching repo restarts it.
    assert_eq!(ledger.set_issue_watch(OWNER, "a", setting(WatchMode::All, ""), 200).unwrap().enabled_at, 100);
    let mut other = setting(WatchMode::All, "");
    other.repo = "acme/other".into();
    assert_eq!(ledger.set_issue_watch(OWNER, "a", other, 300).unwrap().enabled_at, 300);
    ledger.mark_issue_seen("a", "acme/other", 7).unwrap();
    ledger.mark_issue_seen("a", "acme/other", 7).unwrap();
    drop(ledger);
    let ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    assert_eq!(ledger.issue_watch("a").unwrap().setting.repo, "acme/other");
    assert!(ledger.is_issue_seen("a", "acme/other", 7).unwrap());
    assert!(!ledger.is_issue_seen("a", "acme/app", 7).unwrap(), "seen is per repository");
    assert_eq!(ledger.seen_issues("a", "acme/other").unwrap(), HashSet::from([7]));
}

#[test]
fn a_database_from_before_issue_watch_gains_the_tables() {
    let (temp, ledger) = ledger();
    ledger.db.execute_batch("DROP TABLE issue_watch; DROP TABLE issue_watch_seen;").unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    ledger.set_issue_watch(OWNER, "a", setting(WatchMode::All, ""), 5).unwrap();
    assert_eq!(ledger.issue_watch("a").unwrap().setting.mode, WatchMode::All);
    // The CHECK refuses a mode the build does not know.
    assert!(ledger.db.execute("INSERT INTO issue_watch VALUES ('b','sometimes','','acme/app',?,0)", [ROOM]).is_err());
}

#[test]
fn invalid_settings_are_refused_and_other_owners_cannot_set() {
    let (_temp, mut ledger) = ledger();
    for repo in ["acme", "acme/", "/app", "acme/app/x", "ac me/app", ""] {
        let mut bad = setting(WatchMode::All, "");
        bad.repo = repo.into();
        assert!(ledger.set_issue_watch(OWNER, "a", bad, 1).is_err(), "{repo}");
    }
    let mut bad_room = setting(WatchMode::Labelled, "bug");
    bad_room.room_id = "#project:example.org".into();
    assert!(ledger.set_issue_watch(OWNER, "a", bad_room, 1).is_err());
    for room in ["!", "!:x", "!a:", "!a b:x", "!a:x\n", &format!("!{}:x", "a".repeat(260))] {
        let mut bad = setting(WatchMode::All, "");
        bad.room_id = room.to_owned();
        assert!(ledger.set_issue_watch(OWNER, "a", bad, 1).is_err(), "{room:?}");
    }
    for room in ["!abc:example.org", "!v12OpaqueHash"] {
        let mut good = setting(WatchMode::All, "");
        good.room_id = room.into();
        assert!(ledger.set_issue_watch(OWNER, "a", good, 1).is_ok(), "{room}");
    }
    assert!(ledger.set_issue_watch("@mallory:example.org", "a", setting(WatchMode::All, ""), 1).is_err());
    // Off needs no repository or room.
    assert!(ledger.set_issue_watch(OWNER, "a", WatchSetting::default(), 1).is_ok());
}

#[test]
fn labelled_picks_labelled_issues_and_skips_prs_and_seen() {
    let watch = IssueWatch { setting: setting(WatchMode::Labelled, "agent-fix"), enabled_at: 1_000 };
    let issues = [issue(1, &["Agent-Fix"], false, 10), issue(2, &["bug"], false, 2_000), issue(3, &["agent-fix"], true, 2_000), issue(4, &["agent-fix"], false, 2_000)];
    assert_eq!(numbers(select(&watch, &issues, &HashSet::from([4]))), [1], "old labelled issues count: the label is deliberate");
}

#[test]
fn all_picks_only_issues_created_after_the_watch_was_switched_on() {
    let watch = IssueWatch { setting: setting(WatchMode::All, ""), enabled_at: 1_000 };
    let issues = [issue(1, &[], false, 999), issue(2, &[], false, 1_000), issue(3, &[], false, 1_001), issue(4, &[], true, 5_000), issue(5, &["x"], false, 5_000)];
    // Inclusive cutoff: #2 was opened in the switch-on second.
    assert_eq!(numbers(select(&watch, &issues, &HashSet::new())), [2, 3, 5]);
    assert_eq!(numbers(select(&watch, &issues, &HashSet::from([2, 3]))), [5]);
    let off = IssueWatch { setting: setting(WatchMode::Off, ""), enabled_at: 0 };
    assert!(select(&off, &issues, &HashSet::new()).is_empty());
}

#[test]
fn trusted_picks_new_issues_by_the_team_only() {
    let watch = IssueWatch { setting: setting(WatchMode::Trusted, ""), enabled_at: 1_000 };
    let issues = [
        by("MEMBER", 1, &[], false, 2_000),
        by("OWNER", 2, &[], false, 2_000),
        by("COLLABORATOR", 3, &[], false, 2_000),
        by("NONE", 4, &[], false, 2_000),
        by("CONTRIBUTOR", 5, &["agent-fix"], false, 2_000),
        by("MEMBER", 6, &[], false, 999),
        by("MEMBER", 7, &[], true, 2_000),
    ];
    // Outsiders are skipped even with the label: labels count only in Labelled mode.
    assert_eq!(numbers(select(&watch, &issues, &HashSet::from([3]))), [1, 2]);
    let (_temp, mut ledger) = ledger();
    assert_eq!(ledger.set_issue_watch(OWNER, "a", setting(WatchMode::Trusted, ""), 9).unwrap().setting.mode, WatchMode::Trusted);
    assert_eq!(ledger.issue_watch("a").unwrap().setting.mode, WatchMode::Trusted);
}

#[test]
fn github_issue_lists_parse_and_bad_ones_are_errors() {
    let body = json!([
        {"number": 12, "html_url": "https://github.com/acme/app/issues/12", "labels": [{"name": "agent-fix"}], "author_association": "NONE", "created_at": "2026-11-13T10:00:00Z"},
        {"number": 13, "html_url": "https://github.com/acme/app/pull/13", "labels": [], "pull_request": {"url": "x"}, "created_at": "2026-11-13T10:00:01Z"}
    ]);
    let issues = parse_issues(&body.to_string()).unwrap();
    assert_eq!(issues[0], issue(12, &["agent-fix"], false, 1_794_564_000));
    assert!(issues[1].is_pr);
    assert_eq!(issues[1].author_association, "", "missing association is untrusted");
    assert!(matches!(parse_issues("{\"message\":\"Bad credentials\"}"), Err(FetchError::Parse(_))));
    let bad_time = json!([{"number": 1, "html_url": "u", "created_at": "yesterday"}]);
    assert!(matches!(parse_issues(&bad_time.to_string()), Err(FetchError::Parse(_))));
}

#[test]
fn watch_settings_use_camel_case_on_the_wire() {
    let parsed: WatchSetting = serde_json::from_value(json!({"mode": "labelled", "label": "x", "repo": "acme/app", "roomId": ROOM})).unwrap();
    assert_eq!(parsed, setting(WatchMode::Labelled, "x"));
    let wire = serde_json::to_value(IssueWatch { setting: parsed, enabled_at: 3 }).unwrap();
    assert_eq!(wire, json!({"mode": "labelled", "label": "x", "repo": "acme/app", "roomId": ROOM, "enabledAt": 3}));
}

#[test]
fn every_full_page_is_followed_up_to_the_cap() {
    use super::wire::next_page;
    assert_eq!(next_page(1, 100), Some(2), "a full page means more may follow");
    assert_eq!(next_page(1, 99), None);
    assert_eq!(next_page(10, 100), None, "rate-limit cap");
}

#[test]
fn a_failed_atomic_change_rolls_back_earlier_writes() {
    let (_temp, mut ledger) = ledger();
    let result = ledger.atomically(|l| {
        l.mark_issue_seen("a", "acme/app", 7)?;
        let mut bad = setting(WatchMode::All, "");
        bad.repo = "nope".into();
        l.set_issue_watch(OWNER, "a", bad, 1)
    });
    assert!(result.is_err());
    assert!(!ledger.is_issue_seen("a", "acme/app", 7).unwrap(), "the earlier write was rolled back");
    ledger.atomically(|l| l.mark_issue_seen("a", "acme/app", 8)).unwrap();
    assert!(ledger.is_issue_seen("a", "acme/app", 8).unwrap());
}

#[test]
fn page_urls_are_built_only_for_owner_name_repositories() {
    use super::wire::page_url;
    assert_eq!(
        page_url("https://api.github.com", "acme/app", 2).unwrap(),
        "https://api.github.com/repos/acme/app/issues?state=open&sort=created&direction=desc&per_page=100&page=2"
    );
    assert!(matches!(page_url("https://api.github.com", "acme/app/../x", 1), Err(FetchError::Repo(_))));
}
