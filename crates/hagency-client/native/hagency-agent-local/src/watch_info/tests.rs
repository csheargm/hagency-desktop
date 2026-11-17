use super::*;
use crate::issue_watch::WatchSetting;

const OWNER: &str = "@alice:example.org";
const DEV: &str = "!dev:example.org";
const OTHER: &str = "!other:example.org";

fn ledger() -> (tempfile::TempDir, Ledger) {
    let temp = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    (temp, ledger)
}
fn rules(repo: &str, room: &str) -> OwnerRules {
    OwnerRules { repo: repo.into(), room_id: room.into(), issues_per_day: Some(7), open_prs: Some(2), trusted_authors: vec!["secret-login".into()], ..Default::default() }
}
fn legacy(mode: WatchMode, repo: &str, room: &str) -> WatchSetting {
    WatchSetting { mode, label: String::new(), repo: repo.into(), room_id: room.into(), push_trusted: true }
}

#[test]
fn own_block_lists_repos_mode_room_status_and_merge_rule() {
    let (_t, mut ledger) = ledger();
    ledger.set_work(OWNER, "agt_yue", vec![rules("acme/app", DEV), OwnerRules { any_path: true, max_lines: Some(200), ..rules("acme/docs", OTHER) }]).unwrap();
    let block = render_own(&ledger.watch_entries("agt_yue").unwrap(), DEV, OWNER);
    assert!(block.starts_with(OWN_WATCH_OPEN) && block.ends_with(OWN_WATCH_CLOSE));
    assert!(block.contains("- acme/app: active; issues taken: trusted; reports to room !dev:example.org (this room)"));
    assert!(block.contains("docs-only, up to 50 changed lines"));
    assert!(block.contains("- acme/docs: active;") && block.contains("any path, up to 200 changed lines"));
    assert!(!block.contains("!other:example.org (this room)"));
}

#[test]
fn own_block_says_off_when_nothing_is_watched() {
    let (_t, ledger) = ledger();
    let block = render_own(&ledger.watch_entries("agt_none").unwrap(), DEV, OWNER);
    assert!(block.contains("You watch no GitHub repositories that report to this room"));
    assert!(block.contains("Your owner (who approves your cards, in their own Hagency window): @alice:example.org"));
}

#[test]
fn legacy_watch_set_off_is_paused_and_not_duplicated_by_work_list() {
    let (_t, mut ledger) = ledger();
    ledger.set_issue_watch(OWNER, "agt_a", legacy(WatchMode::Off, "acme/old", DEV), 1).unwrap();
    let entries = ledger.watch_entries("agt_a").unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].active && !entries[0].push_freely, "switching off drops push trust");
    ledger.set_work(OWNER, "agt_a", vec![rules("ACME/old", DEV)]).unwrap();
    assert_eq!(ledger.watch_entries("agt_a").unwrap().len(), 1, "work list entry replaces the legacy one");
}

#[test]
fn room_view_lists_only_this_room_and_never_limits_or_trust_lists() {
    let (_t, mut ledger) = ledger();
    ledger.set_work(OWNER, "agt_yue", vec![rules("acme/app", DEV), rules("acme/private", OTHER)]).unwrap();
    ledger.set_issue_watch(OWNER, "agt_bob", legacy(WatchMode::Off, "acme/web", DEV), 1).unwrap();
    let watches = ledger.room_watches(DEV, |id| if id == "agt_yue" { "yue-claude".into() } else { id.into() }).unwrap();
    let text = render_room(&watches);
    assert!(text.contains("\"agent\":\"yue-claude\"") && text.contains("\"repo\":\"acme/app\"") && text.contains("\"status\":\"active\""));
    assert!(text.contains("\"repo\":\"acme/web\"") && text.contains("\"status\":\"paused\""));
    assert!(text.contains(OWNER));
    for leak in ["acme/private", "secret-login", "7", "issues_per_day", "token", OTHER] {
        assert!(!text.contains(leak), "room view leaked {leak}: {text}");
    }
}

#[test]
fn one_line_strips_controls_and_bounds_length() {
    assert_eq!(one_line(" a\nb\u{7}c "), "a b c");
    assert_eq!(one_line(&"é".repeat(300)).len(), 254);
}

#[test]
fn the_own_block_shows_only_this_rooms_watches_even_to_the_owner() {
    let (_t, mut ledger) = ledger();
    ledger.set_work(OWNER, "agt_yue", vec![rules("acme/app", DEV), rules("acme/private", OTHER)]).unwrap();
    let shown = render_own(&visible_to(ledger.watch_entries("agt_yue").unwrap(), DEV), DEV, OWNER);
    assert!(shown.contains("acme/app") && !shown.contains("acme/private") && !shown.contains(OTHER));
}

#[test]
fn a_long_room_list_stays_under_the_result_cap() {
    let many: Vec<RoomWatch> = (0..MAX_LISTED).map(|i| RoomWatch { agent: "a".repeat(200), owner: "@o:example.org".into(), repo: format!("owner/repo-{i:0>90}"), active: true }).collect();
    let out = render_room(&many);
    assert!(out.len() <= ROOM_RESULT_BYTES && out.contains("\"more\":true"));
}
