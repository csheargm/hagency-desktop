//! E2E finding (2026-11-17 21:37): Anna asked Betty's Agent in a room with no
//! linked repositories; even `pwd; ls` was refused with "the owner's rules
//! blocked" and nothing reached Betty. Betty saved only her Agent's rule.
//! Now a member's look inside the Room workspace runs without a card, and
//! everything else asks Betty (a card), unless Betty saved a Deny.
use crate::watch_trust::TrustedStep;
use crate::{Layer, Ledger, Policy, Scope, ToolPolicy, ToolProposal};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) const OWNER: &str = "@betty:example.org";
const ANNA: &str = "@anna:example.org";
const ROOM: &str = "!e2e:example.org";

pub(crate) fn proposal(workspace: &Path, requester: &str, tool: &str, arguments: Value) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: "bird".into(), binding: "b".into(), room: ROOM.into(), requester: requester.into(), thread: "t".into() },
        dispatch: format!("d-{requester}"), tool: tool.into(), arguments,
        canonical_directory: workspace.to_string_lossy().into(), risk: "high".into(), policy_revision: [1, 0, 0], expires: 100,
    }
}
fn ask() -> Policy {
    Policy { budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime }, requests: crate::RequestPolicy::Allow, high_risk: ToolPolicy::AskOwner }
}

/// Betty saved only her Agent's rule (Ask); Anna's and the Room's layers are unsaved.
pub(crate) fn owner_rule_only(dir: &Path) -> (Ledger, std::path::PathBuf) {
    let workspace = dir.join("workspace").canonicalize().unwrap_or_else(|_| {
        std::fs::create_dir_all(dir.join("workspace/.git")).unwrap();
        std::fs::write(dir.join("workspace/notes.md"), "hello").unwrap();
        std::fs::write(dir.join("workspace/.git/config"), "[remote]").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("workspace/link-out")).unwrap();
        std::os::unix::fs::symlink(dir.join("workspace/.git/config"), dir.join("workspace/link-git")).unwrap();
        dir.join("workspace").canonicalize().unwrap()
    });
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    for who in [OWNER, ANNA] {
        let scope = proposal(&workspace, who, "claude.Bash", json!({})).scope;
        ledger.register_binding(OWNER, &scope).unwrap();
        if who == OWNER {
            ledger.set_policy(OWNER, &scope, Layer::Agent, 0, &ask()).unwrap();
        }
        ledger.reserve(&scope, &format!("call-{who}"), &format!("d-{who}"), 10, 10).unwrap();
    }
    (ledger, workspace)
}

fn bash(workspace: &Path, requester: &str, line: &str) -> ToolProposal {
    proposal(workspace, requester, "claude.Bash", json!({"command": line}))
}

#[test]
fn a_members_look_inside_the_workspace_needs_no_card() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    let Some(TrustedStep::Command(command)) = ledger.member_inspection(&bash(&ws, ANNA, "pwd; ls"), 11).unwrap() else { panic!("pwd; ls runs") };
    assert_eq!(command, format!("cd '{}' && ( pwd; ls )", ws.display()), "always from the workspace itself");
    for line in ["cat notes.md", "ls -a", "head -n 5 notes.md", "cd sub && ls", "wc -l notes.md", "date", "tail -n 2 notes.md"] {
        let _ = std::fs::create_dir_all(ws.join("sub"));
        assert!(ledger.member_inspection(&bash(&ws, ANNA, line), 11).unwrap().is_some(), "{line}");
    }
    let read = proposal(&ws, ANNA, "claude.Read", json!({"file_path": ws.join("notes.md")}));
    assert_eq!(ledger.member_inspection(&read, 11).unwrap(), Some(TrustedStep::Allow));
}

