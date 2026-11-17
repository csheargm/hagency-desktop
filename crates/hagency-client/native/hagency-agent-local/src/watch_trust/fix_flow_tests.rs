//! User flow (live 2026-11-17): Betty's bird-claude, watch mode "all" on
//! csheargm/hagency-demo-sandbox with "push freely" on, is handed an issue.
//! Its whole fix (clone, fetch, read, edit, status, add, commit, push, draft
//! PR) runs with zero cards; the same calls from a run outside the watch's
//! Room, from another requester, or with the watch paused still ask.
use super::TrustedStep;
use super::host_git::{Dest, HostCtx};
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::{Layer, Ledger, Scope, ToolPolicy, ToolProposal};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const OWNER: &str = "@betty:example.org";
pub(crate) const ROOM: &str = "!sandbox:example.org";
pub(crate) const REPO: &str = "csheargm/hagency-demo-sandbox";

pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

pub(crate) struct Run {
    _temp: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) ledger: Ledger,
}

/// A workspace, a bare repository standing in for GitHub, and an Ask-owner
/// ledger with the watch on and push freely on.
fn run() -> Run {
    run_under(ToolPolicy::AskOwner)
}

/// [`run`] with every layer's high-risk rule `high_risk` (Deny: a hosted
/// Agent's default).
pub(crate) fn run_under(high_risk: ToolPolicy) -> Run {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let (root, seed, target) = (base.join("workspace"), base.join("seed"), base.join("github.git"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "# Sandbox\nTypo: teh\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "-m", "init"]);
    git(&base, &["clone", "-q", "--bare", seed.to_str().unwrap(), "github.git"]);
    let mut ledger = Ledger::open(base.join("local.db"), OWNER).unwrap();
    let scope = proposal(&root, ROOM, "claude.Bash", json!({})).scope;
    ledger.register_binding(OWNER, &scope).unwrap();
    let ask = crate::Policy {
        budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
        requests: crate::RequestPolicy::Allow,
        high_risk,
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope, layer, 0, &ask).unwrap();
    }
    ledger.reserve(&scope, "call", "d", 10, 10).unwrap();
    watch(&mut ledger, WatchMode::All);
    Run { _temp: temp, root, target, ledger }
}

pub(crate) fn watch(ledger: &mut Ledger, mode: WatchMode) {
    let setting = WatchSetting { mode, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: true };
    ledger.set_issue_watch(OWNER, "bird", setting, 1).unwrap();
}

pub(crate) fn proposal(root: &Path, room: &str, tool: &str, arguments: Value) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: "bird".into(), binding: "b".into(), room: room.into(), requester: OWNER.into(), thread: "t".into() },
        dispatch: "d".into(), tool: tool.into(), arguments,
        canonical_directory: root.to_string_lossy().into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
    }
}
fn bash(root: &Path, line: &str) -> ToolProposal {
    proposal(root, ROOM, "claude.Bash", json!({"command": line}))
}

/// What the Agent's call runs for a card-free step, as its shell would; a
/// host step goes to the local stand-in for GitHub.
pub(crate) fn perform(step: TrustedStep, target: &Path) -> String {
    let command = match step {
        TrustedStep::Allow => return String::new(),
        TrustedStep::Command(command) => command,
        TrustedStep::Host(host) => {
            let ctx = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true));
            host.run_to(&Dest { url: format!("file://{}", target.display()), protocol: "file" }, &ctx).unwrap()
        }
    };
    let out = Command::new("sh").arg("-c").arg(&command).env("GIT_AUTHOR_NAME", "bird").env("GIT_AUTHOR_EMAIL", "b@x").env("GIT_COMMITTER_NAME", "bird").env("GIT_COMMITTER_EMAIL", "b@x").output().unwrap();
    assert!(out.status.success(), "{command}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_hand_off_run_fixes_an_issue_with_zero_cards() {
    let mut r = run();
    let app = r.root.join("hagency-demo-sandbox");
    let readme = app.join("README.md").to_string_lossy().into_owned();
    let mut steps: Vec<ToolProposal> = vec![bash(&r.root, "gh repo clone csheargm/hagency-demo-sandbox ./hagency-demo-sandbox")];
    let mut cards = 0;
    let mut answer = |ledger: &mut Ledger, call: &ToolProposal, target: &Path| match ledger.trusted_publish(OWNER, call, 11).unwrap() {
        // `gh` would reach the real GitHub: answered without a card is enough here.
        Some(_) if call.arguments["command"].as_str().is_some_and(|c| c.starts_with("gh pr")) => String::new(),
        Some(step) => perform(step, target),
        None => {
            cards += 1;
            String::new()
        }
    };
    answer(&mut r.ledger, &steps.remove(0), &r.target);
    // The `origin/HEAD` the fix branch starts from, as a real clone has it.
    assert_eq!(git(&app, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]), "origin/main");
    for line in [
        "git -C hagency-demo-sandbox fetch origin",
        "git -C hagency-demo-sandbox status",
        "git -C hagency-demo-sandbox log --oneline -3",
        "git -C hagency-demo-sandbox checkout -b fix/typo origin/main",
    ] {
        answer(&mut r.ledger, &bash(&r.root, line), &r.target);
    }
    answer(&mut r.ledger, &proposal(&r.root, ROOM, "claude.Read", json!({"file_path": readme})), &r.target);
    let edit = proposal(&r.root, ROOM, "claude.Edit", json!({"file_path": readme, "old_string": "teh", "new_string": "the"}));
    answer(&mut r.ledger, &edit, &r.target);
    std::fs::write(&readme, "# Sandbox\nTypo: the\n").unwrap();
    for line in [
        "git -C hagency-demo-sandbox diff",
        "git -C hagency-demo-sandbox add README.md",
        "git -C hagency-demo-sandbox commit -m 'docs: fix typo (#1)'",
        "git -C hagency-demo-sandbox push -u origin fix/typo",
        "gh pr create --draft --repo csheargm/hagency-demo-sandbox --head fix/typo --title 'docs: fix typo' --body 'Fixes #1'",
    ] {
        let out = answer(&mut r.ledger, &bash(&r.root, line), &r.target);
        assert!(!line.contains(" push ") || out.starts_with("Pushed fix/typo"), "{out}");
    }
    assert_eq!(cards, 0, "the whole fix ran without a card");
    let pushed = git(&r.target, &["log", "-1", "--format=%s", "refs/heads/fix/typo"]);
    assert_eq!(pushed, "docs: fix typo (#1)", "the fix reached the watched repository");
    assert_eq!(git(&r.target, &["rev-parse", "refs/heads/main"]), git(&app, &["rev-parse", "origin/main"]), "main is untouched");
}

