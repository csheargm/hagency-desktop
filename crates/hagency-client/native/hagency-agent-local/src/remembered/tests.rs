use super::*;
use crate::{Layer, Ledger, Scope, ToolPolicy};
use serde_json::json;
use std::path::Path;

const OWNER: &str = "@owner:example.org";

fn ledger(dir: &Path) -> (Ledger, Scope) {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    let scope = Scope { agent: "a".into(), binding: "b".into(), room: "!r:x".into(), requester: OWNER.into(), thread: "t".into() };
    ledger.register_binding(OWNER, &scope).unwrap();
    let ask = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk: ToolPolicy::AskOwner,
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope, layer, 0, &ask).unwrap();
    }
    ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
    (ledger, scope)
}

fn request(scope: &Scope, tool: &str, arguments: Value, dir: &str) -> ToolProposal {
    ToolProposal {
        scope: scope.clone(), dispatch: "d".into(), tool: tool.into(), arguments,
        canonical_directory: dir.into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
    }
}

fn asks(ledger: &mut Ledger, p: &ToolProposal) -> bool {
    ledger.tool_disposition(p, 11).unwrap() == ToolPolicy::AskOwner
}

/// A real folder (macOS temp paths are symlinked, so canonicalize) and a
/// sibling `sub` folder.
fn folders(temp: &Path) -> (String, String) {
    let root = std::fs::canonicalize(temp).unwrap();
    std::fs::create_dir(root.join("w")).unwrap();
    std::fs::create_dir(root.join("w/sub")).unwrap();
    (root.join("w").display().to_string(), root.join("w/sub").display().to_string())
}

/// The owner's flow: the Agent asks for `cargo test`; the owner picks
/// "Always allow"; the identical request runs without a card, and nothing
/// broader does: not another command, not the same command elsewhere, not a
/// push, and not a chained line that starts the same way.
#[test]
fn always_allow_reuses_only_the_exact_request_the_owner_approved() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, sub) = folders(temp.path());
    let bash = |line: &str, dir: &str| request(&scope, "claude.Bash", json!({"command": line, "description": "x"}), dir);
    let first = bash("cargo test", &w);
    assert!(asks(&mut ledger, &first), "nothing remembered yet");
    ledger.remember_request(OWNER, &first, 5).unwrap();
    let again = request(&scope, "claude.Bash", json!({"command": "cargo test", "description": "run the tests again"}), &w);
    assert!(!asks(&mut ledger, &again), "the identical command is allowed");
    ledger.authorize_tool(&again, 11).unwrap();
    for other in [bash("cargo test --release", &w), bash("cargo test; ls", &w), bash("cargo test && echo ok", &w), bash("cargo test", &sub)] {
        assert!(asks(&mut ledger, &other), "{:?} in {} must still ask", other.arguments["command"], other.canonical_directory);
        assert!(ledger.authorize_tool(&other, 11).is_err());
    }
    let rules = ledger.remembered_requests(OWNER, "a").unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].request, r#"{"command":"cargo test"}"#);
    assert_eq!(rules[0].room, "!r:x");
    // Ask mode keeps the rule but asks; revoking it asks again in any mode.
    ledger.set_approval_mode(OWNER, "a", crate::outside::MODE_ASK).unwrap();
    assert!(asks(&mut ledger, &again));
    ledger.set_approval_mode(OWNER, "a", MODE_REMEMBER).unwrap();
    assert!(!asks(&mut ledger, &again));
    assert!(ledger.forget_request(OWNER, "a", &RememberedRequest { room: String::new(), ..rules[0].clone() }).unwrap());
    assert!(asks(&mut ledger, &again));
}

