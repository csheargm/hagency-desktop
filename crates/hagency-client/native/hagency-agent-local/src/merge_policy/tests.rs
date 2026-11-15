use super::gate::{self, GhPr, PrSource, APPROVE_MARKER};
use super::*;
use crate::{Layer, Ledger, Scope, ToolPolicy, ToolProposal};
use serde_json::{json, Value};
use std::path::Path;

const POLICY: &str = "# owner's policy\nlow_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: true\n";
const URL: &str = "https://github.com/csheargm/hagency-demo-sandbox/pull/7";

fn policy() -> MergePolicy {
    parse_policy(POLICY).unwrap()
}
fn file(path: &str, lines: u64) -> ChangedFile {
    ChangedFile { path: path.into(), additions: lines, deletions: 0 }
}
fn facts(files: Vec<ChangedFile>, checks: Vec<Check>) -> PrFacts {
    PrFacts { open: true, files, checks }
}
fn reviewers(names: &[&str]) -> Vec<String> {
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
    assert_eq!(evaluate(&policy(), &facts(vec![file("README.md", 4)], vec![]), &reviewers(&["reviewer"]), "fixer"), Decision::Merge, "no checks configured");
}

#[test]
fn everything_outside_policy_needs_a_person() {
    let ok = |pr: PrFacts, approvers: &[&str]| evaluate(&policy(), &pr, &reviewers(approvers), "fixer");
    let human = |d: Decision| matches!(d, Decision::NeedsHuman(_));
    assert!(human(ok(facts(vec![file("README.md", 4)], vec![]), &["fixer"])), "the author never approves its own pull request");
    assert!(human(ok(facts(vec![file("README.md", 4)], vec![]), &[])));
    assert!(human(ok(facts(vec![file("src/main.rs", 1)], vec![]), &["reviewer"])), "code is not low risk");
    assert!(human(ok(facts(vec![file("README.md", 51)], vec![]), &["reviewer"])), "over the size limit");
    assert!(human(ok(facts(vec![file("README.md", 1)], vec![Check::Failed]), &["reviewer"])));
    assert!(human(ok(facts(vec![file("README.md", 1)], vec![Check::Pending]), &["reviewer"])));
    assert!(human(ok(facts(vec![], vec![]), &["reviewer"])), "an empty diff is not a pass");
    assert!(human(ok(facts(vec![file(".hagency/merge-policy.md", 1)], vec![]), &["reviewer"])), "the policy itself");
    assert!(human(ok(facts(vec![file(".github/workflows/x.md", 1)], vec![]), &["reviewer"])));
    let mut closed = facts(vec![file("README.md", 1)], vec![]);
    closed.open = false;
    assert!(human(ok(closed, &["reviewer"])));
    let zero = parse_policy("low_risk_paths: [\"**\"]\nrequired_agent_approvals: 0\n").unwrap();
    assert!(human(evaluate(&zero, &facts(vec![file("a.md", 1)], vec![]), &[], "fixer")), "one other Agent is always required");
}

fn bash(line: &str, dir: &Path, agent: &str) -> ToolProposal {
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
    let merge = parse("cd repo && gh pr merge 7 --squash --delete-branch").unwrap();
    assert_eq!((merge.number, merge.kind, merge.dir.ends_with("repo")), (7, gate::Kind::Merge, true));
    assert_eq!(parse("gh pr ready 7").unwrap().kind, gate::Kind::Ready);
    assert_eq!(parse("gh pr merge 7 --squash -R csheargm/hagency-demo-sandbox").unwrap().repo.as_deref(), Some("csheargm/hagency-demo-sandbox"));
    assert_eq!(parse("gh pr review 7 --approve").unwrap().kind, gate::Kind::Review);
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
    let value = json!({"url": URL, "state": "OPEN", "baseRefName": "main",
        "files": [{"path": "README.md", "additions": 3, "deletions": 1}],
        "statusCheckRollup": [{"conclusion": "SUCCESS"}, {"conclusion": "", "status": "IN_PROGRESS"}, {"state": "FAILURE"}]});
    let (url, base, pr) = gate::facts_from_json(&value).unwrap();
    assert_eq!((url.as_str(), base.as_str()), (URL, "main"));
    assert_eq!(pr.checks, [Check::Passed, Check::Pending, Check::Failed]);
    assert_eq!(gate::repo_of_url(URL, 7), Some("csheargm/hagency-demo-sandbox"));
    assert_eq!(gate::repo_of_url(URL, 8), None, "the URL must be the pull request asked about");
    assert!(gate::facts_from_json(&json!({"url": URL, "state": "OPEN", "baseRefName": "main&x=1", "files": []})).is_none());
}

struct Fake {
    files: Vec<ChangedFile>,
}
impl PrSource for Fake {
    async fn pull_request(&self, _: &GhPr) -> Option<(String, String, PrFacts)> {
        Some((URL.into(), "main".into(), facts(self.files.clone(), vec![Check::Passed])))
    }
    async fn policy(&self, _: &GhPr, repo: &str, base: &str) -> Option<String> {
        assert_eq!((repo, base), ("csheargm/hagency-demo-sandbox", "main"), "the policy comes from the base branch");
        Some(POLICY.into())
    }
}

