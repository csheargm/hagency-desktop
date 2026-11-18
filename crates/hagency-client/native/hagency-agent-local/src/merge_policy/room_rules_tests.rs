//! Room merge rules (ADR 0029): recorded only through the owner, judged
//! only from the watch room's fresh snapshot, never from the repository.
use super::*;
use crate::merge_policy::gate::{self, APPROVE_MARKER};
use crate::merge_policy::tests::{bash, file, ledger_with, merge_line, set_room_rules, Fake, HEAD, ROOM, SANDBOX};
use crate::merge_policy::Decision;

const OWNER: &str = "@owner:example.org";
const SANDBOX_RULES: &str = "low_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: false\n";

fn snapshot(policy: Option<&str>) -> RoomRulesSnapshot {
    RoomRulesSnapshot { repo: SANDBOX.into(), room_id: ROOM.into(), event_id: Some("$e1".into()), sender: Some("@admin:example.org".into()), policy: policy.map(str::to_owned), unread: false }
}

/// The fixer asks to merge after the reviewer Agent approved: the gate's answer.
async fn approved_merge(ledger: &mut Ledger, dir: &std::path::Path, src: &Fake) -> Option<bool> {
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(OWNER, agent, true).unwrap();
    }
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""), dir, "reviewer");
    let reviewed = gate::auto_answer(ledger, OWNER, &review, "rv", 11, src).await;
    ledger.settle_agent_review("rv", reviewed == Some(true)).unwrap();
    gate::auto_answer(ledger, OWNER, &bash(&merge_line(), dir, "fixer"), "mg", 11, src).await
}

#[tokio::test]
async fn room_rules_let_a_low_risk_change_merge() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    set_room_rules(&mut ledger, SANDBOX, Some(SANDBOX_RULES));
    assert_eq!(approved_merge(&mut ledger, temp.path(), &Fake::new(vec![file("docs/a.md", 3)])).await, Some(true));
}

#[tokio::test]
async fn no_stale_or_invalid_room_rules_need_a_person() {
    for (name, rules, age) in [("none", None, 0), ("stale", Some(SANDBOX_RULES), ROOM_RULES_TTL_SECS + 1), ("invalid", Some("merge_everything: true\n"), 0)] {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
        ledger.set_room_merge_rules(OWNER, &snapshot(rules), unix_now() - age).unwrap();
        let src = Fake::new(vec![file("docs/a.md", 3)]);
        assert_eq!(approved_merge(&mut ledger, temp.path(), &src).await, None, "{name}: the card asks");
        let command = gate::parse_command(&bash(&merge_line(), temp.path(), "fixer")).unwrap();
        let (_, decision) = crate::merge_policy::review_loop::merge_readiness(&mut ledger, &command, ("fixer", "!r:x"), &src).await.unwrap();
        let Decision::NeedsHuman(reason) = decision else { panic!("{name}: no merge") };
        assert!([NO_ROOM_RULES, STALE_ROOM_RULES, INVALID_ROOM_RULES].contains(&reason.as_str()), "{name}: {reason}");
    }
}

#[tokio::test]
async fn the_room_notice_reason_names_where_an_admin_sets_rules() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer"]);
    set_room_rules(&mut ledger, SANDBOX, None);
    let command = gate::parse_command(&bash(&merge_line(), temp.path(), "fixer")).unwrap();
    let (_, decision) = crate::merge_policy::review_loop::merge_readiness(&mut ledger, &command, ("fixer", "!r:x"), &Fake::new(vec![file("a.md", 1)])).await.unwrap();
    assert_eq!(decision, Decision::NeedsHuman(NO_ROOM_RULES.into()));
    assert!(NO_ROOM_RULES.contains("Room info → Status"));
}

