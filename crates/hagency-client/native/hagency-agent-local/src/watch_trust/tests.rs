use super::*;
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::{Ledger, Scope};
use serde_json::json;
use std::path::PathBuf;

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
fn env_prefix() -> &'static str {
    "GIT_CONFIG_COUNT='3' GIT_CONFIG_KEY_0='core.hooksPath' GIT_CONFIG_VALUE_0='/dev/null' GIT_CONFIG_KEY_1='core.fsmonitor' GIT_CONFIG_VALUE_1='false' GIT_CONFIG_KEY_2='commit.gpgSign' GIT_CONFIG_VALUE_2='false'"
}
fn allowed(line: &str, root: &Path) -> bool {
    pinned_line(line, root, REPO).is_some()
}
const PUSH: &str = "git -C app push -u origin docs/faq";

#[test]
fn a_named_branch_push_and_named_repo_prs_are_allowed() {
    let (_t, root) = workspace();
    let abs = format!("git -C {} push origin docs/faq", root.join("app").display());
    for line in [
        PUSH,
        "git -C app push --set-upstream origin docs/faq",
        abs.as_str(),
        "gh pr create --draft --repo acme/app --head docs/faq --title 'docs: FAQ' --body 'Fixes #27' --base main",
        "gh pr ready 28 --repo acme/app",
        "gh pr ready https://github.com/acme/app/pull/28",
        "gh pr edit 28 --add-label docs -R acme/app",
    ] {
        assert!(allowed(line, &root), "{line}");
    }
}

/// The push is the host's: it names the checked checkout, branch and the
/// watched repository, so config written after the check cannot steer it.
#[test]
fn the_push_is_run_by_the_host_to_the_checked_destination() {
    let (_t, root) = workspace();
    let dir = root.join("app");
    let push = host_git::HostStep::Push { dir, remote: "origin".into(), branch: "docs/faq".into(), repo: REPO.into(), upstream: true };
    assert_eq!(pinned_line(PUSH, &root, REPO), Some(TrustedStep::Host(push)));
    let gh = "gh pr ready 28 --repo acme/app";
    assert_eq!(pinned_line(gh, &root, REPO), Some(TrustedStep::Command(format!("{} {gh}", env_prefix()))), "gh runs git with hooks and fsmonitor off");
}

/// GPT review P1s: branch switches before the push, the shell's persistent
/// directory, chained steps and unnamed destinations all ask.
#[test]
fn chained_unnamed_or_directory_relative_publishing_asks() {
    let (_t, root) = workspace();
    for line in [
        "git switch -c master && git -C app push origin HEAD",
        "git -C app switch -c master && git -C app push origin master",
        "git -C app commit -m x && git -C app push origin docs/faq",
        "cd app && git push -u origin docs/faq",
        "git push -u origin docs/faq",
        "git -C app push",
        "git -C app push origin",
        "git -C app push origin HEAD",
        "git -C app push origin HEAD:refs/heads/docs/faq",
        "git -C /tmp push origin docs/faq",
        "git -C ../ push origin docs/faq",
        "cd app && gh pr create --draft --title t --body b",
        "gh pr create --repo acme/app --title t --body-file body.md",
        "gh pr ready 28 --repo acme/app && git -C app push origin docs/faq",
        "gh pr create --repo acme/app --title t --body b",
    ] {
        assert!(!allowed(line, &root), "{line}");
    }
}