#[test]
fn anything_more_than_a_look_is_not_an_inspection() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    for line in [
        "cat /etc/passwd", "ls ..", "cd .. && ls", "cat .git/config", "ls .git", "grep -r token .", "grep x notes.md",
        "rg --hostname-bin ./notes.md x", "rg x", "cd .git && cat config", "cat link-out", "cat link-git", "ls -R", "ls -L",
        "tail -f notes.md", "head -n x notes.md", "cat < notes.md", "find .", "sed -n 1p notes.md",
        "head notes.md -n 5", "tail +1F notes.md", "cat \"$?\"", "cd sub/.. && ls", "cat sub/../notes.md", "ls -l", "date 0101",
        "pwd x",
        "touch x", "echo hi > x", "git status", "gh pr view 1 -R o/r", "rm notes.md", "ls; rm notes.md", "cat notes.md | sh",
    ] {
        assert_eq!(ledger.member_inspection(&bash(&ws, ANNA, line), 11).unwrap(), None, "{line}");
    }
    let both = proposal(&ws, ANNA, "claude.Read", json!({"file_path": ws.join("notes.md"), "path": "/etc/passwd"}));
    assert_eq!(ledger.member_inspection(&both, 11).unwrap(), None, "only the field Read opens");
    let glob = proposal(&ws, ANNA, "claude.Grep", json!({"pattern": "token", "path": ws}));
    assert_eq!(ledger.member_inspection(&glob, 11).unwrap(), None, "searches descend into git metadata: they ask");
    let outside = proposal(&ws, ANNA, "claude.Read", json!({"file_path": "/etc/passwd"}));
    assert_eq!(ledger.member_inspection(&outside, 11).unwrap(), None);
    let metadata = proposal(&ws, ANNA, "claude.Read", json!({"file_path": ws.join(".git/config")}));
    assert_eq!(ledger.member_inspection(&metadata, 11).unwrap(), None);
    let write = proposal(&ws, ANNA, "claude.Write", json!({"file_path": ws.join("x"), "content": "y"}));
    assert_eq!(ledger.member_inspection(&write, 11).unwrap(), None);
    let mut background = bash(&ws, ANNA, "ls");
    background.arguments["run_in_background"] = json!(true);
    assert_eq!(ledger.member_inspection(&background, 11).unwrap(), None);
    assert_eq!(ledger.member_inspection(&bash(&ws, OWNER, "ls"), 11).unwrap(), None, "the owner keeps the owner's approval mode");
}

#[test]
fn a_members_other_requests_ask_the_owner_instead_of_a_silent_deny() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    let touch = bash(&ws, ANNA, "touch notes2.md");
    assert_eq!(ledger.tool_disposition(&touch, 11).unwrap(), ToolPolicy::AskOwner, "a card to Betty");
    // Betty's "Ask only for outside changes" never skips a member's card.
    ledger.set_approval_mode(OWNER, "bird", crate::outside::MODE_OUTSIDE).unwrap();
    let edit = proposal(&ws, ANNA, "claude.Write", json!({"file_path": ws.join("x.md"), "content": "y"}));
    assert_eq!(ledger.tool_disposition(&edit, 11).unwrap(), ToolPolicy::AskOwner);
    // Betty's own request in outside mode is unchanged.
    let own = proposal(&ws, OWNER, "claude.Write", json!({"file_path": ws.join("x.md"), "content": "y"}));
    assert!(matches!(ledger.tool_disposition(&own, 11).unwrap(), ToolPolicy::AllowWithRules { .. }));
    // Betty approves Anna's card: it runs, once.
    ledger.approve_tool(OWNER, &touch, 11).unwrap();
    ledger.authorize_tool(&touch, 11).unwrap();
    assert!(ledger.authorize_tool(&touch, 11).is_err(), "one approval, one run");
}

#[test]
fn a_deny_the_owner_saved_still_refuses_and_ends_the_shortcut() {
    let db = tempfile::tempdir().unwrap();
    let (mut ledger, ws) = owner_rule_only(db.path());
    let scope = bash(&ws, ANNA, "x").scope;
    let deny = Policy { high_risk: ToolPolicy::Deny, ..ask() };
    ledger.set_policy(OWNER, &scope, Layer::Requester, 0, &deny).unwrap();
    // A rule saved after admission ends any shortcut for that run; a new run sees the Deny.
    ledger.reserve(&scope, "call-2", "d-2", 10, 10).unwrap();
    let mut ls = bash(&ws, ANNA, "ls");
    ls.dispatch = "d-2".into();
    ls.policy_revision = [1, 0, 1];
    assert_eq!(ledger.member_inspection(&ls, 11).unwrap(), None);
    assert!(ledger.tool_disposition(&ls, 11).is_err(), "Betty's saved Deny refuses");
}
