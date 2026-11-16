//! The milestone (ADR 0016): Betty's Agent opens a pull request; Anna's Agent,
//! maintainer of the repository under Anna's own rules, reviews and merges it
//! without a card. Every rule is checked on the exact head.
use super::*;
use crate::merge_policy::tests::{bash, file, ledger_with, BASE, HEAD};
use crate::merge_policy::work::{OwnerRules, Role};
use crate::merge_policy::{gate, ChangedFile, Check, PrFacts};
use std::path::Path;

const A: &str = "csheargm/hagency-demo-sandbox";
const B: &str = "csheargm/other";
const OWNER: &str = "@owner:example.org";
const ROOM: &str = "!r:x";

pub(crate) struct Src {
    pub repo: &'static str,
    pub files: Vec<ChangedFile>,
    pub checks: Vec<Check>,
    pub policy: Option<Option<String>>,
    pub reviews: Option<Vec<(String, String, String)>>,
    pub author: &'static str,
    pub viewer: Option<&'static str>,
}
impl Src {
    pub fn new(files: Vec<ChangedFile>) -> Self {
        Self { repo: A, files, checks: vec![Check::Passed], policy: Some(None), reviews: Some(vec![]), author: "betty", viewer: Some("anna") }
    }
}
impl PrSource for Src {
    async fn pull_request(&self, _: &GhPr) -> Option<PrView> {
        let facts = PrFacts { open: true, files: self.files.clone(), checks: self.checks.clone() };
        Some(PrView { url: format!("https://github.com/{}/pull/7", self.repo), base_sha: BASE.into(), head_sha: HEAD.into(), author: self.author.into(), facts })
    }
    async fn policy(&self, _: &GhPr, _: &str, _: &str) -> Option<String> {
        self.policy.clone().flatten()
    }
    async fn policy_file(&self, _: &GhPr, _: &str, base: &str) -> Option<Option<String>> {
        assert_eq!(base, BASE, "the policy file comes from the base commit");
        self.policy.clone()
    }
    async fn viewer_login(&self, _: &GhPr) -> Option<String> {
        self.viewer.map(str::to_owned)
    }
    async fn reviews(&self, _: &GhPr, _: &str) -> Option<Vec<(String, String, String)>> {
        self.reviews.clone()
    }
}

fn rules(repo: &str, role: Role) -> OwnerRules {
    OwnerRules { repo: repo.into(), role, room_id: ROOM.into(), trusted_authors: vec!["betty".into()], any_path: true, ..Default::default() }
}
fn anna(dir: &Path, work: Vec<OwnerRules>) -> Ledger {
    let mut ledger = ledger_with(dir, &["anna"]);
    ledger.set_auto_merge(OWNER, "anna", true).unwrap();
    ledger.set_work(OWNER, "anna", work).unwrap();
    ledger
}
fn line(verb: &str, repo: &str) -> String {
    match verb {
        "merge" => format!("gh pr merge 7 --squash --match-head-commit {HEAD} --repo {repo}"),
        _ => format!("gh pr review 7 --repo {repo} --approve -b \"Hagency verdict: APPROVE {HEAD} - ok\""),
    }
}
/// Anna's Agent reviews (settled as a success), then merges.
async fn review_then_merge(ledger: &mut Ledger, dir: &Path, src: &Src) -> (Option<bool>, Option<bool>) {
    let review = gate::auto_answer(ledger, OWNER, &bash(&line("review", src.repo), dir, "anna"), "rv", 11, src).await;
    ledger.settle_agent_review("rv", review == Some(true)).unwrap();
    let merge = gate::auto_answer(ledger, OWNER, &bash(&line("merge", src.repo), dir, "anna"), "mg", 11, src).await;
    (review, merge)
}

#[tokio::test]
async fn the_maintainer_reviews_and_merges_a_trusted_authors_code_change() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let src = Src::new(vec![file("src/main.rs", 120)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &src).await, (Some(true), Some(true)), "zero cards");
}

