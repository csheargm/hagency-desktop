//! The GPT security review of the gate (Nov 15): head binding, base-commit
//! policy, vacuous CI, renames, opaque files, line-count overflow.
use super::*;

fn human(d: Decision) -> bool {
    matches!(d, Decision::NeedsHuman(_))
}
fn judge(files: Vec<ChangedFile>, checks: Vec<Check>) -> Decision {
    evaluate(&policy(), &facts(files, checks), &reviewers(&["reviewer"]), "fixer")
}

#[test]
fn required_ci_with_no_checks_needs_a_person() {
    assert_eq!(judge(vec![file("README.md", 1)], vec![]), Decision::NeedsHuman("no CI checks reported".into()));
    let no_ci = parse_policy("low_risk_paths: [\"**/*.md\"]\nrequire_ci: false\n").unwrap();
    assert_eq!(evaluate(&no_ci, &facts(vec![file("README.md", 1)], vec![]), &reviewers(&["reviewer"]), "fixer"), Decision::Merge);
}

#[test]
fn renames_and_opaque_files_need_a_person() {
    let renamed = ChangedFile { previous_path: Some(".hagency/merge-policy.yml".into()), ..file("docs/old.md", 1) };
    assert!(human(judge(vec![renamed], vec![Check::Passed])), "a protected file moved into an allowed path");
    let harmless = ChangedFile { previous_path: Some("docs/a.md".into()), ..file("docs/b.md", 1) };
    assert!(human(judge(vec![harmless], vec![Check::Passed])), "every rename goes to a person");
    let binary = ChangedFile { opaque: true, ..file("docs/logo.md", 0) };
    assert!(human(judge(vec![binary], vec![Check::Passed])));
    let huge = ChangedFile { deletions: u64::MAX, ..file("a.md", u64::MAX) };
    assert!(human(judge(vec![huge.clone(), huge.clone()], vec![Check::Passed])), "the line count never wraps");
    let unlimited = parse_policy("low_risk_paths: [\"**\"]\nmax_changed_lines: 18446744073709551615\nrequire_ci: false\n").unwrap();
    assert!(human(evaluate(&unlimited, &facts(vec![huge], vec![]), &reviewers(&["reviewer"]), "fixer")), "overflow is never equal to the limit");
}

#[test]
fn compare_files_mark_renames_binaries_and_odd_statuses() {
    let view = json!({"url": URL, "state": "OPEN", "baseRefOid": BASE, "headRefOid": HEAD, "statusCheckRollup": []});
    let entry = |status: &str, changes: u64, patch: bool| json!({"filename": "docs/a.md", "status": status, "additions": changes, "deletions": 0, "changes": changes, "has_patch": patch});
    let opaque = |e: Value| github::view_from_json(&view, &json!([e])).unwrap().facts.files[0].opaque;
    assert!(!opaque(entry("modified", 2, true)));
    assert!(opaque(entry("modified", 0, false)), "binary: no lines, no patch");
    assert!(opaque(entry("modified", 3, false)), "no patch to review");
    assert!(opaque(entry("changed", 2, true)), "mode change");
    let mut moved = entry("renamed", 0, false);
    moved["previous_filename"] = json!(".github/workflows/ci.yml");
    let files = github::view_from_json(&view, &json!([moved])).unwrap().facts.files;
    assert_eq!(files[0].previous_path.as_deref(), Some(".github/workflows/ci.yml"));
    let full: Vec<Value> = (0..300).map(|_| entry("modified", 1, true)).collect();
    assert!(github::view_from_json(&view, &json!(full)).is_none(), "a full compare page may be truncated");
}

#[test]
fn a_merge_must_pin_the_head_commit() {
    let dir = tempfile::tempdir().unwrap();
    let parse = |line: &str| gate::parse_command(&bash(line, dir.path(), "a"));
    assert_eq!(parse(&merge_line()).unwrap().head.as_deref(), Some(HEAD));
    assert_eq!(parse("gh pr merge 7 --squash -R csheargm/hagency-demo-sandbox").unwrap().head, None);
    for line in ["gh pr merge 7 --squash --match-head-commit", "gh pr merge 7 --squash --match-head-commit abc123", "gh pr review 7 --approve --match-head-commit 1111111111111111111111111111111111111111"] {
        assert!(parse(line).is_none(), "{line}");
    }
}

async fn approved(ledger: &mut Ledger, dir: &Path, source: &Fake) {
    let owner = "@owner:example.org";
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(owner, agent, true).unwrap();
    }
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}\""), dir, "reviewer");
    assert_eq!(gate::auto_answer(ledger, owner, &review, 11, source).await, Some(true));
}

#[tokio::test]
async fn an_unpinned_or_stale_merge_goes_to_the_owner() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    let docs = Fake::new(vec![file("README.md", 5)]);
    approved(&mut ledger, temp.path(), &docs).await;
    let unpinned = bash("gh pr merge 7 --squash -R csheargm/hagency-demo-sandbox", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &unpinned, 11, &docs).await, None, "GitHub would merge whatever the head is");
    *docs.head.borrow_mut() = "3333333333333333333333333333333333333333".into();
    let pinned_old = bash(&merge_line(), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &pinned_old, 11, &docs).await, None, "the head moved after the review");
    let pinned_new = bash("gh pr merge 7 --squash --match-head-commit 3333333333333333333333333333333333333333 -R csheargm/hagency-demo-sandbox", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &pinned_new, 11, &docs).await, None, "the approval was for the old head");
    // One dispatch authorizes once, so the fresh review is recorded directly.
    ledger.record_agent_review(URL, "3333333333333333333333333333333333333333", "reviewer").unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &pinned_new, 11, &docs).await, Some(true), "a fresh review of the new head counts");
}
