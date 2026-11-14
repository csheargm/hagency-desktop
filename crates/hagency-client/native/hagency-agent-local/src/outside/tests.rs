use super::*;
use crate::Scope;
use super::MODE_OUTSIDE;
use super::MODE_ASK;
use serde_json::json;

fn proposal(tool: &str, arguments: Value, directory: &Path, risk: &str) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: "a".into(), binding: "b".into(), room: "!r:x".into(), requester: "@o:x".into(), thread: "t".into() },
        dispatch: "d".into(), tool: tool.into(), arguments, canonical_directory: directory.to_string_lossy().into(), risk: risk.into(),
        policy_revision: [1, 1, 1], expires: 1,
    }
}
fn allowed(tool: &str, arguments: Value, dir: &Path) -> bool {
    stays_in_workspace(&proposal(tool, arguments, dir, "high"))
}

#[test]
fn codex_work_in_its_sandbox_is_allowed_but_an_expansion_always_asks() {
    let dir = tempfile::tempdir().unwrap();
    assert!(allowed("codex.command", json!({"command":["bash","-lc","cargo test"]}), dir.path()));
    assert!(allowed("codex.file_change", json!({}), dir.path()));
    assert!(!stays_in_workspace(&proposal("codex.command", json!({}), dir.path(), "permission_expansion")));
}
#[test]
fn claude_file_tools_must_stay_in_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let inside = dir.path().join("src/new.rs");
    assert!(allowed("claude.Write", json!({"file_path":inside}), dir.path()));
    assert!(allowed("claude.Edit", json!({"file_path":"src/new.rs"}), dir.path()));
    assert!(allowed("claude.Grep", json!({"pattern":"x"}), dir.path()), "search defaults to the working directory");
    for outside in ["/etc/hosts", "../escape.txt", "src/../../escape.txt"] {
        assert!(!allowed("claude.Write", json!({"file_path":outside}), dir.path()), "{outside}");
        assert!(!allowed("claude.Read", json!({"file_path":outside}), dir.path()), "{outside}");
    }
    assert!(!allowed("claude.Write", json!({}), dir.path()), "a write names its file");
    assert!(!allowed("claude.WebFetch", json!({"url":"https://example.com"}), dir.path()));
}
#[cfg(unix)]
#[test]
fn a_symlink_cannot_lead_out_of_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("link")).unwrap();
    assert!(!allowed("claude.Write", json!({"file_path":"link/file.txt"}), dir.path()));
}
#[test]
fn only_plain_read_only_shell_commands_are_allowed() {
    let dir = tempfile::tempdir().unwrap();
    for line in ["ls -la", "git status", "git log --oneline", "rg todo src", "cat README.md"] {
        assert!(allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
    for line in ["gh issue create", "git push origin main", "curl https://x", "ls; rm -rf .", "cat /etc/passwd", "cat ../x", "echo hi > out", "ls $(id)", "git -c core.pager=x log", "rm -rf ."] {
        assert!(!allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
}

/// The Ledger honours the saved mode: an Ask-owner policy stops asking for
/// work inside the workspace, still asks outside it, and a mode never set asks as before.
#[test]
fn ledger_skips_the_question_only_in_outside_mode_and_only_inside_the_workspace() {
    use crate::{Layer, Ledger, ToolPolicy};
    let temp = tempfile::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let owner = "@owner:example.org";
    let mut ledger = Ledger::open(temp.path().join("local.db"), owner).unwrap();
    let mut scope = proposal("claude.Write", json!({}), &work, "high").scope;
    scope.requester = owner.into();
    ledger.register_binding(owner, &scope).unwrap();
    let ask = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk: ToolPolicy::AskOwner,
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(owner, &scope, layer, 0, &ask).unwrap();
    }
    ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
    let make = |tool: &str, args: Value| {
        let mut p = proposal(tool, args, &work, "high");
        p.scope = scope.clone();
        p.policy_revision = [1, 1, 1];
        p.expires = 100;
        p
    };
    let inside = make("claude.Write", json!({"file_path":"a.txt"}));
    let outside = make("claude.Write", json!({"file_path":"/etc/hosts"}));
    assert_eq!(ledger.tool_disposition(&inside, 11).unwrap(), ToolPolicy::AskOwner, "no mode saved: asks as before");
    ledger.set_approval_mode(owner, "a", MODE_OUTSIDE).unwrap();
    assert!(matches!(ledger.tool_disposition(&inside, 11).unwrap(), ToolPolicy::AllowWithRules { .. }));
    ledger.authorize_tool(&inside, 11).unwrap();
    assert_eq!(ledger.tool_disposition(&outside, 11).unwrap(), ToolPolicy::AskOwner);
    assert!(ledger.authorize_tool(&outside, 11).is_err(), "an outside change needs the owner's one-use approval");
    ledger.set_approval_mode(owner, "a", MODE_ASK).unwrap();
    assert_eq!(ledger.tool_disposition(&inside, 11).unwrap(), ToolPolicy::AskOwner);
    assert!(ledger.set_approval_mode(owner, "a", "trusted").is_err());
}
