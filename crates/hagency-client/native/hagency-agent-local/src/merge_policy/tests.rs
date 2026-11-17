use super::gate::{self, GhPr, APPROVE_MARKER};
use super::github::{self, PrSource, PrView};
use super::*;
use crate::{Layer, Ledger, Scope, ToolPolicy, ToolProposal};
use serde_json::{json, Value};
use std::path::Path;

const POLICY: &str = "# owner's policy\nlow_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: true\n";
pub(super) const URL: &str = "https://github.com/csheargm/hagency-demo-sandbox/pull/7";
pub(super) const HEAD: &str = "1111111111111111111111111111111111111111";
pub(super) const BASE: &str = "2222222222222222222222222222222222222222";

mod hardening;
mod ready_review;

pub(super) fn policy() -> MergePolicy {
    parse_policy(POLICY).unwrap()
}
pub(super) fn file(path: &str, lines: u64) -> ChangedFile {
    ChangedFile { path: path.into(), previous_path: None, additions: lines, deletions: 0, opaque: false }
}
pub(super) fn facts(files: Vec<ChangedFile>, checks: Vec<Check>) -> PrFacts {
    PrFacts { open: true, files, checks }
}
pub(super) fn reviewers(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn policy_file_reads_inline_and_block_lists_and_refuses_unknown_keys() {
    let p = policy();
    assert_eq!(p.low_risk_paths, ["**/*.md", "docs/**"]);
    assert_eq!((p.max_changed_lines, p.required_agent_approvals, p.require_ci), (50, 1, true));
    let block = parse_policy("low_risk_paths:\n  - \"tests/**\"\n  - '*.txt'\nmax_changed_lines: 10\n").unwrap();
    assert_eq!(block.low_risk_paths, ["tests/**", "*.txt"]);
    assert!(parse_policy("low_risk_path: [\"**\"]\n").is_none(), "a typo never drops a rule");
    assert!(parse_policy("max_changed_lines: lots\n").is_none());
}

#[test]
fn globs_match_one_part_or_many() {
    assert!(glob_match("**/*.md", "README.md"));
    assert!(glob_match("**/*.md", "docs/a/b.md"));
    assert!(glob_match("docs/**", "docs/x/y.rs"));
    assert!(glob_match("*.txt", "a.txt"));
    assert!(!glob_match("*.txt", "dir/a.txt"));
    assert!(!glob_match("**/*.md", "src/main.rs"));
    assert!(!glob_match("docs/**", "docsx/a"));
}

#[test]
fn a_small_docs_change_approved_by_another_agent_merges() {
    let pr = facts(vec![file("README.md", 4), file("docs/guide.md", 6)], vec![Check::Passed]);
    assert_eq!(evaluate(&policy(), &pr, &reviewers(&["reviewer"]), "fixer"), Decision::Merge);
    assert_eq!(evaluate(&policy(), &facts(vec![file("README.md", 4)], vec![Check::Passed]), &reviewers(&["reviewer"]), "fixer"), Decision::Merge, "CI passed");
}

#[test]
fn everything_outside_policy_needs_a_person() {
    let ok = |pr: PrFacts, approvers: &[&str]| evaluate(&policy(), &pr, &reviewers(approvers), "fixer");
    let human = |d: Decision| matches!(d, Decision::NeedsHuman(_));
    assert!(human(ok(facts(vec![file("README.md", 4)], vec![Check::Passed]), &["fixer"])), "the author never approves its own pull request");
    assert!(human(ok(facts(vec![file("README.md", 4)], vec![Check::Passed]), &[])));
    assert!(human(ok(facts(vec![file("src/main.rs", 1)], vec![Check::Passed]), &["reviewer"])), "code is not low risk");
    assert!(human(ok(facts(vec![file("README.md", 51)], vec![Check::Passed]), &["reviewer"])), "over the size limit");
    assert!(human(ok(facts(vec![file("README.md", 1)], vec![Check::Failed]), &["reviewer"])));
    assert!(human(ok(facts(vec![file("README.md", 1)], vec![Check::Pending]), &["reviewer"])));
    assert!(human(ok(facts(vec![], vec![Check::Passed]), &["reviewer"])), "an empty diff is not a pass");
    assert!(human(ok(facts(vec![file(".hagency/merge-policy.md", 1)], vec![Check::Passed]), &["reviewer"])), "the policy itself");
    assert!(human(ok(facts(vec![file(".github/workflows/x.md", 1)], vec![Check::Passed]), &["reviewer"])));
    let mut closed = facts(vec![file("README.md", 1)], vec![Check::Passed]);
    closed.open = false;
    assert!(human(ok(closed, &["reviewer"])));
    let zero = parse_policy("low_risk_paths: [\"**\"]\nrequired_agent_approvals: 0\n").unwrap();
    assert!(human(evaluate(&zero, &facts(vec![file("a.md", 1)], vec![Check::Passed]), &[], "fixer")), "one other Agent is always required");
}

pub(super) fn bash(line: &str, dir: &Path, agent: &str) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: agent.into(), binding: format!("b-{agent}"), room: "!r:x".into(), requester: "@owner:example.org".into(), thread: "t".into() },
        dispatch: format!("d-{agent}"), tool: "claude.Bash".into(), arguments: json!({"command": line}),
        canonical_directory: dir.to_string_lossy().into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
    }
}