/// Item 1: pushes, network tools, remote gh writes and chained lines holding
/// one are never remembered, for Claude or Codex, and the card can say why.
#[test]
fn pushes_and_network_steps_are_never_remembered() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    for line in ["git push origin fix-6", "cargo test && git push", "curl -sS https://x", "gh pr merge 1", "npm publish", "scp a b:c", "echo $(curl x)"] {
        let bash = request(&scope, "claude.Bash", json!({"command": line}), &w);
        let codex = request(&scope, "codex.command", json!({"command": ["bash", "-lc", line], "cwd": w}), &w);
        for p in [bash, codex] {
            assert_eq!(request_key(&p), None, "{line}");
            assert!(is_network_step(&p), "{line}");
            assert!(ledger.remember_request(OWNER, &p, 5).is_err(), "{line}");
        }
    }
    let read = request(&scope, "claude.Bash", json!({"command": "gh pr view 3"}), &w);
    assert!(request_key(&read).is_some() && !is_network_step(&read), "gh reads stay rememberable");
}

/// Item 2: the shell and `-c`/`-lc` are part of the key, never unwrapped.
#[test]
fn shell_wrappers_are_kept_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let codex = |cmd: Value| request(&scope, "codex.command", json!({"command": cmd, "cwd": w, "itemId": "i1", "reason": "r"}), &w);
    ledger.remember_request(OWNER, &codex(json!(["bash", "-lc", "cargo test"])), 5).unwrap();
    assert!(!asks(&mut ledger, &codex(json!(["bash", "-lc", "cargo test"]))), "same argv, new ids");
    for other in [json!(["zsh", "-lc", "cargo test"]), json!(["bash", "-c", "cargo test"]), json!("/bin/zsh -lc \"cargo test\""), json!("cargo test")] {
        assert!(asks(&mut ledger, &codex(other.clone())), "{other}");
    }
}

/// Item 3: a rule made in one room does not answer for another room.
#[test]
fn a_rule_belongs_to_its_room() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    ledger.remember_request(OWNER, &request(&scope, "claude.Bash", json!({"command": "cargo test"}), &w), 5).unwrap();
    let other_room = Scope { binding: "b2".into(), room: "!other:x".into(), ..scope.clone() };
    ledger.register_binding(OWNER, &other_room).unwrap();
    let p = request(&other_room, "claude.Bash", json!({"command": "cargo test"}), &w);
    assert!(!remembered(&ledger.db, &p, None, 11).unwrap());
    assert!(remembered(&ledger.db, &request(&scope, "claude.Bash", json!({"command": "cargo test"}), &w), None, 11).unwrap());
}

/// Item 4: a symlinked, missing or non-canonical folder is never remembered.
#[test]
fn only_a_canonical_real_folder_is_remembered() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let link = format!("{w}-link");
    std::os::unix::fs::symlink(&w, &link).unwrap();
    for dir in [link.clone(), format!("{w}/sub/.."), format!("{w}/missing"), "relative".into()] {
        let p = request(&scope, "claude.Bash", json!({"command": "cargo test"}), &dir);
        assert!(ledger.remember_request(OWNER, &p, 5).is_err(), "{dir}");
    }
    let codex = request(&scope, "codex.command", json!({"command": "cargo test", "cwd": link}), &w);
    assert_eq!(request_key(&codex), None, "codex cwd must be the canonical folder");
}