#[tokio::test]
async fn only_the_watch_rooms_rules_count() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    set_room_rules(&mut ledger, SANDBOX, None);
    // Generous rules recorded for another room do not judge this repository.
    let other = RoomRulesSnapshot { room_id: "!elsewhere:x".into(), ..snapshot(Some("low_risk_paths: [\"**\"]\n")) };
    ledger.set_room_merge_rules(OWNER, &other, unix_now()).unwrap();
    assert_eq!(approved_merge(&mut ledger, temp.path(), &Fake::new(vec![file("docs/a.md", 3)])).await, None);
    assert_eq!(ledger.watch_room("fixer", SANDBOX).unwrap().as_deref(), Some(ROOM), "the only watch room on this device");
    assert_eq!(ledger.watch_room("fixer", "acme/unwatched").unwrap(), None);
}

#[tokio::test]
async fn a_paused_watch_supplies_no_rules() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    set_room_rules(&mut ledger, SANDBOX, Some(SANDBOX_RULES));
    // The fixer's own watch of the sandbox is paused and points at a generous room.
    let paused = crate::issue_watch::WatchSetting { mode: crate::issue_watch::WatchMode::Trusted, repo: SANDBOX.into(), room_id: "!paused:x".into(), ..Default::default() };
    ledger.set_issue_watch(OWNER, "fixer", paused, 1).unwrap();
    ledger.end_watch_trial(OWNER, "fixer", SANDBOX).unwrap();
    ledger.set_room_merge_rules(OWNER, &RoomRulesSnapshot { room_id: "!paused:x".into(), ..snapshot(Some("low_risk_paths: [\"**\"]\nrequire_ci: false\n")) }, unix_now()).unwrap();
    let off = crate::issue_watch::WatchSetting { mode: crate::issue_watch::WatchMode::Off, repo: SANDBOX.into(), room_id: "!paused:x".into(), ..Default::default() };
    ledger.set_issue_watch(OWNER, "fixer", off, 2).unwrap();
    assert_eq!(ledger.watch_room("fixer", SANDBOX).unwrap().as_deref(), Some(ROOM), "the active watch's room, not the paused one");
    assert_eq!(approved_merge(&mut ledger, temp.path(), &Fake::new(vec![file("src/main.rs", 1)])).await, None, "the paused room's generous rules do not count");
}

#[tokio::test]
async fn a_repository_policy_file_and_protected_paths_change_nothing() {
    // The gate reads no file at all (PrSource has no policy read); a pull
    // request adding one is protected and asks a person.
    for path in [".hagency/merge-policy.yml", ".github/workflows/ci.yml", "docs/CODEOWNERS"] {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
        set_room_rules(&mut ledger, SANDBOX, Some("low_risk_paths: [\"**\"]\nrequire_ci: false\n"));
        assert_eq!(approved_merge(&mut ledger, temp.path(), &Fake::new(vec![file(path, 1)])).await, None, "{path}");
    }
}

#[test]
fn only_the_owner_records_rules_and_bad_snapshots_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer"]);
    assert!(ledger.set_room_merge_rules("@intruder:example.org", &snapshot(Some(SANDBOX_RULES)), 1).is_err(), "only the owner");
    for bad in [
        RoomRulesSnapshot { repo: "nope".into(), ..snapshot(Some(SANDBOX_RULES)) },
        RoomRulesSnapshot { room_id: "r:x".into(), ..snapshot(Some(SANDBOX_RULES)) },
        RoomRulesSnapshot { event_id: None, ..snapshot(Some(SANDBOX_RULES)) },
        RoomRulesSnapshot { sender: Some("admin".into()), ..snapshot(Some(SANDBOX_RULES)) },
    ] {
        assert!(ledger.set_room_merge_rules(OWNER, &bad, 1).is_err(), "{bad:?}");
    }
    // Oversized rules replace the old ones and count as invalid (GPT review).
    ledger.set_room_merge_rules(OWNER, &snapshot(Some(SANDBOX_RULES)), 7).unwrap();
    ledger.set_room_merge_rules(OWNER, &snapshot(Some(&format!("{SANDBOX_RULES}#{}", "x".repeat(MAX_POLICY_BYTES)))), 8).unwrap();
    assert_eq!(ledger.room_merge_rules(SANDBOX, ROOM, 8).unwrap(), RoomRules::Missing(INVALID_ROOM_RULES));
    ledger.set_room_merge_rules(OWNER, &snapshot(Some(SANDBOX_RULES)), 7).unwrap();
    let (event, sender, _, at) = ledger.room_merge_rules_record("CSheargm/Hagency-Demo-Sandbox", ROOM).unwrap().unwrap();
    assert_eq!((event.as_str(), sender.as_str(), at), ("$e1", "@admin:example.org", 7), "the event id is recorded per repo and room");
    assert!(serde_json::from_value::<RoomRulesSnapshot>(serde_json::json!({"repo": SANDBOX, "roomId": ROOM, "trusted": true})).is_err(), "unknown fields are refused");
}

