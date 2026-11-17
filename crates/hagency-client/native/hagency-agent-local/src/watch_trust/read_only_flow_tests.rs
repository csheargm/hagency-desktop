//! User flow (live 2026-11-17): Anna asks Betty's watching Agent "any issues
//! pending?" in the watch's Room. `gh issue list` on the watched repository
//! runs without a card, for a non-owner requester and with push trust off;
//! another repository, another Room, a chained or writing command still ask.
use super::TrustedStep;
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::merge_policy::work::OwnerRules;
use crate::{Layer, Ledger, Scope, ToolPolicy, ToolProposal};
use serde_json::json;

const OWNER: &str = "@betty:example.org";
const ROOM: &str = "!dev:example.org";
const REPO: &str = "csheargm/hagency-demo-sandbox";

fn proposal(room: &str, line: &str) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: "bird".into(), binding: "b".into(), room: room.into(), requester: "@anna:example.org".into(), thread: "t".into() },
        dispatch: "d".into(), tool: "claude.Bash".into(), arguments: json!({"command": line}),
        canonical_directory: "/tmp".into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
    }
}

fn ask_owner_ledger(dir: &std::path::Path) -> Ledger {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    let scope = proposal(ROOM, "x").scope;
    ledger.register_binding(OWNER, &scope).unwrap();
    let ask = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk: ToolPolicy::AskOwner,
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope, layer, 0, &ask).unwrap();
    }
    ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
    ledger
}

#[test]
fn read_only_queries_on_a_watched_repo_skip_the_card() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = ask_owner_ledger(db.path());
    let list = proposal(ROOM, "gh issue list -R csheargm/hagency-demo-sandbox --state open");
    assert_eq!(ledger.trusted_publish(OWNER, &list, 11).unwrap(), None, "not watched yet: asks");
    let rules = OwnerRules { repo: REPO.into(), room_id: ROOM.into(), ..Default::default() };
    ledger.set_work(OWNER, "bird", vec![rules]).unwrap();
    let Some(TrustedStep::Command(pinned)) = ledger.trusted_publish(OWNER, &list, 11).unwrap() else { panic!("read-only list runs") };
    assert!(pinned.ends_with("GH_HOST='github.com' gh issue list -R csheargm/hagency-demo-sandbox --state open") && pinned.starts_with("GIT_CONFIG_COUNT="));
    for asks in [
        proposal(ROOM, "gh issue list -R other/repo"),
        proposal(ROOM, "gh issue list -R csheargm/hagency-demo-sandbox; rm -rf x"),
        proposal(ROOM, "gh api -X POST repos/csheargm/hagency-demo-sandbox/issues"),
        proposal(ROOM, "gh issue close 1 -R csheargm/hagency-demo-sandbox"),
        proposal("!other:example.org", "gh issue list -R csheargm/hagency-demo-sandbox"),
    ] {
        assert_eq!(ledger.trusted_publish(OWNER, &asks, 11).unwrap(), None, "{}", asks.arguments["command"]);
    }
}

#[test]
fn a_legacy_watch_counts_only_while_on() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = ask_owner_ledger(db.path());
    let list = proposal(ROOM, "gh pr list -R csheargm/hagency-demo-sandbox");
    let set = |mode| WatchSetting { mode, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: false };
    ledger.set_issue_watch(OWNER, "bird", set(WatchMode::Trusted), 1).unwrap();
    assert!(ledger.trusted_publish(OWNER, &list, 11).unwrap().is_some());
    ledger.set_issue_watch(OWNER, "bird", set(WatchMode::Off), 1).unwrap();
    assert_eq!(ledger.trusted_publish(OWNER, &list, 11).unwrap(), None, "a paused watch grants nothing");
}

/// GPT review: moving a repository's reports to another room through the
/// work list leaves the legacy watch's room with nothing.
#[test]
fn a_moved_report_room_no_longer_grants_the_old_room() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = ask_owner_ledger(db.path());
    let list = proposal(ROOM, "gh issue list -R csheargm/hagency-demo-sandbox");
    let legacy = WatchSetting { mode: WatchMode::Trusted, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: false };
    ledger.set_issue_watch(OWNER, "bird", legacy, 1).unwrap();
    let moved = OwnerRules { repo: REPO.into(), room_id: "!new:example.org".into(), ..Default::default() };
    ledger.set_work(OWNER, "bird", vec![moved]).unwrap();
    assert_eq!(ledger.trusted_publish(OWNER, &list, 11).unwrap(), None);
    assert!(super::read_only_query(&ledger.db, &proposal("!new:example.org", "gh issue list -R csheargm/hagency-demo-sandbox")).unwrap());
}
