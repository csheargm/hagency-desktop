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

/// Live dev room, Nov 19 00:15: "@anna-claude go build this" made the Agent
/// build it itself. With "may work in this room" and a linked repository the
/// context makes it a planner, and the command it is taught files card-free.
#[test]
fn go_build_this_planner_rule_only_where_it_may_work_and_the_room_links_repos() {
    let (_t, mut ledger) = ledger();
    let linked = vec!["csheargm/hagency-demo-sandbox".to_owned()];
    let context = |ledger: &Ledger, linked: &[String]| {
        render_context(&[], DEV, OWNER, ledger.may_work("agt_yue", DEV).unwrap(), linked)
    };
    assert!(!context(&ledger, &linked).contains(hagency_runtime::PLAN_ON_GO_BUILD), "not let work here");
    ledger.set_may_work(OWNER, "agt_yue", DEV, true).unwrap();
    assert!(!context(&ledger, &[]).contains(hagency_runtime::PLAN_ON_GO_BUILD), "no linked repository");
    let shown = context(&ledger, &linked);
    assert!(shown.starts_with(OWN_WATCH_OPEN) && shown.ends_with(hagency_runtime::PLAN_ON_GO_BUILD));
}

/// The words a command line reads as, or `None` when it is not one plain command.
fn one_command(line: &str) -> Option<Vec<String>> {
    let mut parsed = crate::outside::shell::parse(line)?;
    (parsed.segments.len() == 1 && parsed.heredoc_at.is_none()).then(|| parsed.segments.remove(0))
}

/// The taught shape, filled in, passes issue_create's checker; the usual
/// `'\''` escape does not (the shell reader refuses any backslash), so the
/// rule teaches double quotes for an apostrophe instead.
#[test]
fn the_taught_issue_command_passes_the_card_free_checker() {
    let note = hagency_runtime::PLAN_ON_GO_BUILD;
    let start = note.find("gh issue create").unwrap();
    let end = note.find("criteria>'").unwrap() + "criteria>'".len();
    let fill = |title: &str, body: &str| {
        note[start..end].replace("<owner/name>", "csheargm/hagency-demo-sandbox")
            .replace("'<title>'", title).replace("'<body with clear acceptance criteria>'", body)
    };
    let escaped = fill("'Add each doc'\\''s purpose'", "'Acceptance: it'\\''s linked from README.'");
    assert!(one_command(&escaped).is_none(), "'\\'' must not be taught: {escaped}");
    assert!(note.contains("apostrophe goes in double quotes"));
    let line = fill("\"Add each doc's purpose to docs/INDEX.md\"", "\"Acceptance: INDEX.md names every file in docs/; it's linked from README.\"");
    let words = one_command(&line).expect(&line);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let issue = crate::watch_trust::issue_create::IssueCreate::parse(&words).expect(&line);
    assert_eq!((issue.repo, issue.title), ("csheargm/hagency-demo-sandbox", "Add each doc's purpose to docs/INDEX.md"));
    assert!(issue.body.contains("it's linked from README."));
    let plain = fill("'Add docs/INDEX.md'", "'Acceptance: one line per doc.'");
    let plain = one_command(&plain).unwrap();
    assert!(crate::watch_trust::issue_create::IssueCreate::parse(&plain.iter().map(String::as_str).collect::<Vec<_>>()).is_some());
}

#[test]
fn the_taught_apostrophe_form_passes_the_shell_checker_and_the_escape_does_not() {
    // Live Nov 19: the steps told agents to write an apostrophe as '\'', which the
    // shell checker refuses (no unquoted backslash), so such a commit asked the owner.
    assert!(super::PUSH_FREELY_STEPS.contains("goes in double quotes instead"));
    assert!(!super::PUSH_FREELY_STEPS.contains("(an apostrophe as '\\''"));
    assert!(crate::outside::shell::parse("git -C /w/repo commit -m \"docs: add the team's index\"").is_some());
    assert!(crate::outside::shell::parse("git -C /w/repo commit -m 'docs: add the team'\\''s index'").is_none());
}