/// GPT integration review: an admin tightens the rules and the desktop's next
/// read fails; the old snapshot stops counting at once, not after the TTL.
#[test]
fn an_unread_room_withdraws_the_last_rules_at_once() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["fixer"]);
    ledger.set_room_merge_rules(OWNER, &snapshot(Some(SANDBOX_RULES)), 7).unwrap();
    assert!(ledger.room_merge_rules(SANDBOX, ROOM, 8).unwrap().policy().is_some());
    let unread = RoomRulesSnapshot { event_id: None, sender: None, policy: None, unread: true, ..snapshot(None) };
    ledger.set_room_merge_rules(OWNER, &unread, 8).unwrap();
    assert_eq!(ledger.room_merge_rules(SANDBOX, ROOM, 9).unwrap(), RoomRules::Missing(UNREAD_ROOM_RULES));
    let contradictory = RoomRulesSnapshot { unread: true, ..snapshot(Some(SANDBOX_RULES)) };
    assert!(ledger.set_room_merge_rules(OWNER, &contradictory, 9).is_err(), "unread carries no rules");
    let wire = serde_json::json!({"repo": SANDBOX, "roomId": ROOM, "unread": true});
    assert!(serde_json::from_value::<RoomRulesSnapshot>(wire).unwrap().unread, "the owner API takes the flag");
}

#[test]
fn rules_render_and_parse_back_and_unsafe_globs_are_refused() {
    let suggested = suggested_policy();
    let text = render_policy(&suggested).unwrap();
    assert_eq!(parse_policy(&text), Some(suggested.clone()));
    assert_eq!(rules_of(&content_of(&text)), Some(suggested.clone()));
    let trusted = parse_policy("tier: trusted\npaths: [\"src/**\"]\nmax_lines: 9\nalways_human: [\"secrets/**\"]\n").unwrap();
    assert_eq!(parse_policy(&render_policy(&trusted).unwrap()), Some(trusted));
    for glob in ["a\"b", "a, b", "a #b", "[x]", " pad", "line\nbreak", ""] {
        let policy = MergePolicy { low_risk_paths: vec![glob.into()], ..suggested.clone() };
        assert_eq!(render_policy(&policy), None, "{glob:?}");
    }
}

#[test]
fn the_suggested_rules_take_root_doc_files_but_never_legal_or_protected_ones() {
    let policy = suggested_policy();
    let facts = |path: &str| crate::merge_policy::PrFacts { open: true, files: vec![file(path, 1)], checks: vec![] };
    for low in ["AUTHORS", "CONTRIBUTORS", "MAINTAINERS", "CHANGELOG", "NOTICE", "notes.txt", "README.md", "docs/x.rs"] {
        assert_eq!(crate::merge_policy::file_reason(&policy, &facts(low)), None, "{low}");
    }
    for person in ["LICENSE", "LICENSE.txt", "COPYING", "LICENSE.md", "requirements.txt", "src/AUTHORS", "src/main.rs", ".github/x.md", ".hagency/x.md", "CODEOWNERS"] {
        assert!(crate::merge_policy::file_reason(&policy, &facts(person)).is_some(), "{person}");
    }
    assert_eq!(crate::merge_policy::evaluate(&policy, &facts("AUTHORS"), &["reviewer".into()], "fixer"), Decision::Merge, "sandbox PR #80");
}
