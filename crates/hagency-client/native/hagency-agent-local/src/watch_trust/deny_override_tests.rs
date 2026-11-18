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
    // Another member of the Room: their read-only query is answered
    // (ADR 0019), anything else keeps the tool rule.
    let mut stranger = status.clone();
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
fn a_watch_without_push_freely_lifts_deny_only_for_read_only_queries() {
    let mut r = denied();
    let setting = WatchSetting { mode: WatchMode::All, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: false };
    r.ledger.set_issue_watch(OWNER, "bird", setting, 1).unwrap();
    // A read-only query on the watched repository: the watch reports it to
    // this Room anyway, so Deny does not refuse it (Chris, Nov 17).
    let read = bash(&r.root, &format!("gh issue view 33 -R {REPO}"));
    assert!(matches!(r.ledger.trusted_publish(OWNER, &read, 11).unwrap(), Some(super::TrustedStep::Command(_))));
    // Push freely is off: every other step keeps the Deny.
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

/// GPT review: the runtime builds each proposal with the CURRENT revisions,
/// so a rule saved after the run was admitted must still end the override.
#[test]
fn a_rule_saved_after_admission_ends_the_override_for_that_run() {
    let mut r = denied();
    let status = bash(&r.root, "git -C app status");
    assert!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap().is_some());
    let still_deny = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    r.ledger.set_policy(OWNER, &status.scope, crate::Layer::Room, 1, &still_deny).unwrap();
    let mut fresh = status;
    fresh.policy_revision = [1, 2, 1];
    assert_eq!(r.ledger.trusted_publish(OWNER, &fresh, 11).unwrap(), None);
}

/// Integration GPT review: the host's last check before a push re-reads
/// the same admission, not only the watch: a rule saved after the run was
/// admitted, or Requests Deny, stops the push in flight.
#[test]
fn the_pre_push_recheck_rereads_the_admission() {
    let mut r = denied();
    let push = bash(&r.root, "git -C app push -u origin fix/x");
    let still = r.ledger.push_trust_recheck(&push, REPO);
    assert!(still(), "admitted and trusted");
    let mut stranger = push.clone();
    stranger.scope.requester = "@mallory:example.org".into();
    assert!(!r.ledger.push_trust_recheck(&stranger, REPO)(), "only the owner's own run");
    let refuse = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Deny,
        high_risk: ToolPolicy::Deny,
    };
    r.ledger.set_policy(OWNER, &push.scope, crate::Layer::Room, 1, &refuse).unwrap();
    assert!(!still(), "a rule saved since admission stops the push");
    let mut fresh = push;
    fresh.policy_revision = [1, 2, 1];
    assert!(!r.ledger.push_trust_recheck(&fresh, REPO)(), "Requests Deny stops it whatever the revision");
}

/// Integration GPT review: `--fill*` and `--template` read the git checkout
/// gh runs in, which need not be the watched repository's: they ask.
#[test]
fn pr_autofill_and_templates_ask() {
    let mut r = denied();
    let base = format!("gh pr create --repo {REPO} --head fix/x");
    assert!(r.ledger.trusted_publish(OWNER, &bash(&r.root, &format!("{base} --title t --body b")), 11).unwrap().is_some());
    for extra in ["--fill", "-f", "--fill-first", "--fill-verbose", "--title t --template bug.md", "-T bug.md"] {
        let line = format!("{base} {extra}");
        assert_eq!(r.ledger.trusted_publish(OWNER, &bash(&r.root, &line), 11).unwrap(), None, "{line}");
    }
}
