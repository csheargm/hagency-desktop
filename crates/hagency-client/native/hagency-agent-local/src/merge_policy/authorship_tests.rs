//! GPT batch review P1-1: what counts as this ledger's evidence of who
//! published a pull request's head branch.
use super::*;
use crate::merge_policy::github::{HeadRef, PrView};
use crate::merge_policy::tests::{bash, ledger_with};
use crate::merge_policy::PrFacts;

const REPO: &str = "Acme/App";
const OPENED: i64 = 1_800_000_000;

fn view(branch: Option<&str>) -> PrView {
    let head_ref = branch.map(|b| HeadRef { branch: b.into(), created_at: OPENED });
    PrView { url: format!("https://github.com/{REPO}/pull/7"), base_sha: "1".repeat(40), head_sha: "2".repeat(40), author: "anna".into(), opened_by: None, head_ref, facts: PrFacts { open: true, files: vec![], checks: vec![] } }
}

#[test]
fn a_create_names_its_repository_and_head_in_any_segment() {
    let read = |line: &str| created_branch(line);
    let want = Some((REPO.to_owned(), "fix-3".to_owned()));
    assert_eq!(read("gh pr create --repo Acme/App --head fix-3 -t 'x' -b 'y'"), want);
    assert_eq!(read("cd app && GH_HOST=github.com gh pr create -R Acme/App --head=fix-3 --draft"), want);
    assert_eq!(read("gh pr create -R Acme/App -t '--head evil' -H fix-3"), want, "a title's value is never an option");
    for line in ["gh pr create -R Acme/App -t x", "gh pr create --head fix-3", "gh pr create -R Acme/App -H fork:fix-3", "gh pr view -R Acme/App -H fix-3", "gh pr create -R github.com/a/b -H x"] {
        assert_eq!(read(line), None, "{line}");
    }
}

#[test]
fn every_publisher_of_the_branch_is_an_author_and_a_recent_create_makes_it_known() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["anna", "yue"]);
    let push = TrustedStep::Host(HostStep::Push { dir: temp.path().into(), remote: "origin".into(), branch: "fix-3".into(), repo: "acme/app".into(), upstream: true });
    note(&ledger.db, &bash("git -C app push -u origin fix-3", temp.path(), "yue"), Some(&push), OPENED - 600).unwrap();
    assert_eq!(ledger.branch_authors(REPO, &view(Some("fix-3"))).unwrap(), (vec!["yue".to_owned()], false), "a push alone does not say who opened it");
    let create = bash("gh pr create --repo Acme/App --head fix-3 -b x", temp.path(), "anna");
    note(&ledger.db, &create, None, OPENED - 30).unwrap();
    assert_eq!(ledger.branch_authors(REPO, &view(Some("fix-3"))).unwrap(), (vec!["anna".to_owned(), "yue".to_owned()], false), "allowed is not ran");
    // GPT review of these fixes: a create that failed never vouches; one that ran does.
    ledger.hold_create("failed", &create, OPENED - 30).unwrap();
    ledger.settle_tool_call("failed", false).unwrap();
    assert!(!ledger.branch_authors(REPO, &view(Some("fix-3"))).unwrap().1, "a failed create vouches for nothing");
    ledger.hold_create("ran", &create, OPENED - 30).unwrap();
    ledger.settle_tool_call("ran", true).unwrap();
    assert_eq!(ledger.branch_authors("acme/app", &view(Some("fix-3"))).unwrap(), (vec!["anna".to_owned(), "yue".to_owned()], true));
    assert_eq!(ledger.branch_authors(REPO, &view(Some("fix-4"))).unwrap(), (vec![], false), "another branch");
    assert_eq!(ledger.branch_authors(REPO, &view(None)).unwrap(), (vec![], false), "a fork's branch is never ours");
    note(&ledger.db, &bash("gh pr view 7 -R Acme/App", temp.path(), "bird"), None, OPENED).unwrap();
    assert_eq!(ledger.branch_authors(REPO, &view(Some("fix-3"))).unwrap().0.len(), 2, "a read publishes nothing");
}
