//! GPT integration review (Nov 15): `gh pr ready` and approving reviews skip
//! the card only for a low-risk pull request under the base-commit policy.
use super::*;

const READY: &str = "gh pr ready 7 -R csheargm/hagency-demo-sandbox";

fn review() -> String {
    format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}\"")
}

async fn answer(files: Vec<ChangedFile>, line: &str) -> Option<bool> {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer"]);
    ledger.set_auto_merge(owner, "fixer", true).unwrap();
    gate::auto_answer(&mut ledger, owner, &bash(line, temp.path(), "fixer"), 11, &Fake::new(files)).await
}

#[tokio::test]
async fn a_low_risk_pull_request_is_readied_and_reviewed_without_a_card() {
    assert_eq!(answer(vec![file("README.md", 5)], READY).await, Some(true));
    assert_eq!(answer(vec![file("docs/a.md", 5)], &review()).await, Some(true));
}

#[tokio::test]
async fn anything_outside_the_policy_gets_the_owner_card() {
    let renamed = ChangedFile { previous_path: Some("old.md".into()), ..file("README.md", 1) };
    let opaque = ChangedFile { opaque: true, ..file("README.md", 0) };
    let cases = [vec![file("src/main.rs", 1)], vec![file("README.md", 51)], vec![file(".github/workflows/x.md", 1)], vec![renamed], vec![opaque], vec![]];
    for files in cases {
        for line in [READY.to_owned(), review()] {
            assert_eq!(answer(files.clone(), &line).await, None, "{line} {files:?}");
        }
    }
}

#[tokio::test]
async fn a_repository_other_than_the_pull_requests_gets_the_owner_card() {
    assert_eq!(answer(vec![file("README.md", 1)], "gh pr ready 7 -R someone/else").await, None);
}

#[tokio::test]
async fn a_review_is_recorded_for_the_head_that_was_judged() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["reviewer"]);
    ledger.set_auto_merge(owner, "reviewer", true).unwrap();
    let docs = Fake::new(vec![file("README.md", 1)]);
    assert_eq!(gate::auto_answer(&mut ledger, owner, &bash(&review(), temp.path(), "reviewer"), 11, &docs).await, Some(true));
    assert_eq!(ledger.agent_reviewers(URL, HEAD).unwrap(), ["reviewer"]);
    assert!(ledger.agent_reviewers(URL, BASE).unwrap().is_empty());
}
