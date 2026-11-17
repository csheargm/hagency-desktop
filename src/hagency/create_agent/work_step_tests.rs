use super::*;
use std::collections::HashMap;

struct Fake {
    login: &'static str,
    repos: HashMap<&'static str, &'static str>,
}
impl Github for Fake {
    async fn login(&self) -> Result<String, String> {
        Ok(self.login.into())
    }
    async fn access(&self, repo: &str) -> Result<Access, String> {
        Ok(self.repos.get(repo).map_or(Access::NotFound, |p| Access::Readable((*p).into())))
    }
}
fn anna() -> Fake {
    Fake { login: "anna", repos: HashMap::from([("acme/app", "write"), ("acme/docs", "read")]) }
}
fn draft(repos: &str) -> WorkDraft {
    WorkDraft { repos: repos.into(), ..Default::default() }
}

#[test]
fn an_empty_step_is_skipped_and_the_role_defaults_to_developer() {
    assert!(draft("  \n ").wanted().unwrap().is_empty());
    let wanted = draft("acme/app; acme/docs reviewer\nacme/x maintainer").wanted().unwrap();
    assert_eq!(wanted.iter().map(|w| w.role).collect::<Vec<_>>(), [Role::Developer, Role::Reviewer, Role::Maintainer]);
}

#[test]
fn bad_lines_are_refused_before_github_is_asked() {
    for bad in ["acme", "acme/app boss", "acme/app developer all", "acme/app; ACME/app reviewer"] {
        assert!(draft(bad).wanted().is_err(), "{bad}");
    }
}

#[tokio::test]
async fn every_repository_must_be_readable_and_maintainer_needs_merge_rights() {
    let ok = check(&anna(), &draft("acme/app maintainer; acme/docs reviewer").wanted().unwrap()).await.unwrap();
    assert_eq!(ok.len(), 2);
    let missing = check(&anna(), &draft("acme/app; acme/secret").wanted().unwrap()).await.unwrap_err();
    assert!(missing.contains("acme/secret") && !missing.contains("acme/app"), "{missing}");
    let no_merge = check(&anna(), &draft("acme/docs maintainer").wanted().unwrap()).await.unwrap_err();
    assert!(no_merge.contains("merge rights") && no_merge.contains("read"), "{no_merge}");
}

#[test]
fn limits_land_on_developer_entries_of_the_same_work_list_the_panel_edits() {
    let mut step = draft("acme/app; acme/docs reviewer");
    step.issues_per_day = "2".into();
    let work = step.entries(&step.wanted().unwrap(), "!dm:s", 50).unwrap();
    assert_eq!((work[0].issues_per_day, work[0].open_prs, work[0].room_id.as_str(), work[0].since), (Some(2), Some(DEFAULT_OPEN_PRS), "!dm:s", 50));
    assert_eq!((work[1].issues_per_day, work[1].open_prs), (None, None), "a reviewer claims nothing");
    // The panel's text round-trips what the step saved.
    let text = crate::hagency::issue_watch::work_lines::render(&work);
    assert_eq!(crate::hagency::issue_watch::work_lines::parse(&text, "!dm:s", &work, 99).unwrap(), work);
    for bad in ["0", "101", "lots"] {
        step.open_prs = bad.into();
        assert!(step.entries(&step.wanted().unwrap(), "!dm:s", 50).is_err(), "{bad}");
    }
}

#[test]
fn the_budget_is_optional_and_must_be_a_positive_number() {
    assert_eq!(WorkDraft::default().budget().unwrap(), None);
    assert_eq!(WorkDraft { budget: "200,000".into(), ..Default::default() }.budget().unwrap(), Some(200_000));
    assert!(WorkDraft { budget: "0".into(), ..Default::default() }.budget().is_err());
}
