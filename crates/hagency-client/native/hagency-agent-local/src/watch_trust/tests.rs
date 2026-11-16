use super::*;
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::{Ledger, Scope};
use serde_json::json;

const OWNER: &str = "@alice:example.org";
const ROOM: &str = "!watch:example.org";
const REPO: &str = "acme/app";

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}
/// A workspace holding a clone of `acme/app` on branch `docs/faq`.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join("app");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "--allow-empty", "-m", "init"]);
    git(&repo, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
    git(&repo, &["checkout", "-q", "-b", "docs/faq"]);
    (temp, root)
}
fn allowed(line: &str, root: &Path) -> bool {
    line_allowed(line, root, REPO, true)
}

#[test]
fn a_branch_push_and_draft_pr_in_the_watched_repo_are_allowed() {
    let (_t, root) = workspace();
    for line in [
        "cd app && git push -u origin docs/faq",
        "cd app && git push --set-upstream origin HEAD",
        "cd app && git add README.md && git commit -m 'docs: faq' && git push -u origin docs/faq",
        "cd app && gh pr create --draft --repo acme/app --title 'docs: FAQ' --body 'Fixes #27' --base main",
        "cd app && gh pr create --draft --title t --body b",
        "cd app && gh pr ready 28 --repo acme/app",
        "gh pr ready https://github.com/acme/app/pull/28",
        "cd app && gh pr edit 28 --add-label docs -R acme/app",
    ] {
        assert!(allowed(line, &root), "{line}");
    }
}

#[test]
fn a_bare_push_follows_the_current_branch_only_with_safe_config() {
    let (_t, root) = workspace();
    let repo = root.join("app");
    git(&repo, &["config", "branch.docs/faq.remote", "origin"]);
    git(&repo, &["config", "branch.docs/faq.merge", "refs/heads/docs/faq"]);
    assert!(allowed("cd app && git push", &root));
    git(&repo, &["config", "push.default", "matching"]);
    assert!(!allowed("cd app && git push", &root), "matching pushes every branch, main included");
    git(&repo, &["config", "push.default", "simple"]);
    git(&repo, &["config", "remote.origin.push", "refs/heads/*:refs/heads/*"]);
    assert!(!allowed("cd app && git push", &root), "a configured refspec widens it");
}

#[test]
fn force_delete_tags_default_branch_and_refspecs_still_ask() {
    let (_t, root) = workspace();
    for line in [
        "cd app && git push --force origin docs/faq",
        "cd app && git push -f origin docs/faq",
        "cd app && git push --force-with-lease origin docs/faq",
        "cd app && git push --mirror origin",
        "cd app && git push --delete origin docs/faq",
        "cd app && git push origin :docs/faq",
        "cd app && git push origin +docs/faq",
        "cd app && git push --tags origin",
        "cd app && git push origin main",
        "cd app && git push origin master",
        "cd app && git push origin docs/faq:main",
        "cd app && git push origin refs/heads/docs/faq",
        "cd app && git push origin no-such-branch",
        "cd app && git push https://github.com/acme/app.git docs/faq",
        "cd app && git push origin docs/faq || git push -f origin docs/faq",
        "cd app && gh pr create --web",
        "cd app && gh pr create -w",
        "cd app && gh pr create --editor",
        "cd app && gh pr create --repo acme/other --title t --body b",
        "cd app && gh pr create --body-file /etc/passwd --title t",
        "cd app && gh pr merge 28 --squash --repo acme/app",
        "gh pr ready https://github.com/acme/other/pull/28",
        "cd app && gh pr ready 28 --repo acme/other",
        "cd app && git status",
        "git -C app push origin docs/faq",
    ] {
        assert!(!allowed(line, &root), "{line}");
    }
}

#[test]
fn the_default_branch_named_by_the_remote_is_protected() {
    let (_t, root) = workspace();
    let repo = root.join("app");
    git(&repo, &["branch", "trunk"]);
    git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);
    assert!(!allowed("cd app && git push origin trunk", &root));
}

#[test]
fn a_remote_or_push_url_of_another_repository_asks() {
    let (_t, root) = workspace();
    let repo = root.join("app");
    git(&repo, &["remote", "add", "fork", "https://github.com/mallory/app.git"]);
    assert!(!allowed("cd app && git push -u fork docs/faq", &root));
    assert!(!allowed("cd app && gh pr create --title t --body b", &root), "gh could pick the fork: name --repo");
    assert!(allowed("cd app && gh pr create --repo acme/app --title t --body b", &root));
    git(&repo, &["config", "--add", "remote.origin.pushurl", "https://github.com/mallory/app.git"]);
    assert!(!allowed("cd app && git push -u origin docs/faq", &root), "the push URL is what counts");
    git(&repo, &["config", "--unset-all", "remote.origin.pushurl"]);
    git(&repo, &["config", "url.https://evil.example/.pushInsteadOf", "https://github.com/"]);
    assert!(!allowed("cd app && git push -u origin docs/faq", &root), "insteadOf rewriting is seen");
}

