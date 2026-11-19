//! The gate under the room's autonomy level (ADR 0030): reviews and readying
//! run in Assist and Autonomous, a merge without a person only in an
//! Autonomous, running room, and only for Agents of the room's owner or a
//! maintainer. Off and Pause stop every step.
use super::gate::{self, APPROVE_MARKER};
use super::tests::{bash, file, ledger_with, merge_line, Fake, HEAD, ROOM};
use super::Decision;
use crate::room_autonomy::{AutonomySnapshot, Level, ASSIST_REASON, NOT_MAINTAINER_REASON};
use crate::Ledger;

const OWNER: &str = "@owner:example.org";

fn room(ledger: &mut Ledger, level: Level, paused: bool, maintainer: bool) {
    let snapshot = AutonomySnapshot { room_id: ROOM.into(), level: Some(level), paused, unread: false, owner_maintainer: maintainer, as_of: crate::room_autonomy::as_of::next_for_test(), sticky: false };
    ledger.set_room_autonomy(OWNER, &snapshot, super::room_rules::unix_now()).unwrap();
}

/// The reviewer approves the fixer's docs change; the fixer then asks to merge.
async fn review_then_merge(ledger: &mut Ledger, dir: &std::path::Path) -> (Option<bool>, Option<bool>) {
    let src = Fake::new(vec![file("docs/a.md", 3)]);
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""), dir, "reviewer");
    let reviewed = gate::auto_answer(ledger, OWNER, &review, "rv", 11, &src).await;
    ledger.settle_agent_review("rv", reviewed == Some(true)).unwrap();
    (reviewed, gate::auto_answer(ledger, OWNER, &bash(&merge_line(), dir, "fixer"), "mg", 11, &src).await)
}

fn loop_ledger(dir: &std::path::Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["fixer", "reviewer"]);
    for agent in ["fixer", "reviewer"] {
        ledger.allow_loop(OWNER, agent, true);
    }
    ledger
}

#[tokio::test]
async fn each_level_runs_its_steps() {
    for (level, paused, review, merge) in [
        (Level::Off, false, None, None),
        (Level::Assist, false, Some(true), None),
        (Level::Autonomous, false, Some(true), Some(true)),
        (Level::Autonomous, true, None, None),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = loop_ledger(temp.path());
        room(&mut ledger, level, paused, true);
        assert_eq!(review_then_merge(&mut ledger, temp.path()).await, (review, merge), "{level:?} paused={paused}");
    }
}

/// Assist: the merge waits for a person, and the hold names why.
#[tokio::test]
async fn assist_says_a_person_delivers() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = loop_ledger(temp.path());
    room(&mut ledger, Level::Assist, false, true);
    review_then_merge(&mut ledger, temp.path()).await;
    let command = gate::parse_command(&bash(&merge_line(), temp.path(), "fixer")).unwrap();
    let ready = super::review_loop::merge_readiness(&mut ledger, &command, ("fixer", ROOM), &Fake::new(vec![file("docs/a.md", 3)])).await;
    assert_eq!(ready.map(|(_, d)| d), Some(Decision::NeedsHuman(ASSIST_REASON.into())));
}

/// A plain member's Agents build and review but never merge, card or not;
/// once the owner is a maintainer again the same merge goes through.
#[tokio::test]
async fn a_members_agent_never_merges_and_a_maintainers_does() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = loop_ledger(temp.path());
    room(&mut ledger, Level::Autonomous, false, false);
    assert_eq!(review_then_merge(&mut ledger, temp.path()).await, (Some(true), Some(false)), "reviewed, merge refused");
    let command = gate::parse_command(&bash(&merge_line(), temp.path(), "fixer")).unwrap();
    let ready = super::review_loop::merge_readiness(&mut ledger, &command, ("fixer", ROOM), &Fake::new(vec![file("docs/a.md", 3)])).await;
    assert_eq!(ready.map(|(_, d)| d), Some(Decision::NeedsHuman(NOT_MAINTAINER_REASON.into())));
    room(&mut ledger, Level::Autonomous, false, true);
    let merge = gate::auto_answer(&mut ledger, OWNER, &bash(&merge_line(), temp.path(), "fixer"), "mg2", 11, &Fake::new(vec![file("docs/a.md", 3)])).await;
    assert_eq!(merge, Some(true), "promoted: merges");
}

/// Batch GPT review: a background merge in a project room with no snapshot
/// is refused like a foreground one, never left to a card.
#[tokio::test]
async fn a_background_merge_in_an_unread_project_room_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = loop_ledger(temp.path());
    ledger.db.execute("DELETE FROM room_autonomy", []).unwrap();
    let mut merge = bash(&merge_line(), temp.path(), "fixer");
    merge.arguments["run_in_background"] = serde_json::Value::Bool(true);
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &merge, "mg", 11, &Fake::new(vec![file("docs/a.md", 3)])).await, Some(false));
}