fn ledger_with(dir: &Path, agents: &[&str]) -> Ledger {
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
    let docs = Fake { files: vec![file("README.md", 5)] };
    let merge = bash("gh pr merge 7 --squash", temp.path(), "fixer");
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}\""), temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, None, "off by default: the card asks");
    ledger.set_auto_merge(owner, "fixer", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &bash("gh pr ready 7", temp.path(), "fixer"), 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, None, "no other Agent approved yet");
    let self_review = bash("gh pr review 7 --approve", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &self_review, 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, None, "its own approval does not count");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, 11, &docs).await, None, "the reviewer's switch is off");
    ledger.set_auto_merge(owner, "reviewer", true).unwrap();
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, Some(true));
    let mut again = merge.clone();
    again.arguments = json!({"command": "gh pr merge 7 --squash -d"});
    assert_eq!(gate::auto_answer(&mut ledger, owner, &again, 11, &docs).await, None, "one automatic merge per pull request");
}

#[tokio::test]
async fn a_code_change_still_needs_the_owner_after_an_agent_approval() {
    let temp = tempfile::tempdir().unwrap();
    let owner = "@owner:example.org";
    let mut ledger = ledger_with(temp.path(), &["fixer", "reviewer"]);
    let code = Fake { files: vec![file("src/main.rs", 2)] };
    for agent in ["fixer", "reviewer"] {
        ledger.set_auto_merge(owner, agent, true).unwrap();
    }
    let review: Value = json!(format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}\""));
    let mut approve = bash("", temp.path(), "reviewer");
    approve.arguments = json!({"command": review});
    assert_eq!(gate::auto_answer(&mut ledger, owner, &approve, 11, &code).await, Some(true));
    let merge = bash("gh pr merge 7 --squash", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &code).await, None);
    assert!(ledger.authorize_tool(&merge, 11).is_err(), "the owner's card is still required");
}

#[test]
fn the_agent_instructions_use_the_marker_the_gate_counts() {
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains(APPROVE_MARKER));
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("gh pr merge <N> --squash --repo <owner/repo>` as its own command"));
    // The Nov 15 live test: a chained or repo-less merge falls back to a card.
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("never chained"));
    // The issue-watch hand-off's merge-policy sentence and the base-branch check lead here.
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("agent merge policy"));
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.contains("git show origin/<base>:.hagency/merge-policy.yml"));
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
    let docs = Fake { files: vec![file("README.md", 5)] };
    let merge = bash("gh pr merge 7 --squash", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &bash("gh pr ready 7", temp.path(), "fixer"), 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, None, "no review recorded yet");
    let own = bash("gh pr review 7 --approve", temp.path(), "fixer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &own, 11, &docs).await, Some(true), "recorded, but as the author's");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, None, "the author's own verdict does not count");
    let review = bash(&format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}\""), temp.path(), "reviewer");
    assert_eq!(gate::auto_answer(&mut ledger, owner, &review, 11, &docs).await, Some(true));
    assert_eq!(gate::auto_answer(&mut ledger, owner, &merge, 11, &docs).await, Some(true));
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
    let wrapped = parse(json!(["/bin/zsh", "-lc", "cd repo && gh pr merge 7 --squash"])).unwrap();
    assert!(wrapped.dir.ends_with("repo"));
    assert_eq!(parse(json!(["bash", "-c", format!("gh pr review 7 --comment -b \"{APPROVE_MARKER}: ok\"")])).unwrap().kind, gate::Kind::Review);
}

#[test]
fn codex_chained_redirected_or_expanded_commands_still_get_a_card() {
    let dir = tempfile::tempdir().unwrap();
    let parse = |command: Value| gate::parse_command(&codex(command, dir.path()));
    for line in ["gh pr merge 7 --squash; rm -rf ~", "gh pr merge 7 --squash && gh pr merge 8 --squash", "gh pr merge 7 --squash > out", "gh pr merge $(echo 7) --squash", "cd .. && gh pr merge 7 --squash", "gh pr merge 7 --merge"] {
        assert!(parse(json!(line)).is_none(), "{line}");
        assert!(parse(json!(["bash", "-lc", line])).is_none(), "{line}");
    }
    assert!(parse(json!(["gh", "pr", "merge", "7", "--squash", ";"])).is_none(), "argv words hold no metacharacter");
    assert!(parse(json!(["gh", "pr", "merge", "7", "--squash", "--admin"])).is_none());
    assert!(parse(json!(["gh", "pr", "review", "7", "--comment", "-b", "Hagency verdict: CHANGES - no"])).is_none());
    assert!(parse(json!(["python3", "-c", "gh pr merge 7 --squash"])).is_none(), "only a shell's -c line is read");
    assert!(parse(json!(["gh", "pr", 7])).is_none());
    let mut expansion = codex(json!("gh pr merge 7 --squash"), dir.path());
    expansion.risk = "permission_expansion".into();
    assert!(gate::parse_command(&expansion).is_none());
    let mut network = codex(json!("gh pr merge 7 --squash"), dir.path());
    network.tool = "codex.network".into();
    assert!(gate::parse_command(&network).is_none());
}

#[test]
fn codex_room_folder_instructions_carry_the_shared_merge_note() {
    assert!(hagency_runtime::claude::AGENT_TOOLS_NOTE.ends_with(hagency_runtime::AGENT_MERGE_NOTE));
    assert!(hagency_runtime::AGENT_MERGE_NOTE.contains("gh pr merge <N> --squash --repo <owner/repo>` as its own command"));
}