#[test]
fn the_same_steps_ask_outside_the_watch() {
    let mut r = run();
    let app = r.root.join("app");
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&app, &["remote", "set-url", "origin", "https://github.com/csheargm/hagency-demo-sandbox.git"]);
    let status = bash(&r.root, "git -C app status");
    let read = proposal(&r.root, ROOM, "claude.Read", json!({"file_path": app.join("README.md")}));
    assert!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap().is_some());
    assert!(r.ledger.trusted_publish(OWNER, &read, 11).unwrap().is_some());
    let mut elsewhere = status.clone();
    elsewhere.scope.room = "!other:example.org".into();
    let mut stranger = status.clone();
    stranger.scope.requester = "@mallory:example.org".into();
    for asks in [
        elsewhere,
        stranger,
        bash(&r.root, "git -C app fetch origin && cat app/README.md"),
        bash(&r.root, "cargo test --manifest-path app/Cargo.toml"),
        bash(&r.root, "npm install"),
        bash(&r.root, "curl https://example.com"),
        proposal(&r.root, ROOM, "claude.Write", json!({"file_path": r.root.join("notes.md")})),
        proposal(&r.root, ROOM, "claude.Read", json!({"file_path": "/etc/hosts"})),
        proposal(&r.root, ROOM, "claude.Edit", json!({"file_path": app.join(".git/config")})),
    ] {
        assert_eq!(r.ledger.trusted_publish(OWNER, &asks, 11).unwrap(), None, "{}", asks.arguments);
    }
    watch(&mut r.ledger, WatchMode::Off);
    assert_eq!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap(), None, "a paused watch asks");
    assert_eq!(r.ledger.trusted_publish(OWNER, &read, 11).unwrap(), None);
    let mut setting = r.ledger.issue_watch("bird").unwrap().setting;
    setting.mode = WatchMode::All;
    setting.push_trusted = false;
    r.ledger.set_issue_watch(OWNER, "bird", setting, 1).unwrap();
    assert_eq!(r.ledger.trusted_publish(OWNER, &status, 11).unwrap(), None, "push freely off asks");
}

/// GPT review (Nov 17): the permission callback derives the proposal's
/// directory from an absolute path argument. A second checkout of the same
/// repository OUTSIDE the workspace, so named, must still ask once the
/// proposal is rooted at the Room workspace; and reads of `.git` ask.
#[test]
fn a_checkout_outside_the_workspace_asks_whatever_the_derived_directory() {
    let mut r = run();
    let outside = r.root.parent().unwrap().join("elsewhere");
    git(r.root.parent().unwrap(), &["clone", "-q", r.target.to_str().unwrap(), "elsewhere"]);
    git(&outside, &["remote", "set-url", "origin", "https://github.com/csheargm/hagency-demo-sandbox.git"]);
    let file = outside.join("README.md");
    for tool in ["claude.Read", "claude.Edit"] {
        // What the callback produced: the file's own folder as the directory.
        let derived = proposal(&outside, ROOM, tool, json!({"file_path": file}));
        assert!(r.ledger.trusted_publish(OWNER, &derived, 11).unwrap().is_some(), "the old root would have trusted it");
        let rooted = super::for_workspace(&derived, &r.root);
        assert_eq!(r.ledger.trusted_publish(OWNER, &rooted, 11).unwrap(), None, "{tool}");
    }
    let app = r.root.join("app");
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&app, &["remote", "set-url", "origin", "https://github.com/csheargm/hagency-demo-sandbox.git"]);
    for path in [app.join(".git/config"), app.join(".git/hooks/pre-commit.sample")] {
        let read = proposal(&r.root, ROOM, "claude.Read", json!({"file_path": path}));
        assert_eq!(r.ledger.trusted_publish(OWNER, &read, 11).unwrap(), None, "{}", path.display());
    }
}

/// GPT review (Nov 17): a watch paused while a clone or pull is in flight
/// stops it before anything is handed to the Agent.
#[test]
fn a_revoked_watch_stops_an_in_flight_clone_or_pull() {
    let r = run();
    let off = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| false));
    let dest = Dest { url: format!("file://{}", r.target.display()), protocol: "file" };
    let clone = super::host_git::HostStep::Clone { target: r.root.join("app"), repo: REPO.into() };
    assert!(clone.run_to(&dest, &off).is_err_and(|e| e.contains("no longer lets")));
    assert!(!r.root.join("app").exists());
}
