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
    WatchSetting { mode, label: label.into(), repo: "acme/app".into(), room_id: ROOM.into(), push_trusted: false }
}
fn issue(number: i64, labels: &[&str], is_pr: bool, created_at: i64) -> Issue {
    by("NONE", number, labels, is_pr, created_at)
}
fn by(author: &str, number: i64, labels: &[&str], is_pr: bool, created_at: i64) -> Issue {
    Issue { author_association: author.into(), number, url: format!("https://github.com/acme/app/issues/{number}"), title: String::new(), labels: labels.iter().map(|s| s.to_string()).collect(), is_pr, created_at }
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
        {"number": 12, "html_url": "https://github.com/acme/app/issues/12", "title": "Fix the README", "labels": [{"name": "agent-fix"}], "author_association": "NONE", "created_at": "2026-11-13T10:00:00Z"},
        {"number": 13, "html_url": "https://github.com/acme/app/pull/13", "labels": [], "pull_request": {"url": "x"}, "created_at": "2026-11-13T10:00:01Z"}
    ]);
    let issues = parse_issues(&body.to_string()).unwrap();
    assert_eq!(issues[0], Issue { title: "Fix the README".into(), ..issue(12, &["agent-fix"], false, 1_794_564_000) });
    assert!(issues[1].is_pr);
    assert_eq!(issues[1].author_association, "", "missing association is untrusted");
    assert_eq!((issues[0].title.as_str(), issues[1].title.as_str()), ("Fix the README", ""), "title read; missing title is empty");
    assert!(matches!(parse_issues("{\"message\":\"Bad credentials\"}"), Err(FetchError::Parse(_))));
    let bad_time = json!([{"number": 1, "html_url": "u", "created_at": "yesterday"}]);
    assert!(matches!(parse_issues(&bad_time.to_string()), Err(FetchError::Parse(_))));
}

#[test]
fn watch_settings_use_camel_case_on_the_wire() {
    let parsed: WatchSetting = serde_json::from_value(json!({"mode": "labelled", "label": "x", "repo": "acme/app", "roomId": ROOM})).unwrap();
    assert_eq!(parsed, setting(WatchMode::Labelled, "x"));
    let wire = serde_json::to_value(IssueWatch { setting: parsed, enabled_at: 3 }).unwrap();
    assert_eq!(wire, json!({"mode": "labelled", "label": "x", "repo": "acme/app", "roomId": ROOM, "pushTrusted": false, "enabledAt": 3}));
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

/// A ledger from before push trust gains the column, off for its saved watch.
#[test]
fn an_older_watch_table_gains_push_trust_switched_off() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA).unwrap();
    db.execute("INSERT INTO issue_watch VALUES ('a','all','','acme/app','!r:x',1)", []).unwrap();
    ensure_schema(&db).unwrap();
    ensure_schema(&db).unwrap();
    let trusted: i64 = db.query_row("SELECT push_trusted FROM issue_watch WHERE agent='a'", [], |r| r.get(0)).unwrap();
    assert_eq!(trusted, 0);
}

#[test]
fn the_review_loop_reads_only_other_peoples_ready_pull_requests() {
    use super::wire::{others_ready_pulls, parse_login, parse_permission, parse_pulls, pulls_url};
    let sha = "1".repeat(40);
    let body = json!([
        {"number": 30, "html_url": "https://github.com/acme/app/pull/30", "draft": false, "head": {"sha": sha}, "user": {"login": "Anna"}, "created_at": "2026-11-16T10:00:00Z"},
        {"number": 31, "html_url": "https://github.com/acme/app/pull/31", "draft": true, "head": {"sha": sha}, "user": {"login": "anna"}, "created_at": "2026-11-16T10:00:00Z"},
        {"number": 32, "html_url": "https://github.com/acme/app/pull/32", "head": {"sha": sha}, "user": {"login": "mallory"}, "created_at": "2026-11-16T10:00:00Z"},
        {"number": 33, "html_url": "https://github.com/acme/app/pull/33", "head": {"sha": sha}, "user": {"login": "anna"}, "created_at": "2020-01-01T00:00:00Z"}
    ]).to_string();
    let pulls = parse_pulls(&body).unwrap();
    let theirs: Vec<u64> = others_ready_pulls(&pulls, "mallory", 1_700_000_000).iter().map(|p| p.number).collect();
    assert_eq!(theirs, [30], "not drafts, not our own, not from before the watch");
    assert!(parse_pulls("{}").is_err());
    assert!(pulls_url("https://api.github.com", "acme/app/../x").is_err());
    assert_eq!(parse_permission(&json!({"permission": "write", "role_name": "maintain"}).to_string()).unwrap(), "maintain");
    assert_eq!(parse_permission(&json!({"permission": "read"}).to_string()).unwrap(), "read");
    assert!(parse_permission("{}").is_err());
    assert_eq!(parse_login(&json!({"login": "anna"}).to_string()).unwrap(), "anna");
}