#[test]
fn only_exact_merge_and_approving_review_commands_reach_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("repo")).unwrap();
    let parse = |line: &str| gate::parse_command(&bash(line, dir.path(), "a"));
    let merge = parse("cd repo && gh pr merge 7 --squash -R csheargm/hagency-demo-sandbox").unwrap();
    assert_eq!((merge.number, merge.kind, merge.dir.ends_with("repo")), (7, gate::Kind::Merge, true));
    assert_eq!(parse("gh pr ready 7 -R csheargm/hagency-demo-sandbox").unwrap().kind, gate::Kind::Ready);
    assert_eq!(parse("gh pr merge 7 --squash -R csheargm/hagency-demo-sandbox").unwrap().repo.as_deref(), Some("csheargm/hagency-demo-sandbox"));
    assert_eq!(parse(&format!("gh pr review 7 --approve -b {HEAD}")).unwrap().kind, gate::Kind::Review);
    assert!(parse(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}: docs only\"")).is_some());
    for line in [
        "gh pr merge 7", "gh pr merge 7 --merge", "gh pr merge 7 --squash --admin", "gh pr merge 7 --squash --auto",
        "gh pr merge 7 --squash; rm -rf .", "gh pr merge 7 --squash | cat", "cd /tmp && gh pr merge 7 --squash",
        "cd ../x && gh pr merge 7 --squash", "gh pr merge $N --squash", "gh pr merge 7 --squash -R ghe.example.com/o/r",
        "gh pr merge abc --squash", "gh pr review 7 --comment -b 'looks fine'", "gh pr review 7 --approve --request-changes",
        "gh pr review 7 --approve --comment", "gh pr create --fill", "gh pr ready 7 --undo", "gh pr ready 7 --squash", "gh pr review 7 --squash --approve",
    ] {
        assert!(parse(line).is_none(), "{line}");
    }
}

#[test]
fn github_pr_json_becomes_facts_and_checks() {
    let view = json!({"url": URL, "state": "OPEN", "baseRefOid": BASE, "headRefOid": HEAD,
        "statusCheckRollup": [{"conclusion": "SUCCESS"}, {"conclusion": "", "status": "IN_PROGRESS"}, {"state": "FAILURE"}]});
    let files = json!([{"filename": "README.md", "status": "modified", "additions": 3, "deletions": 1, "changes": 4, "has_patch": true}]);
    let pr = github::view_from_json(&view, &files).unwrap();
    assert_eq!((pr.url.as_str(), pr.base_sha.as_str(), pr.head_sha.as_str()), (URL, BASE, HEAD));
    assert_eq!(pr.facts.files, [ChangedFile { deletions: 1, ..file("README.md", 3) }]);
    assert_eq!(pr.facts.checks, [Check::Passed, Check::Pending, Check::Failed]);
    assert_eq!(github::repo_of_url(URL, 7), Some("csheargm/hagency-demo-sandbox"));
    assert_eq!(github::repo_of_url(URL, 8), None, "the URL must be the pull request asked about");
    let bad = json!({"url": URL, "state": "OPEN", "baseRefOid": "main", "headRefOid": HEAD});
    assert!(github::view_from_json(&bad, &json!([])).is_none(), "the policy ref is a commit, never a branch name");
}

pub(super) struct Fake {
    files: Vec<ChangedFile>,
    pub(super) head: std::cell::RefCell<String>,
    pub(super) checks: Vec<Check>,
}
impl Fake {
    pub(super) fn new(files: Vec<ChangedFile>) -> Self {
        Self { files, head: std::cell::RefCell::new(HEAD.into()), checks: vec![Check::Passed] }
    }
}
impl PrSource for Fake {
    async fn pull_request(&self, _: &GhPr) -> Option<PrView> {
        let facts = facts(self.files.clone(), self.checks.clone());
        Some(PrView { url: URL.into(), base_sha: BASE.into(), head_sha: self.head.borrow().clone(), author: String::new(), facts })
    }
    async fn policy(&self, _: &GhPr, repo: &str, base: &str) -> Option<String> {
        assert_eq!((repo, base), ("csheargm/hagency-demo-sandbox", BASE), "the policy comes from the base commit");
        Some(POLICY.into())
    }
}