#[test]
fn in_workspace_steps_chain_only_in_outside_mode() {
    let (_t, root) = workspace();
    let line = "cd app && git commit -m x && git push -u origin docs/faq";
    assert!(line_allowed(line, &root, REPO, true));
    assert!(!line_allowed(line, &root, REPO, false), "ask mode: the commit would have asked");
    assert!(line_allowed("cd app && git push -u origin docs/faq", &root, REPO, false));
    assert!(!line_allowed("cd app && git push -u origin docs/faq", &root, "acme/other", true));
}

#[test]
fn github_urls_are_read_in_every_form() {
    for url in ["https://github.com/acme/app.git", "https://github.com/acme/app", "git@github.com:acme/app.git", "ssh://git@github.com/acme/app.git"] {
        assert_eq!(github_repo_of(url), Some("acme/app"), "{url}");
    }
    for url in ["https://gitlab.com/acme/app.git", "https://github.com.evil/acme/app", "https://x@github.com/acme/app", "/tmp/app", "https://github.com/acme/app/extra"] {
        assert_eq!(github_repo_of(url), None, "{url}");
    }
}

fn proposal(root: &Path, room: &str, tool: &str, risk: &str) -> ToolProposal {
    ToolProposal {
        scope: Scope { agent: "a".into(), binding: "b".into(), room: room.into(), requester: OWNER.into(), thread: "t".into() },
        dispatch: "d".into(), tool: tool.into(), arguments: json!({"command": "cd app && git push -u origin docs/faq"}),
        canonical_directory: root.to_string_lossy().into(), risk: risk.into(), policy_revision: [1, 1, 1], expires: 1,
    }
}
fn watch(ledger: &mut Ledger, mode: WatchMode, trusted: bool) {
    let setting = WatchSetting { mode, label: String::new(), repo: REPO.into(), room_id: ROOM.into(), push_trusted: trusted };
    ledger.set_issue_watch(OWNER, "a", setting, 1).unwrap();
}

#[test]
fn only_the_watched_room_with_the_switch_on_skips_the_card() {
    let (_t, root) = workspace();
    let db = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(db.path().join("local.db"), OWNER).unwrap();
    let here = proposal(&root, ROOM, "claude.Bash", "high");
    assert!(!allows(&ledger.db, &here, true).unwrap(), "no watch");
    watch(&mut ledger, WatchMode::Trusted, false);
    assert!(!allows(&ledger.db, &here, true).unwrap(), "switch off (the default)");
    watch(&mut ledger, WatchMode::Trusted, true);
    assert!(ledger.issue_watch("a").unwrap().setting.push_trusted);
    assert!(allows(&ledger.db, &here, true).unwrap());
    assert!(!allows(&ledger.db, &proposal(&root, "!other:example.org", "claude.Bash", "high"), true).unwrap(), "another Room");
    assert!(!allows(&ledger.db, &proposal(&root, ROOM, "claude.Bash", "permission_expansion"), true).unwrap());
    assert!(!allows(&ledger.db, &proposal(&root, ROOM, "claude.Write", "high"), true).unwrap());
    watch(&mut ledger, WatchMode::Off, true);
    assert!(!ledger.issue_watch("a").unwrap().setting.push_trusted, "switching the watch off drops the trust");
    assert!(!allows(&ledger.db, &here, true).unwrap());
}

/// The user flow: an Ask-owner Agent in the watched Room. The push and draft
/// PR skip the card only once the owner switches trust on; a force push and a
/// push to main still ask, and the approval is still one-use.
#[test]
fn ledger_skips_the_card_for_trusted_publishing_only() {
    use crate::{Layer, ToolPolicy};
    let (_t, root) = workspace();
    let db = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(db.path().join("local.db"), OWNER).unwrap();
    let scope = proposal(&root, ROOM, "claude.Bash", "high").scope;
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
    ledger.set_approval_mode(OWNER, "a", crate::outside::MODE_OUTSIDE).unwrap();
    let make = |line: &str| {
        let mut p = proposal(&root, ROOM, "claude.Bash", "high");
        p.arguments = json!({"command": line});
        p.expires = 100;
        p
    };
    let push = make("cd app && git commit -m 'docs: faq' && git push -u origin docs/faq");
    let pr = make("cd app && gh pr create --draft --repo acme/app --title t --body b");
    let risky = [make("cd app && git push -f origin docs/faq"), make("cd app && git push origin main")];
    assert_eq!(ledger.tool_disposition(&push, 11).unwrap(), ToolPolicy::AskOwner, "trust off: asks");
    watch(&mut ledger, WatchMode::Trusted, true);
    for allowed in [&push, &pr] {
        assert!(matches!(ledger.tool_disposition(allowed, 11).unwrap(), ToolPolicy::AllowWithRules { .. }));
    }
    for risky in &risky {
        assert_eq!(ledger.tool_disposition(risky, 11).unwrap(), ToolPolicy::AskOwner);
        assert!(ledger.authorize_tool(risky, 11).is_err());
    }
}

/// Regression (live 2026-11-15): an Agent ran `cd <repo>` while already in it,
/// the `cd` failed and the push had to be retried. Both runtimes are told.
#[test]
fn agents_are_told_the_shell_keeps_its_directory() {
    for note in [hagency_runtime::claude::AGENT_TOOLS_NOTE, hagency_runtime::AGENT_MERGE_NOTE] {
        assert!(note.contains("keeps its working directory between commands"));
        assert!(note.contains("run `pwd` before any `cd`"));
    }
}