/// Item 5: unknown argument fields fail closed; keyed fields must match.
#[test]
fn unknown_fields_make_a_request_one_use() {
    let temp = tempfile::tempdir().unwrap();
    let (_ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let bash = |args: Value| request_key(&request(&scope, "claude.Bash", args, &w));
    assert_eq!(bash(json!({"command": "cargo test", "dangerouslyDisableSandbox": true})), None);
    assert_ne!(bash(json!({"command": "cargo test", "timeout": 5})), bash(json!({"command": "cargo test"})));
    let codex = |extra: Value| {
        let mut args = json!({"command": "cargo test", "cwd": w});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request_key(&request(&scope, "codex.command", args, &w))
    };
    assert!(codex(json!({"grantRoot": null, "threadId": "t"})).is_some());
    assert_eq!(codex(json!({"sandbox": "danger-full-access"})), None);
}

/// Item 6 and migration: rules expire, and rows of the old table stop matching.
#[test]
fn rules_expire_and_old_rows_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let p = request(&scope, "claude.Bash", json!({"command": "cargo test"}), &w);
    ledger.remember_request(OWNER, &p, 5).unwrap();
    assert!(remembered(&ledger.db, &p, None, 5 + RULE_LIFETIME_SECS - 1).unwrap());
    assert!(!remembered(&ledger.db, &p, None, 5 + RULE_LIFETIME_SECS).unwrap());
    ledger.db.execute_batch("CREATE TABLE remembered_requests (agent TEXT, tool TEXT, directory TEXT, request TEXT, created INTEGER); INSERT INTO remembered_requests VALUES ('a','claude.Bash','/w','{}',1);").unwrap();
    ensure_schema(&ledger.db).unwrap();
    let old: i64 = ledger.db.query_row("SELECT count(*) FROM sqlite_master WHERE name='remembered_requests'", [], |r| r.get(0)).unwrap();
    assert_eq!(old, 0, "the room-less table is dropped");
}

#[test]
fn expansions_file_changes_network_and_mcp_can_only_be_approved_once() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let mut expansion = request(&scope, "codex.command", json!({"command": "cargo test", "cwd": w}), &w);
    expansion.risk = "permission_expansion".into();
    for p in [
        expansion,
        request(&scope, "codex.file_change", json!({"request": {}, "item": {}}), &w),
        request(&scope, "codex.network", json!({"command": "cargo test", "cwd": w}), &w),
        request(&scope, "codex.permissions", json!({}), &w),
        request(&scope, "mcp.github.merge", json!({}), &w),
        request(&scope, "claude.Bash", json!({"command": "  "}), &w),
    ] {
        assert_eq!(request_key(&p), None, "{}", p.tool);
        assert!(ledger.remember_request(OWNER, &p, 5).is_err());
    }
}

#[test]
fn claude_file_tools_match_their_exact_arguments_in_any_key_order() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let (w, _) = folders(temp.path());
    let edit = request(&scope, "claude.Edit", json!({"file_path": format!("{w}/a.rs"), "old_string": "a", "new_string": "b"}), &w);
    ledger.remember_request(OWNER, &edit, 5).unwrap();
    let reordered = request(&scope, "claude.Edit", json!({"new_string": "b", "old_string": "a", "file_path": format!("{w}/a.rs")}), &w);
    assert!(!asks(&mut ledger, &reordered));
    let other = request(&scope, "claude.Edit", json!({"file_path": format!("{w}/.git/config"), "old_string": "a", "new_string": "b"}), &w);
    assert!(asks(&mut ledger, &other));
}

#[test]
fn a_mode_table_from_before_remember_is_widened_and_keeps_its_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    drop(Ledger::open(&path, OWNER).unwrap());
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE approval_modes; CREATE TABLE approval_modes (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('ask','outside'))) STRICT; INSERT INTO approval_modes VALUES ('a','outside');").unwrap();
        ensure_schema(&db).unwrap();
        ensure_schema(&db).unwrap();
        let mode: String = db.query_row("SELECT mode FROM approval_modes WHERE agent='a'", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "outside");
        db.execute("INSERT INTO approval_modes VALUES ('b','remember')", []).unwrap();
        assert!(db.execute("INSERT INTO approval_modes VALUES ('c','trusted')", []).is_err());
    }
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(ledger.approval_mode("a").unwrap(), "outside");
    assert_eq!(ledger.approval_mode("unsaved").unwrap(), MODE_REMEMBER);
    ledger.set_approval_mode(OWNER, "a", MODE_REMEMBER).unwrap();
    assert!(ledger.set_approval_mode(OWNER, "a", "trusted").is_err());
}