pub(super) fn merge_line() -> String {
    format!("gh pr merge 7 --squash --match-head-commit {HEAD} -R csheargm/hagency-demo-sandbox")
}

pub(super) fn ledger_with(dir: &Path, agents: &[&str]) -> Ledger {
    let owner = "@owner:example.org";
    let mut ledger = Ledger::open(dir.join("local.db"), owner).unwrap();
    let ask = crate::Policy { budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime }, requests: crate::RequestPolicy::Allow, high_risk: ToolPolicy::AskOwner };
    for agent in agents {
        let scope = bash("x", dir, agent).scope;
        ledger.register_binding(owner, &scope).unwrap();
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
            ledger.set_policy(owner, &scope, layer, 0, &ask).unwrap();
        }
        ledger.reserve(&scope, &format!("call-{agent}"), &format!("d-{agent}"), 10, 10).unwrap();
    }
    ledger
}

/// The scene: the fixer asks to merge (a card: no approval yet), a reviewer
/// Agent approves without a card, the fixer's merge is then allowed once.
#[tokio::test]
async fn reviewer_approval_lets_the_fixer_merge_once_and_off_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    let docs = Fake::new(vec![file("README.md", 5)]);
    let merge = bash(&merge_line(), temp.path(), "fixer");
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""), temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, None, "off by default: the card asks");
    ledger.set_auto_merge(owner, "fixer", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &bash("gh pr ready 7 -R csheargm/hagency-demo-sandbox", temp.path(), "fixer"), "call", 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, None, "no other Agent approved yet");
    let self_review = bash(&format!("gh pr review 7 --approve -b {HEAD}"), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &self_review, "call", 11, &docs).await, Some(true));
    ledger.settle_agent_review("call", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, None, "its own approval does not count");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, "call", 11, &docs).await, None, "the reviewer's switch is off");
    ledger.set_auto_merge(owner, "reviewer", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, "call", 11, &docs).await, Some(true));
    ledger.settle_agent_review("call", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, Some(true));
    let mut again = merge.clone();
    again.arguments = json!({"command": format!("{} --repo csheargm/hagency-demo-sandbox", merge_line().replace(" -R csheargm/hagency-demo-sandbox", ""))});
    assert_eq!(gate::auto_answer(&mut ledger, owner, &again, "call", 11, &docs).await, None, "one automatic merge per pull request");
}

#[tokio::test]
async fn a_code_change_still_needs_the_owner_after_an_agent_approval() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    let code = Fake::new(vec![file("src/main.rs", 2)]);
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(owner, agent, true).unwrap();
    }
    let review: Value = json!(format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""));
    let mut approve = bash("", temp.path(), "reviewer");
    approve.arguments = json!({"command": review});
    assert_eq!(gate::auto_answer(&mut ledger, owner, &approve, "call", 11, &code).await, None, "a code review is not low risk: the card asks");
    let merge = bash(&merge_line(), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &code).await, None);
    assert!(ledger.authorize_tool(&merge, 11).is_err(), "the owner's card is still required");
}

#[test]
fn the_agent_instructions_use_the_marker_the_gate_counts() {
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains(APPROVE_MARKER));
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("gh pr merge <N> --squash --match-head-commit <sha> --repo <owner/repo>` as its own command"));
    // The Nov 15 live test: a chained or repo-less merge falls back to a card.
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("never chained"));
    // The issue-watch hand-off's merge-policy sentence and the base-branch check lead here.
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("agent merge policy"));
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("do not read or fetch the policy yourself"));
}

/// The hand-off now tells the fixer to continue (ready, ask a reviewer), but
/// the merge gate is unchanged: with both switches on, the fixer's merge still
/// needs a recorded approval from an Agent other than the author.
#[tokio::test]
async fn the_handoff_flow_still_needs_a_recorded_non_author_approval() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(owner, agent, true).unwrap();
    }
    let docs = Fake::new(vec![file("README.md", 5)]);
    let merge = bash(&merge_line(), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &bash("gh pr ready 7 -R csheargm/hagency-demo-sandbox", temp.path(), "fixer"), "call", 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, None, "no review recorded yet");
    let own = bash(&format!("gh pr review 7 --approve -b {HEAD}"), temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &own, "call", 11, &docs).await, Some(true), "recorded, but as the author's");
    ledger.settle_agent_review("call", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, None, "the author's own verdict does not count");
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER} {HEAD}\""), temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, "call", 11, &docs).await, Some(true));
    ledger.settle_agent_review("call", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, "call", 11, &docs).await, Some(true));
}

