//! User flow (live 2026-11-17, issue #33): a hosted Agent runs under the
//! default Deny high-risk rule, and its hand-off run failed with "policy
//! declines (high-risk tools)". Chris: inside the exact push-freely scope
//! the switch overrides Deny; outside it, Deny still refuses.
use super::fix_flow_tests::{OWNER, REPO, ROOM, Run, git, perform, proposal, run_under, watch};
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::{ToolPolicy, ToolProposal};
use serde_json::json;
use std::path::Path;

fn bash(root: &Path, line: &str) -> ToolProposal {
    proposal(root, ROOM, "claude.Bash", json!({"command": line}))
}

/// A Deny ledger with the watched repository cloned at `app`.
fn denied() -> Run {
    let r = run_under(ToolPolicy::Deny);
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&r.root.join("app"), &["remote", "set-url", "origin", &format!("https://github.com/{REPO}.git")]);
    r
}

#[test]
fn push_freely_overrides_deny_inside_its_scope() {
    let mut r = denied();
    let readme = r.root.join("app/README.md");
    for call in [
        bash(&r.root, "git -C app status"),
        bash(&r.root, "git -C app fetch origin"),
        bash(&r.root, "git -C app checkout -b fix/typo origin/main"),
        bash(&r.root, &format!("gh issue view 33 -R {REPO}")),
        proposal(&r.root, ROOM, "claude.Read", json!({"file_path": readme})),
        proposal(&r.root, ROOM, "claude.Edit", json!({"file_path": readme, "old_string": "teh", "new_string": "the"})),
    ] {
        assert!(r.ledger.trusted_publish(OWNER, &call, 11).unwrap().is_some(), "{}", call.arguments);
    }
    // And it really runs: the status of the checkout, without a card.
    let step = r.ledger.trusted_publish(OWNER, &bash(&r.root, "git -C app status --short"), 11).unwrap().unwrap();
    assert_eq!(perform(step, &r.target), "");
    // The ordinary path still refuses: only push trust lifted the Deny.
    assert!(r.ledger.tool_disposition(&bash(&r.root, "git -C app status"), 11).is_err());
}

#[test]
fn deny_still_wins_outside_the_scope() {
    let mut r = denied();
    // A clone of another repository in the same workspace.
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "other"]);
    git(&r.root.join("other"), &["remote", "set-url", "origin", "https://github.com/csheargm/other.git"]);
    let status = bash(&r.root, "git -C app status");
    let mut elsewhere = status.clone();
    elsewhere.scope.room = "!other:example.org".into();
    let mut stranger = bash(&r.root, &format!("gh issue view 33 -R {REPO}"));
    stranger.scope.requester = "@mallory:example.org".into();
    let mut expired = status.clone();
    expired.expires = 5;
    for refused in [
        bash(&r.root, "git -C other status"),
        bash(&r.root, "git -C app status && curl https://example.com"),
        bash(&r.root, "git -C app status; rm -rf app"),
        bash(&r.root, "cargo test --manifest-path app/Cargo.toml"),
        bash(&r.root, "gh issue view 1 -R csheargm/other"),
        proposal(&r.root, ROOM, "claude.Edit", json!({"file_path": r.root.join("notes.md")})),
        proposal(&r.root, ROOM, "claude.Edit", json!({"file_path": r.root.join("app/.git/config")})),
        elsewhere,
        stranger,
        expired,
    ] {
        assert_eq!(r.ledger.trusted_publish(OWNER, &refused, 11).unwrap(), None, "{}", refused.arguments);
    }
    watch(&mut r.ledger, WatchMode::Off);
    assert_eq!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap(), None, "a paused watch grants nothing");
}

#[test]
fn a_watch_without_push_freely_never_lifts_deny() {
    let mut r = denied();
    let setting = WatchSetting { mode: WatchMode::All, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: false };
    r.ledger.set_issue_watch(OWNER, "bird", setting, 1).unwrap();
    // The owner's read-only query on a watched repository: card-free under
    // Ask (ADR 0019), but push freely is off, so Deny keeps refusing it.
    let read = bash(&r.root, &format!("gh issue view 33 -R {REPO}"));
    assert_eq!(r.ledger.trusted_publish(OWNER, &read, 11).unwrap(), None);
    assert_eq!(r.ledger.trusted_publish(OWNER, &bash(&r.root, "git -C app status"), 11).unwrap(), None);
}

#[test]
fn a_changed_policy_or_refused_requests_still_stop_the_override() {
    let mut r = denied();
    let status = bash(&r.root, "git -C app status");
    // The owner saves a new rule: the proposal's revisions are stale.
    let deny = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Deny,
        high_risk: ToolPolicy::Deny,
    };
    r.ledger.set_policy(OWNER, &status.scope, crate::Layer::Agent, 1, &deny).unwrap();
    assert_eq!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap(), None, "stale revision");
    let mut current = status;
    current.policy_revision = [2, 1, 1];
    assert_eq!(r.ledger.trusted_publish(OWNER, &current, 11).unwrap(), None, "requests refused outright");
}
