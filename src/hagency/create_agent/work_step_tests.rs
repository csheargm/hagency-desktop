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
fn an_empty_step_is_skipped_and_lines_are_repositories_only() {
    assert!(draft("  \n ").wanted().unwrap().is_empty());
    assert_eq!(draft("acme/app; acme/docs\nacme/x").wanted().unwrap().len(), 3);
    for bad in ["acme", "acme/app maintainer", "acme/app; ACME/app"] {
        assert!(draft(bad).wanted().is_err(), "{bad}");
    }
}

#[tokio::test]
async fn every_repository_must_be_readable_and_merge_rights_are_reported_not_required() {
    let ok = check(&anna(), &draft("acme/app; acme/docs").wanted().unwrap()).await.unwrap();
    assert!(ok[0].contains("can merge") && !ok[1].contains("can merge"), "{ok:?}");
    let missing = check(&anna(), &draft("acme/app; acme/secret").wanted().unwrap()).await.unwrap_err();
    assert!(missing.contains("acme/secret") && !missing.contains("acme/app"), "{missing}");
}

#[test]
fn limits_land_on_the_same_work_list_the_panel_edits() {
    let mut step = draft("acme/app; acme/docs");
    step.issues_per_day = "2".into();
    let work = step.entries(&step.wanted().unwrap(), "!dm:s", 50).unwrap();
    assert_eq!((work[0].issues_per_day, work[0].open_prs, work[0].room_id.as_str(), work[0].since), (Some(2), Some(DEFAULT_OPEN_PRS), "!dm:s", 50));
    let text = crate::hagency::issue_watch::work_lines::render(&work);
    assert!(!text.contains("maintainer"), "no role in the panel text: {text}");
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
