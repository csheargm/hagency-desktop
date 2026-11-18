//! R6 flow (E2E finding, 2026-11-17 21:37): Anna, a member of Betty's room,
//! asks Betty's Claude Agent. Its `pwd; ls` runs from the Room workspace with
//! no card; its `touch` puts a card in Betty's approval queue (the bridge to
//! her inbox), and once Betty approves it runs. Before, both were refused
//! with "the owner's rules blocked" and Betty saw nothing.
use super::claude_turn::ToolAnswer;
use crate::member_inspection::tests::{OWNER, owner_rule_only};
use serde_json::json;

const ANNA: &str = "@anna:example.org";

fn scope(requester: &str) -> crate::Scope {
    crate::Scope { agent: "bird".into(), binding: "b".into(), room: "!e2e:example.org".into(), requester: requester.into(), thread: "t".into() }
}

#[tokio::test]
async fn a_members_request_cards_the_owner_and_runs_once_approved() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    let (queue, mut inbox) = crate::codex::approval_queue(4).unwrap();
    let github = crate::merge_policy::github::GhCli { token: None };
    let anna = scope(ANNA);
    let dispatch = format!("d-{ANNA}");
    let look = super::permission(&mut ledger, OWNER, &anna, &dispatch, "c1", "claude.Bash".into(), json!({"command": "pwd; ls"}), &ws, &ws, &queue, &github).await;
    assert_eq!(look, ToolAnswer::AllowCommand(format!("cd '{}' && ( pwd; ls )", ws.display())), "a look needs no card");
    assert!(inbox.try_recv().is_err(), "and Betty saw no card for it");
    let betty = async {
        let card = inbox.recv().await.expect("a card reaches Betty's queue");
        assert_eq!(card.proposal.scope.requester, ANNA);
        assert_eq!(card.proposal.arguments["command"], "touch notes2.md");
        card.decide(true);
    };
    let (answer, ()) = tokio::join!(
        super::permission(&mut ledger, OWNER, &anna, &dispatch, "c2", "claude.Bash".into(), json!({"command": "touch notes2.md"}), &ws, &ws, &queue, &github),
        betty
    );
    assert_eq!(answer, ToolAnswer::Allow, "Betty approved: it runs");
}

/// e2e Nov 17 23:32: #69's hand-off run asked to run `gh pr create`, the
/// card ended unanswered, and the issue loop counted the run as a failed
/// attempt. The ledger now marks that hand-off, and only an unanswered card.
#[tokio::test]
async fn an_unanswered_card_marks_its_issue_hand_off() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    let (queue, mut inbox) = crate::codex::approval_queue(4).unwrap();
    let github = crate::merge_policy::github::GhCli { token: None };
    let anna = scope(ANNA);
    let dispatch = format!("d-{ANNA}"); // the run's reserved call, as a hand-off of #69
    ledger.db.execute("INSERT INTO handoff_issues VALUES (?,'acme/app',69)", [&dispatch]).unwrap();
    let marked = |ledger: &crate::Ledger| -> i64 { ledger.db.query_row("SELECT count(*) FROM handoff_unanswered WHERE dispatch=?", [&dispatch], |r| r.get(0)).unwrap() };
    let card = std::time::Duration::from_secs(5);
    let refuse = async { tokio::time::timeout(card, inbox.recv()).await.expect("a card").unwrap().decide(false) };
    let (answer, ()) = tokio::join!(
        super::permission(&mut ledger, OWNER, &anna, &dispatch, "c1", "claude.Bash".into(), json!({"command": "touch a.md"}), &ws, &ws, &queue, &github),
        refuse
    );
    assert_eq!((answer, marked(&ledger)), (ToolAnswer::Deny, 0), "a refusal is the Agent's run as before");
    let walk_away = async { drop(tokio::time::timeout(card, inbox.recv()).await.expect("a card").unwrap()) };
    let (answer, ()) = tokio::join!(
        super::permission(&mut ledger, OWNER, &anna, &dispatch, "c2", "claude.Bash".into(), json!({"command": "touch b.md"}), &ws, &ws, &queue, &github),
        walk_away
    );
    assert_eq!((answer, marked(&ledger)), (ToolAnswer::Unanswered, 1));
}
