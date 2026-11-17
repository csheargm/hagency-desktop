//! The local fix steps "push freely" answers (ADR 0022): the allowlist, path
//! escapes, links, config and hook tricks, and other-repository remotes.
use crate::watch_trust::{TrustedStep, pinned_line};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "acme/app";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A workspace with checkout `app` (origin = github acme/app, branch `fix/27`).
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let app = root.join("app");
    std::fs::create_dir(&app).unwrap();
    git(&app, &["init", "-q", "-b", "main"]);
    git(&app, &["config", "user.name", "t"]);
    git(&app, &["config", "user.email", "t@x"]);
    std::fs::write(app.join("README.md"), "hi\n").unwrap();
    git(&app, &["add", "README.md"]);
    git(&app, &["commit", "-q", "-m", "init"]);
    git(&app, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    git(&app, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&app, &["checkout", "-q", "-b", "fix/27"]);
    (temp, root)
}

fn command(line: &str, root: &Path) -> Option<String> {
    match pinned_line(line, root, REPO) {
        Some(TrustedStep::Command(c)) => Some(c),
        _ => None,
    }
}

fn agent_runs(command: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(command).output().unwrap();
    assert!(out.status.success(), "{command}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_listed_local_steps_in_the_own_checkout_are_pinned() {
    let (_t, root) = workspace();
    for line in [
        "git -C app status",
        "git -C app status -sb",
        "git -C app diff",
        "git -C app diff --cached --stat",
        "git -C app diff origin/main...HEAD -- README.md",
        "git -C app log --oneline -5",
        "git -C app log -n 3 --stat",
        "git -C app show HEAD --stat",
        "git -C app branch --show-current",
        "git -C app add README.md",
        "git -C app add -A",
        "git -C app commit -m 'docs: fix typo'",
        "git -C app restore --staged README.md",
    ] {
        let pinned = command(line, &root).unwrap_or_else(|| panic!("{line}"));
        for pin in ["'core.hooksPath=/dev/null'", "'core.fsmonitor=false'", "'protocol.allow=never'", "GIT_ATTR_SOURCE=", "-u GIT_DIR"] {
            assert!(pinned.contains(pin), "{line}: {pin}");
        }
    }
    assert!(command("git status", &root.join("app")).is_some(), "bare git from the checkout's own top");
    assert!(command("git -C app diff", &root).unwrap().contains("'--no-ext-diff' '--no-textconv'"));
    assert!(command("git -C app commit -m x", &root).unwrap().contains("'--no-verify'"));
}

#[test]
fn a_commit_message_from_a_heredoc_runs_and_nothing_else_reads_one() {
    let (_t, root) = workspace();
    std::fs::write(root.join("app/README.md"), "fixed\n").unwrap();
    agent_runs(&command("git -C app add README.md", &root).unwrap());
    let line = "git -C app commit -F - <<'EOF'\ndocs: fix\n\nFixes #27\nEOF";
    agent_runs(&command(line, &root).expect("commit -F - with a heredoc"));
    assert_eq!(git(&root.join("app"), &["log", "-1", "--format=%s"]), "docs: fix");
    for asks in [
        "git -C app status <<'EOF'\nx\nEOF",
        "git -C app commit -m x <<'EOF'\nx\nEOF",
        "git -C app commit -F -",
        "git -C app commit -F msg.txt",
        // GPT review: git reads the LAST -F, from anywhere.
        "git -C app commit -m x -F /etc/passwd",
        "git -C app commit -F - -F /etc/passwd <<'EOF'\nx\nEOF",
        "git -C app commit -F - --file=/etc/passwd <<'EOF'\nx\nEOF",
        "git -C app commit -F - -F - <<'EOF'\nx\nEOF",
        "git -C app commit -F - <<'EOF'\nx\nEOF\nrm -rf app",
        "git -C app push origin fix/27 <<'EOF'\ncommit\nEOF",
    ] {
        assert_eq!(pinned_line(asks, &root, REPO), None, "{asks}");
    }
}

#[test]
fn unlisted_forms_chains_and_escaping_paths_ask() {
    let (_t, root) = workspace();
    std::os::unix::fs::symlink("/etc", root.join("app/etc")).unwrap();
    for line in [
        "git -C app status && cat README.md",
        "git -C app status; git -C app add .",
        "git -C app status | head",
        "git -C app diff > out.patch",
        "git -C app diff $(echo x)",
        "git -C app diff --no-index /etc/passwd README.md",
        "git -C app diff /etc/passwd",
        "git -C app diff ../../x",
        "git -C app add etc/passwd",
        "git -C app add ../outside.txt",
        "git -C app add",
        "git -C app add -f secrets",
        "git -C app diff --ext-diff",
        "git -C app diff --output=/tmp/x",
        "git -C app log --format=%H",
        "git -C app commit",
        "git -C app commit --amend -m x",
        "git -C app commit -m x README.md",
        "git -C app branch -D main",
        "git -C app branch new",
        "git -C app reset --hard origin/main",
        "git -C app stash",
        "git -C app rebase origin/main",
        "git -C app merge origin/main",
        "git -C app config core.hooksPath x",
        "git -C app config --list",
        "git -C app -c core.pager=sh status",
        "git -c core.fsmonitor=x -C app status",
        "git -C app --git-dir=/tmp/x status",
        "git -C app restore",
        "git -C app restore --source=HEAD~1 README.md",
        "GIT_DIR=/tmp/x git -C app status",
        "git -C app submodule update",
        "git -C ../ status",
        "git -C /tmp status",
        "git status",
        "cargo test",
        "npm test",
        "make",
    ] {
        assert_eq!(pinned_line(line, &root, REPO), None, "{line}");
    }
}

#[test]
fn config_hook_and_attribute_tricks_make_the_checkout_untrusted() {
    let (_t, root) = workspace();
    let app = root.join("app");
    let line = "git -C app status";
    assert!(command(line, &root).is_some());
    for (key, value) in [
        ("core.sshCommand", "touch ran"),
        ("core.hooksPath", "hooks"),
        ("core.fsmonitor", "touch ran"),
        ("core.pager", "sh -c 'touch ran'"),
        ("alias.status", "!touch ran"),
        ("credential.helper", "!touch ran"),
        ("diff.external", "touch"),
        ("filter.x.clean", "touch ran"),
        ("include.path", "/tmp/x.cfg"),
        ("core.worktree", "/tmp"),
    ] {
        git(&app, &["config", key, value]);
        assert_eq!(pinned_line(line, &root, REPO), None, "{key}");
        git(&app, &["config", "--unset", key]);
    }
    std::fs::write(app.join(".git/info/attributes"), "* filter=x\n").unwrap();
    assert_eq!(pinned_line(line, &root, REPO), None, "info/attributes");
    std::fs::remove_file(app.join(".git/info/attributes")).unwrap();
    std::fs::write(app.join(".gitmodules"), "[submodule \"x\"]\n").unwrap();
    assert_eq!(pinned_line(line, &root, REPO), None, ".gitmodules");
    std::fs::remove_file(app.join(".gitmodules")).unwrap();
    assert!(command(line, &root).is_some(), "back to a plain checkout");
}

/// A repository hook and a worktree `.gitattributes` filter never run from
/// a pinned step, even when written after the check.
#[test]
fn pinned_steps_run_no_hook_or_filter() {
    let (_t, root) = workspace();
    let app = root.join("app");
    let add = command("git -C app add README.md", &root).unwrap();
    let commit = command("git -C app commit -m 'docs: fix'", &root).unwrap();
    let marker = root.join("ran");
    for hook in ["pre-commit", "commit-msg", "post-commit", "prepare-commit-msg"] {
        let path = app.join(".git/hooks").join(hook);
        std::fs::write(&path, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    std::fs::write(app.join(".gitattributes"), "*.md filter=x\n").unwrap();
    git(&app, &["config", "filter.x.clean", &format!("touch {}", marker.display())]);
    std::fs::write(app.join("README.md"), "changed\n").unwrap();
    agent_runs(&add);
    agent_runs(&commit);
    assert!(!marker.exists(), "a hook or filter ran");
    assert_eq!(git(&app, &["log", "-1", "--format=%s"]), "docs: fix");
}

#[test]
fn a_checkout_with_another_repositorys_remote_or_a_linked_git_dir_asks() {
    let (_t, root) = workspace();
    let app = root.join("app");
    git(&app, &["remote", "add", "fork", "https://github.com/mallory/app.git"]);
    assert_eq!(pinned_line("git -C app status", &root, REPO), None, "every remote must be the watched repository");
    git(&app, &["remote", "remove", "fork"]);
    git(&app, &["config", "remote.origin.pushurl", "https://github.com/mallory/app.git"]);
    assert_eq!(pinned_line("git -C app status", &root, REPO), None, "push URL of another repository");
    git(&app, &["config", "--unset", "remote.origin.pushurl"]);
    assert_eq!(pinned_line("git -C app status", &root, "acme/other"), None, "another watched repository");
    std::os::unix::fs::symlink(&app, root.join("alias")).unwrap();
    assert!(pinned_line("git -C alias status", &root, REPO).is_some(), "a link inside the workspace resolves to the same checkout");
    let elsewhere = root.join("elsewhere");
    std::fs::rename(app.join(".git"), &elsewhere).unwrap();
    std::fs::write(app.join(".git"), format!("gitdir: {}\n", elsewhere.display())).unwrap();
    assert_eq!(pinned_line("git -C app status", &root, REPO), None, "a .git file");
}

#[test]
fn a_pull_is_the_hosts_and_fast_forwards_only() {
    let (_t, root) = workspace();
    let app = root.join("app");
    git(&app, &["branch", "--set-upstream-to=origin/main"]);
    let pull = |line| pinned_line(line, &root, REPO);
    let expected = Some(TrustedStep::Host(super::super::host_git::HostStep::Pull { dir: app.clone(), remote: "origin".into(), branch: "main".into(), repo: REPO.into() }));
    assert_eq!(pull("git -C app pull"), expected);
    assert_eq!(pull("git -C app pull --ff-only origin"), expected);
    assert_eq!(pull("git -C app pull origin main"), expected);
    for asks in ["git -C app pull --rebase", "git -C app pull https://github.com/acme/app.git main", "git -C app pull fork main", "git -C app pull origin main:x"] {
        assert_eq!(pull(asks), None, "{asks}");
    }
}

/// The own-watch block says what push freely covers; each git shape it
/// names, filled in, is one trust answers (guards the prompt against drift).
#[test]
fn the_watch_block_names_the_trusted_shapes() {
    let (_t, root) = workspace();
    let text = crate::watch_info::PUSH_FREELY_STEPS;
    for sub in ["fetch", "pull", "status", "diff", "log", "show", "branch", "add", "commit -m", "restore", "checkout -b", "push -u origin"] {
        assert!(text.contains(sub), "{sub}");
    }
    git(&root.join("app"), &["branch", "--set-upstream-to=origin/main"]);
    git(&root.join("app"), &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
    for line in [
        "git -C app fetch", "git -C app pull", "git -C app status", "git -C app diff", "git -C app log", "git -C app show",
        "git -C app branch", "git -C app add README.md", "git -C app commit -m x", "git -C app restore README.md",
        "git -C app checkout -b fix/28 origin/main", "git -C app push -u origin fix/27",
    ] {
        assert!(pinned_line(line, &root, REPO).is_some(), "{line}");
    }
    let block = crate::watch_info::render_own(&[crate::watch_info::WatchEntry {
        repo: REPO.into(), mode: "all", room_id: "!r:x".into(), active: true, push_freely: true, merge_rule: String::new(),
    }], "!r:x", "@o:x");
    assert!(block.contains(text));
    let off = crate::watch_info::render_own(&[], "!r:x", "@o:x");
    assert!(!off.contains(text), "said only when the switch is on");
}