#[tokio::test]
async fn untrusted_authors_red_or_pending_ci_and_protected_paths_need_the_owner() {
    let cases: Vec<(&str, Src)> = vec![
        ("untrusted author", Src { author: "mallory", ..Src::new(vec![file("src/a.rs", 1)]) }),
        ("unknown viewer and author", Src { author: "", ..Src::new(vec![file("src/a.rs", 1)]) }),
        ("red CI", Src { checks: vec![Check::Failed], ..Src::new(vec![file("src/a.rs", 1)]) }),
        ("pending CI", Src { checks: vec![Check::Pending], ..Src::new(vec![file("src/a.rs", 1)]) }),
        ("workflow", Src::new(vec![file(".github/workflows/ci.yml", 1)])),
        ("policy file", Src::new(vec![file(".hagency/merge-policy.yml", 1)])),
        ("CODEOWNERS", Src::new(vec![file("CODEOWNERS", 1)])),
        ("file tightens", Src { policy: Some(Some("low_risk_paths: [\"**/*.md\"]\n".into())), ..Src::new(vec![file("src/a.rs", 1)]) }),
        ("unreadable file", Src { policy: None, ..Src::new(vec![file("src/a.rs", 1)]) }),
    ];
    for (name, src) in cases {
        let temp = tempfile::tempdir().unwrap();
        let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
        assert_eq!(review_then_merge(&mut ledger, temp.path(), &src).await.1, None, "{name}");
    }
    let temp = tempfile::tempdir().unwrap();
    let human = OwnerRules { always_human: vec!["secrets/**".into()], ..rules(A, Role::Maintainer) };
    let mut ledger = anna(temp.path(), vec![human]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &Src::new(vec![file("secrets/k", 1)])).await.1, None, "owner's always-human path");
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let no_ci = Src { checks: vec![], ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &no_ci).await.1, Some(true), "a repository without CI");
}

#[tokio::test]
async fn the_maintainers_own_pull_request_still_needs_another_agent() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let own = Src { author: "anna", ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &own).await.1, None, "its owner wrote it: self-approval does not count");
}

#[tokio::test]
async fn a_trusted_reviewers_github_approval_counts_only_on_the_exact_head() {
    let temp = tempfile::tempdir().unwrap();
    let trusting = OwnerRules { trusted_reviewers: vec!["carl".into()], ..rules(A, Role::Maintainer) };
    let mut ledger = anna(temp.path(), vec![trusting]);
    let proposal = bash(&line("merge", A), temp.path(), "anna");
    macro_rules! merge { ($src:expr) => { gate::auto_answer(&mut ledger, OWNER, &proposal, "mg", 11, $src) }; }
    let mut src = Src::new(vec![file("src/a.rs", 1)]);
    src.reviews = Some(vec![("carl".into(), "APPROVED".into(), BASE.into())]);
    assert_eq!(merge!(&src).await, None, "another head");
    src.reviews = Some(vec![("mallory".into(), "APPROVED".into(), HEAD.into())]);
    assert_eq!(merge!(&src).await, None, "an untrusted reviewer");
    src.reviews = None;
    assert_eq!(merge!(&src).await, None, "reviews unreadable");
    src.reviews = Some(vec![("Carl".into(), "APPROVED".into(), HEAD.into())]);
    assert_eq!(merge!(&src).await, Some(true));
}

#[tokio::test]
async fn two_repositories_with_different_roles_are_judged_separately() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer), rules(B, Role::Reviewer)]);
    let in_b = Src { repo: B, ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &in_b).await, (Some(true), None), "a reviewer's review skips the card, its merge never does");
    let in_a = Src::new(vec![file("src/a.rs", 1)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &in_a).await, (Some(true), Some(true)));
    assert!(ledger.set_work(OWNER, "anna", vec![rules(A, Role::Maintainer), rules("CSheargm/Hagency-Demo-Sandbox", Role::Reviewer)]).is_err(), "a repository once");
    assert_eq!(ledger.all_owner_rules("anna").unwrap().len(), 2, "a refused list changes nothing");
    let developer = Src { repo: B, ..Src::new(vec![file("README.md", 1)]) };
    ledger.set_work(OWNER, "anna", vec![rules(B, Role::Developer)]).unwrap();
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &developer).await.1, None, "a developer never merges by rule");
    let ready = format!("gh pr ready 7 --repo {B}");
    let code = Src { repo: B, ..Src::new(vec![file("src/a.rs", 300)]) };
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&ready, temp.path(), "anna"), "rd", 11, &code).await, Some(true), "a developer readies its pull request without a card");
}

#[test]
fn rules_are_validated_and_claims_are_limited_per_agent_across_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["anna"]);
    for bad in [OwnerRules { repo: "nope".into(), ..rules(A, Role::Maintainer) }, OwnerRules { trusted_reviewers: vec!["*".into()], ..rules(A, Role::Maintainer) }, OwnerRules { trusted_authors: vec!["a b".into()], ..rules(A, Role::Maintainer) }] {
        assert!(ledger.set_work(OWNER, "anna", vec![bad]).is_err());
    }
    assert!(ledger.set_work("@intruder:example.org", "anna", vec![rules(A, Role::Developer)]).is_err(), "only the owner sets the work list");
    let left = ledger.claims_left_today("anna", 1000).unwrap();
    assert!(ledger.record_work_claim("anna", A, 1, 1000).unwrap());
    assert!(!ledger.record_work_claim("anna", A, 1, 1000).unwrap(), "one claim per issue");
    assert!(ledger.record_work_claim("anna", B, 1, 1000).unwrap(), "the same number in another repository");
    assert_eq!(ledger.claims_left_today("anna", 1000).unwrap(), left - 2);
    assert_eq!(ledger.claims_left_today("anna", 1000 + 86_401).unwrap(), left, "a day later");
}