#[test]
fn the_earliest_unexpired_eligible_claim_holds_an_issue() {
    use super::wire::{claim_authors, claim_body, claim_holder, comments_url, parse_comments, Claim, CLAIM_MARKER, CLAIM_EXPIRY_SECS};
    let body = json!([
        {"user": {"login": "carl"}, "body": "me too", "created_at": "2026-11-16T09:00:00Z"},
        {"user": {"login": "mallory"}, "body": claim_body("agt_x", "fake"), "created_at": "2026-11-16T09:30:00Z"},
        {"user": {"login": "betty"}, "body": claim_body("agt_b1", "\"betty-claude\""), "created_at": "2026-11-16T10:00:00Z"},
        {"user": {"login": "anna"}, "body": format!("{CLAIM_MARKER} — anna-claude"), "created_at": "2026-11-16T10:00:05Z"}
    ]).to_string();
    let comments = parse_comments(&body).unwrap();
    let now = comments[3].created_at + 10;
    let team = |login: &str| login != "mallory";
    assert_eq!(claim_holder(&comments, now, team), Some(Claim { login: "betty", agent: Some("agt_b1") }), "mallory's forged claim is ignored");
    assert!(claim_holder(&comments, now, team).unwrap().is("BETTY", "agt_b1"));
    assert!(!claim_holder(&comments, now, team).unwrap().is("betty", "agt_b2"), "another Agent on the same login");
    assert_eq!(claim_holder(&comments[..1], now, team), None, "an ordinary comment is no claim");
    let later = comments[2].created_at + CLAIM_EXPIRY_SECS + 1;
    assert_eq!(claim_holder(&comments, later, team), Some(Claim { login: "anna", agent: None }), "betty's claim expired");
    assert_eq!(claim_authors(&comments, "anna", now), vec!["betty", "mallory"]);
    assert!(comments_url("https://api.github.com", "acme/app", 0, 1).is_err());
}

#[test]
fn the_owners_own_pull_request_merges_only_on_another_accounts_approval_of_the_head() {
    use super::wire::{approved_by_other, own_ready_pulls, parse_pulls, parse_reviews, reviews_url};
    let (head, old) = ("a".repeat(40), "b".repeat(40));
    let pulls = parse_pulls(&json!([
        {"number": 40, "html_url": "https://github.com/acme/app/pull/40", "head": {"sha": head}, "user": {"login": "Anna"}, "created_at": "2026-11-16T10:00:00Z"},
        {"number": 41, "html_url": "https://github.com/acme/app/pull/41", "head": {"sha": head}, "user": {"login": "bob"}, "created_at": "2026-11-16T10:00:00Z"},
        {"number": 42, "html_url": "https://github.com/acme/app/pull/42", "draft": true, "head": {"sha": head}, "user": {"login": "anna"}, "created_at": "2026-11-16T10:00:00Z"}
    ]).to_string()).unwrap();
    let own: Vec<u64> = own_ready_pulls(&pulls, "anna", 1_700_000_000).iter().map(|p| p.number).collect();
    assert_eq!(own, [40], "the owner's own ready pull requests only");
    let review = |login: &str, state: &str, commit: &str, body: &str| json!({"user": {"login": login}, "state": state, "commit_id": commit, "body": body});
    let reviews = |list: Vec<serde_json::Value>| parse_reviews(&serde_json::Value::Array(list).to_string()).unwrap();
    let bound = format!("LGTM at {head}");
    assert!(approved_by_other(&reviews(vec![review("bob", "APPROVED", &head, &bound)]), "anna", &head));
    assert!(!approved_by_other(&reviews(vec![review("ANNA", "APPROVED", &head, &bound)]), "anna", &head), "the author's own approval never counts");
    assert!(!approved_by_other(&reviews(vec![review("bob", "APPROVED", &old, &format!("ok {old}"))]), "anna", &head), "an older head");
    assert!(!approved_by_other(&reviews(vec![review("bob", "APPROVED", &head, "LGTM")]), "anna", &head), "an approval not naming its commit is unbound");
    assert!(!approved_by_other(&reviews(vec![review("bob", "APPROVED", &head, &bound), review("bob", "CHANGES_REQUESTED", &head, "")]), "anna", &head), "the latest decisive review wins");
    assert!(parse_reviews("{}").is_err());
    let ghost = reviews(vec![json!({"user": null, "state": "APPROVED", "commit_id": head, "body": bound})]);
    assert_eq!(ghost.len(), 1, "a deleted account's review keeps the page full for pagination");
    assert!(!approved_by_other(&ghost, "anna", &head), "and never counts as an approval");
    assert!(reviews_url("https://api.github.com", "acme/../x", 1, 1).is_err());
}