#[test]
fn force_delete_tags_default_branch_and_refspecs_still_ask() {
    let (_t, root) = workspace();
    for line in [
        "git -C app push --force origin docs/faq",
        "git -C app push -f origin docs/faq",
        "git -C app push --force-with-lease origin docs/faq",
        "git -C app push --mirror origin",
        "git -C app push --delete origin docs/faq",
        "git -C app push origin :docs/faq",
        "git -C app push origin +docs/faq",
        "git -C app push --tags origin",
        "git -C app push origin main",
        "git -C app push origin master",
        "git -C app push origin docs/faq:main",
        "git -C app push origin refs/heads/docs/faq",
        "git -C app push origin no-such-branch",
        "git -C app push https://github.com/acme/app.git docs/faq",
        "gh pr create --web --repo acme/app",
        "gh pr create --editor --repo acme/app",
        "gh pr create --repo acme/other --title t --body b",
        "gh pr create --repo acme/app --body-file /etc/passwd --title t",
        "gh pr merge 28 --squash --repo acme/app",
        "gh pr ready https://github.com/acme/other/pull/28",
        "gh pr ready 28 --repo acme/other",
        "git -C app status",
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
    assert!(!allowed("git -C app push origin trunk", &root));
}

#[test]
fn a_remote_or_push_url_of_another_repository_asks() {
    let (_t, root) = workspace();
    let repo = root.join("app");
    git(&repo, &["remote", "add", "fork", "https://github.com/mallory/app.git"]);
    assert!(!allowed("git -C app push -u fork docs/faq", &root));
    git(&repo, &["config", "--add", "remote.origin.pushurl", "https://github.com/mallory/app.git"]);
    assert!(!allowed(PUSH, &root), "the push URL is what counts");
    git(&repo, &["config", "--unset-all", "remote.origin.pushurl"]);
    git(&repo, &["config", "url.https://evil.example/.pushInsteadOf", "https://github.com/"]);
    assert!(!allowed(PUSH, &root), "insteadOf rewriting is seen");
    assert!(pinned_line(PUSH, &root, "acme/other").is_none());
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
        dispatch: "d".into(), tool: tool.into(), arguments: json!({"command": PUSH}),
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
    assert!(!pinned(&ledger.db, &here, OWNER).unwrap().is_some(), "no watch");
    watch(&mut ledger, WatchMode::Trusted, false);
    assert!(!pinned(&ledger.db, &here, OWNER).unwrap().is_some(), "switch off (the default)");
    watch(&mut ledger, WatchMode::Trusted, true);
    assert!(ledger.issue_watch("a").unwrap().setting.push_trusted);
    assert!(pinned(&ledger.db, &here, OWNER).unwrap().is_some());
    assert!(!pinned(&ledger.db, &proposal(&root, "!other:example.org", "claude.Bash", "high"), OWNER).unwrap().is_some(), "another Room");
    assert!(!pinned(&ledger.db, &proposal(&root, ROOM, "claude.Bash", "permission_expansion"), OWNER).unwrap().is_some());
    assert!(!pinned(&ledger.db, &proposal(&root, ROOM, "codex.command", "high"), OWNER).unwrap().is_some(), "Codex cannot pin the command");
    assert!(!pinned(&ledger.db, &here, "@bob:example.org").unwrap().is_some(), "a non-owner requester asks");
    let mut background = proposal(&root, ROOM, "claude.Bash", "high");
    background.arguments["run_in_background"] = json!(true);
    assert!(!pinned(&ledger.db, &background, OWNER).unwrap().is_some(), "a background call asks");
    assert!(!pinned(&ledger.db, &proposal(&root, ROOM, "claude.Write", "high"), OWNER).unwrap().is_some());
    watch(&mut ledger, WatchMode::Off, true);
    assert!(!ledger.issue_watch("a").unwrap().setting.push_trusted, "switching the watch off drops the trust");
    assert!(!pinned(&ledger.db, &here, OWNER).unwrap().is_some());
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
    let push = make(PUSH);
    let pr = make("gh pr create --draft --repo acme/app -H docs/faq --title t --body b");
    let risky = [make("git -C app push -f origin docs/faq"), make("git -C app push origin main"), make("git switch -c master && git -C app push origin HEAD")];
    assert_eq!(ledger.trusted_publish(OWNER, &push, 11).unwrap(), None, "trust off: asks");
    watch(&mut ledger, WatchMode::Trusted, true);
    let mut stranger = push.clone();
    stranger.scope.requester = "@bob:example.org".into();
    assert_eq!(ledger.trusted_publish(OWNER, &stranger, 11).unwrap(), None, "a non-owner requester asks");
    for risky in &risky {
        assert_eq!(ledger.trusted_publish(OWNER, risky, 11).unwrap(), None);
        assert!(ledger.authorize_tool(risky, 11).is_err());
    }
    // GPT review: one check authorizes and returns what must run.
    let step = ledger.trusted_publish(OWNER, &push, 11).unwrap().unwrap();
    assert!(matches!(&step, TrustedStep::Host(host_git::HostStep::Push { repo, .. }) if repo == REPO), "the host runs the push: {step:?}");
    let Some(TrustedStep::Command(pr)) = ledger.trusted_publish(OWNER, &pr, 11).unwrap() else { panic!("gh pr create is pinned") };
    assert!(pr.starts_with("GIT_CONFIG_COUNT="));
}

/// Regression (live 2026-11-15): an Agent ran `cd <repo>` while already in it,
/// the `cd` failed and the push had to be retried. Both runtimes are told.
#[test]
fn agents_are_told_the_shell_keeps_its_directory() {
    for note in [hagency_runtime::claude::AGENT_TOOLS_NOTE, hagency_runtime::AGENT_MERGE_NOTE] {
        assert!(note.contains("keeps its working directory between commands"));
        assert!(note.contains("run `pwd` before any `cd`"));
        assert!(note.contains(hagency_runtime::TRUSTED_PUSH), "agents are told the one push shape trust answers");
    }
    let (_t, root) = workspace();
    let told = hagency_runtime::TRUSTED_PUSH.replace("<repo path>", "app").replace("<branch>", "docs/faq");
    assert!(allowed(&told, &root), "the shape the note teaches is the shape trust allows");
}

/// GPT review (Nov 15): an unknown default branch, `push.followTags` and a
/// local `core.sshCommand` each make the push ask.
#[test]
fn risky_push_config_asks() {
    let (_t, root) = workspace();
    let repo = root.join("app");
    let line = PUSH;
    assert!(allowed(line, &root));
    git(&repo, &["config", "core.sshCommand", "sh -c 'printenv GH_TOKEN'"]);
    assert!(!allowed(line, &root));
    git(&repo, &["config", "--unset", "core.sshCommand"]);
    git(&repo, &["config", "push.followTags", "true"]);
    assert!(!allowed(line, &root));
    git(&repo, &["config", "--unset", "push.followTags"]);
    git(&repo, &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
    assert!(!allowed(line, &root), "default branch unknown");
}

/// Issue fix flow: file edits inside the workspace skip the card in the
/// watched Room; git metadata, outside paths and other Rooms still ask.
#[test]
fn workspace_edits_are_trusted_only_in_the_watched_room() {
    let (_t, root) = workspace();
    let db = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(db.path().join("local.db"), OWNER).unwrap();
    let edit = |room: &str, path: &str| {
        let mut p = proposal(&root, room, "claude.Edit", "high");
        p.arguments = json!({"file_path": path, "old_string": "a", "new_string": "b"});
        p
    };
    let readme = root.join("app/README.md").to_string_lossy().into_owned();
    assert_eq!(pinned(&ledger.db, &edit(ROOM, &readme), OWNER).unwrap(), None, "no trust");
    watch(&mut ledger, WatchMode::Trusted, true);
    assert_eq!(pinned(&ledger.db, &edit(ROOM, &readme), OWNER).unwrap(), Some(TrustedStep::Allow));
    assert_eq!(pinned(&ledger.db, &edit(ROOM, "app/docs/faq.md"), OWNER).unwrap(), Some(TrustedStep::Allow));
    for path in ["app/.git/config", "app/.gitattributes", "/etc/hosts", "../x.md"] {
        assert_eq!(pinned(&ledger.db, &edit(ROOM, path), OWNER).unwrap(), None, "{path}");
    }
    assert_eq!(pinned(&ledger.db, &edit("!other:example.org", &readme), OWNER).unwrap(), None);
}
