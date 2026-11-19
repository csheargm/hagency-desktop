//! User flow (Chris, Nov 18): people agree on a change in the project room,
//! Anna (a maintainer) says "@bird go build this", and bird files the issues on
//! the room's linked repository with no card, while the room runs
//! (Assist/Autonomous). The host records the linked set only for the room's
//! owner or a maintainer; for anyone else nothing is recorded, so it asks.
use super::*;
use crate::room_autonomy::{AutonomySnapshot, Level};
use crate::watch_trust::read_only_flow_tests::{ask_owner_ledger, proposal, OWNER, REPO, ROOM};
use crate::watch_trust::{room_scope, TrustedStep};

const FILE: &str = "gh issue create --repo csheargm/hagency-demo-sandbox --title 'Add a dark mode' --body 'As agreed in the room: a toggle in Settings.'";

fn room(ledger: &mut crate::Ledger, level: Level, paused: bool) {
    let snapshot = AutonomySnapshot { room_id: ROOM.into(), level: Some(level), paused, unread: false, owner_maintainer: false, as_of: crate::room_autonomy::as_of::next_for_test(), sticky: false };
    ledger.set_room_autonomy(OWNER, &snapshot, crate::merge_policy::room_rules::unix_now()).unwrap();
}

#[test]
fn only_the_three_options_each_once_and_bounded() {
    let words = |line: &str| crate::outside::shell::parse(line).unwrap().segments.remove(0);
    let ok = words(FILE);
    let ok: Vec<&str> = ok.iter().map(String::as_str).collect();
    assert_eq!(IssueCreate::parse(&ok).map(|i| i.title), Some("Add a dark mode"));
    for bad in [
        "gh issue create --repo a/b --title t",
        "gh issue create --repo a/b --title t --body b --label x",
        "gh issue create --repo a/b --title t --title u --body b",
        "gh issue create --repo a/b --title=t --body b",
        "gh issue create --repo https://evil/a/b --title t --body b",
        "gh issue create --repo a/b --title '' --body b",
        "gh issue create --repo a/b --web --title t --body b",
    ] {
        let w = words(bad);
        let w: Vec<&str> = w.iter().map(String::as_str).collect();
        assert_eq!(IssueCreate::parse(&w), None, "{bad}");
    }
    let long = "x".repeat(MAX_TITLE_CHARS + 1);
    assert_eq!(IssueCreate::parse(&["gh", "issue", "create", "--repo", "a/b", "--title", &long, "--body", "b"]), None);
    let pinned = IssueCreate { repo: "a/b", title: "it's", body: "$(rm -rf ~)" }.command();
    assert!(pinned.contains("GH_HOST='github.com' gh 'issue' 'create' '--repo' 'a/b' '--title' 'it'\\''s' '--body' '$(rm -rf ~)'"), "{pinned}");
}

#[test]
fn a_member_files_on_a_linked_repo_while_the_room_runs() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ask_owner_ledger(temp.path());
    let ask = proposal(ROOM, FILE);
    room_scope::record_linked(ROOM, &ask.scope.requester, vec![REPO.into()]);
    ledger.set_may_work(OWNER, "bird", ROOM, true).unwrap();
    room(&mut ledger, Level::Off, false);
    assert_eq!(ledger.trusted_publish(OWNER, &ask, 11).unwrap(), None, "Off: asks");
    room(&mut ledger, Level::Assist, true);
    assert_eq!(ledger.trusted_publish(OWNER, &ask, 11).unwrap(), None, "paused: asks");
    room(&mut ledger, Level::Assist, false);
    assert!(matches!(ledger.trusted_publish(OWNER, &ask, 11).unwrap(), Some(TrustedStep::Command(c)) if c.contains("'issue' 'create'")), "Assist: no card");
    let other = proposal(ROOM, &FILE.replace("csheargm/hagency-demo-sandbox", "csheargm/other"));
    assert_eq!(ledger.trusted_publish(OWNER, &other, 11).unwrap(), None, "a repository the room does not link");
    let mut stranger = proposal(ROOM, FILE);
    stranger.scope.requester = "@stranger:example.org".into();
    assert_eq!(ledger.trusted_publish(OWNER, &stranger, 11).unwrap(), None, "nothing recorded for a plain member or a non-member: the card asks");
    ledger.set_may_work(OWNER, "bird", ROOM, false).unwrap();
    assert_eq!(ledger.trusted_publish(OWNER, &ask, 11).unwrap(), None, "may not work here: asks");
}