#[test]
fn each_loop_step_is_taken_once_per_head() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["anna"]);
    let url = "https://github.com/csheargm/hagency-demo-sandbox/pull/7";
    assert!(ledger.claim_loop_step("anna", "merge", url, HEAD).unwrap());
    assert!(!ledger.claim_loop_step("anna", "merge", url, HEAD).unwrap());
    assert!(ledger.loop_step_taken("anna", "merge", url, HEAD).unwrap());
    assert!(ledger.claim_loop_step("anna", "merge", url, BASE).unwrap(), "a new head is a new step");
}

#[tokio::test]
async fn readiness_reports_the_decision_without_taking_the_merge() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let src = Src::new(vec![file("src/a.rs", 3)]);
    let command = GhPr { dir: temp.path().into(), number: 7, repo: Some(A.into()), head: None, kind: gate::Kind::Merge, body: String::new() };
    assert!(matches!(merge_readiness(&mut ledger, &command, "anna", &src).await.unwrap().1, Decision::NeedsHuman(_)), "no review yet");
    let review = gate::auto_answer(&mut ledger, OWNER, &bash(&line("review", A), temp.path(), "anna"), "rv", 11, &src).await;
    ledger.settle_agent_review("rv", review == Some(true)).unwrap();
    let (view, decision) = merge_readiness(&mut ledger, &command, "anna", &src).await.unwrap();
    assert_eq!((view.head_sha.as_str(), decision), (HEAD, Decision::Merge));
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&line("merge", A), temp.path(), "anna"), "mg", 11, &src).await, Some(true), "still available");
}

#[tokio::test]
async fn gpt_review_fixes_hold() {
    // An approval must name the head it reviewed; a stale or missing SHA asks.
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let src = Src::new(vec![file("src/a.rs", 1)]);
    for body in ["Hagency verdict: APPROVE - ok".to_owned(), format!("Hagency verdict: APPROVE {BASE}"), format!("Hagency verdict: APPROVE {HEAD}0")] {
        let line = format!("gh pr review 7 --repo {A} --approve -b \"{body}\"");
        assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&line, temp.path(), "anna"), "rv", 11, &src).await, None, "{body}");
    }
    // An untrusted author's pull request gets no card-free approval.
    let mallory = Src { author: "mallory", ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&line("review", A), temp.path(), "anna"), "rv", 11, &mallory).await, None);
    // Request changes withdraws an earlier approval.
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&line("review", A), temp.path(), "anna"), "rv", 11, &src).await, Some(true));
    ledger.settle_agent_review("rv", true).unwrap();
    let changes = format!("gh pr review 7 --repo {A} --request-changes -b \"Hagency verdict: CHANGES - no\"");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&changes, temp.path(), "anna"), "rc", 11, &src).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&line("merge", A), temp.path(), "anna"), "mg", 11, &src).await, None, "the approval was withdrawn");
}

#[tokio::test]
async fn the_file_keeps_its_own_approval_rule_and_one_review_counts_once() {
    // A repository file requiring another Agent still does under owner rules.
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let filed = Src { policy: Some(Some("low_risk_paths: [\"**\"]\n".into())), ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &filed).await.1, None, "the maintainer alone does not satisfy the file");
    // Two approvals required: the device's own login in reviewers= does not count twice.
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![OwnerRules { trusted_reviewers: vec!["anna".into()], ..rules(A, Role::Maintainer) }]);
    let two = Src { policy: Some(Some("tier: trusted\nrequired_agent_approvals: 2\n".into())), reviews: Some(vec![("anna".into(), "APPROVED".into(), HEAD.into())]), ..Src::new(vec![file("src/a.rs", 1)]) };
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &two).await.1, None);
}

#[test]
fn missing_ci_evidence_is_pending_and_the_daily_claim_limit_is_enforced_on_reserve() {
    let view = serde_json::json!({"url": "https://github.com/a/b/pull/7", "state": "OPEN", "baseRefOid": BASE, "headRefOid": HEAD});
    assert_eq!(crate::merge_policy::github::view_from_json(&view, &serde_json::json!([])).unwrap().facts.checks, [Check::Pending]);
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["anna"]);
    let limit = crate::merge_policy::work::DEFAULT_ISSUES_PER_DAY as i64;
    for n in 0..limit {
        assert!(ledger.record_work_claim("anna", A, n, 10).unwrap());
    }
    assert!(!ledger.record_work_claim("anna", B, 99, 10).unwrap(), "the limit holds across repositories");
}
