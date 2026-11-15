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

/// The owner's flow: the Agent asks for `cargo test`; the owner picks
/// "Always allow"; the identical request runs without a card, and nothing
/// broader does: not another command, not the same command elsewhere, not a
/// push, and not a chained line that starts the same way.
#[test]
fn always_allow_reuses_only_the_exact_request_the_owner_approved() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let bash = |line: &str, dir: &str| request(&scope, "claude.Bash", json!({"command": line, "description": "x"}), dir);
    let first = bash("cargo test", "/w");
    assert!(asks(&mut ledger, &first), "nothing remembered yet");
    ledger.remember_request(OWNER, &first, 5).unwrap();
    let again = request(&scope, "claude.Bash", json!({"command": "cargo test", "description": "run the tests again"}), "/w");
    assert!(!asks(&mut ledger, &again), "the identical command is allowed");
    ledger.authorize_tool(&again, 11).unwrap();
    for other in [bash("cargo test --release", "/w"), bash("cargo test; git push", "/w"), bash("cargo test && gh pr merge 1", "/w"), bash("git push", "/w"), bash("cargo test", "/other")] {
        assert!(asks(&mut ledger, &other), "{:?} in {} must still ask", other.arguments["command"], other.canonical_directory);
        assert!(ledger.authorize_tool(&other, 11).is_err());
    }
    let rules = ledger.remembered_requests(OWNER, "a").unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].request, r#"{"command":"cargo test"}"#);
    // Ask mode keeps the rule but asks; revoking it asks again in any mode.
    ledger.set_approval_mode(OWNER, "a", crate::outside::MODE_ASK).unwrap();
    assert!(asks(&mut ledger, &again));
    ledger.set_approval_mode(OWNER, "a", MODE_REMEMBER).unwrap();
    assert!(!asks(&mut ledger, &again));
    assert!(ledger.forget_request(OWNER, "a", &rules[0]).unwrap());
    assert!(asks(&mut ledger, &again));
}

#[test]
fn a_remembered_push_allows_only_that_push() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let codex = |cmd: Value, cwd: &str| request(&scope, "codex.command", json!({"command": cmd, "cwd": cwd, "itemId": "i1", "reason": "r"}), "/w");
    ledger.remember_request(OWNER, &codex(json!(["bash", "-lc", "git push origin fix-6"]), "/w"), 5).unwrap();
    let same = request(&scope, "codex.command", json!({"command": "/bin/zsh -lc \"git push origin fix-6\"", "cwd": "/w", "itemId": "i2", "reason": "again"}), "/w");
    assert!(!asks(&mut ledger, &same), "the same push, wrapped differently, with new ids");
    assert!(asks(&mut ledger, &codex(json!(["bash", "-lc", "git push --force origin fix-6"]), "/w")));
    assert!(asks(&mut ledger, &codex(json!(["bash", "-lc", "git push origin main"]), "/w")));
    assert!(asks(&mut ledger, &codex(json!(["bash", "-lc", "git push origin fix-6"]), "/w/sub")), "another cwd");
}

#[test]
fn expansions_file_changes_network_and_mcp_can_only_be_approved_once() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let mut expansion = request(&scope, "codex.command", json!({"command": "curl x", "cwd": "/w"}), "/w");
    expansion.risk = "permission_expansion".into();
    for p in [
        expansion,
        request(&scope, "codex.file_change", json!({"request": {}, "item": {}}), "/w"),
        request(&scope, "codex.network", json!({"command": "curl x", "cwd": "/w"}), "/w"),
        request(&scope, "codex.permissions", json!({}), "/w"),
        request(&scope, "mcp.github.merge", json!({}), "/w"),
        request(&scope, "claude.Bash", json!({"command": "  "}), "/w"),
    ] {
        assert_eq!(request_key(&p), None, "{}", p.tool);
        assert!(ledger.remember_request(OWNER, &p, 5).is_err());
    }
}

#[test]
fn claude_file_tools_match_their_exact_arguments_in_any_key_order() {
    let temp = tempfile::tempdir().unwrap();
    let (mut ledger, scope) = ledger(temp.path());
    let edit = request(&scope, "claude.Edit", json!({"file_path": "/w/a.rs", "old_string": "a", "new_string": "b"}), "/w");
    ledger.remember_request(OWNER, &edit, 5).unwrap();
    let reordered = request(&scope, "claude.Edit", json!({"new_string": "b", "old_string": "a", "file_path": "/w/a.rs"}), "/w");
    assert!(!asks(&mut ledger, &reordered));
    let other = request(&scope, "claude.Edit", json!({"file_path": "/w/.git/config", "old_string": "a", "new_string": "b"}), "/w");
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