fn codex(command: Value, dir: &Path) -> ToolProposal {
    ToolProposal { tool: "codex.command".into(), arguments: json!({"command": command, "cwd": dir}), ..bash("", dir, "c") }
}

#[test]
fn codex_merge_ready_and_review_commands_are_read_like_claude_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("repo")).unwrap();
    let parse = |command: Value| gate::parse_command(&codex(command, dir.path()));
    let merge = parse(json!("gh pr merge 7 --squash --repo csheargm/hagency-demo-sandbox")).unwrap();
    assert_eq!((merge.number, merge.kind, merge.repo.as_deref()), (7, gate::Kind::Merge, Some("csheargm/hagency-demo-sandbox")));
    assert_eq!(parse(json!(["gh", "pr", "ready", "7", "--repo", "o/r"])).unwrap().kind, gate::Kind::Ready);
    let body = format!("{APPROVE_MARKER} - docs only");
    assert_eq!(parse(json!(["gh", "pr", "review", "7", "--comment", "-b", body])).unwrap().kind, gate::Kind::Review);
    let wrapped = parse(json!(["/bin/bash", "-c", "cd repo && gh pr merge 7 --squash -R o/r"])).unwrap();
    assert!(wrapped.dir.ends_with("repo"));
    assert_eq!(parse(json!(["/bin/sh", "-c", format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}: ok\"")])).unwrap().kind, gate::Kind::Review);
}

#[test]
fn codex_chained_redirected_or_expanded_commands_still_get_a_card() {
    let dir = tempfile::tempdir().unwrap();
    let parse = |command: Value| gate::parse_command(&codex(command, dir.path()));
    for line in ["gh pr merge 7 --squash; rm -rf ~", "gh pr merge 7 --squash && gh pr merge 8 --squash", "gh pr merge 7 --squash > out", "gh pr merge $(echo 7) --squash", "cd .. && gh pr merge 7 --squash", "gh pr merge 7 --merge"] {
        assert!(parse(json!(line)).is_none(), "{line}");
        assert!(parse(json!(["/bin/bash", "-c", line])).is_none(), "{line}");
    }
    assert!(parse(json!(["gh", "pr", "merge", "7", "--squash", ";"])).is_none(), "argv words hold no metacharacter");
    assert!(parse(json!(["gh", "pr", "merge", "7", "--squash", "--admin"])).is_none());
    assert!(parse(json!(["gh", "pr", "review", "7", "--comment", "-b", "Hagency verdict: CHANGES - no"])).is_none());
    assert!(parse(json!(["python3", "-c", "gh pr merge 7 --squash"])).is_none(), "only a shell's -c line is read");
    assert!(parse(json!(["gh", "pr", 7])).is_none());
    for program in ["bash", "sh", "/bin/zsh", "./bash", "repo/zsh", "/workspace/bash", "bin/sh", "/tmp/sh"] {
        assert!(parse(json!([program, "-c", "gh pr merge 7 --squash -R o/r"])).is_none(), "{program} is not a system shell");
    }
    assert!(parse(json!(["/bin/bash", "-lc", "gh pr merge 7 --squash -R o/r"])).is_none(), "a login shell sources startup files");
    for line in ["gh pr merge 7 --squash", "gh pr ready 7", "gh pr merge 7 --squash -d -R o/r", "gh pr merge 7 --squash --delete-branch -R o/r"] {
        assert!(parse(json!(line)).is_none(), "{line}: exact merge with an explicit repository only");
        assert!(gate::parse_command(&bash(line, dir.path(), "a")).is_none(), "{line}");
    }
    let mut expansion = codex(json!("gh pr merge 7 --squash -R o/r"), dir.path());
    expansion.risk = "permission_expansion".into();
    assert!(gate::parse_command(&expansion).is_none());
    let mut network = codex(json!("gh pr merge 7 --squash -R o/r"), dir.path());
    network.tool = "codex.network".into();
    assert!(gate::parse_command(&network).is_none());
}

#[test]
fn codex_room_folder_instructions_carry_the_shared_merge_note() {
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.ends_with(hagency_runtime::AGENT_MERGE_NOTE));
    assert!(hagency_runtime::AGENT_MERGE_NOTE.contains("gh pr merge <N> --squash --match-head-commit <sha> --repo <owner/repo>` as its own command"));
}

/// Live scene 4 (Nov 15): fixer cloned into /tmp, so every later step, reads
/// included, needed the owner. The instructions keep clones in the workspace.
#[test]
fn agents_are_told_to_clone_inside_the_workspace() {
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("never into /tmp"));
}
