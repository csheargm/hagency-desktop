use super::tests::proposal;
use super::stays_in_workspace;
use serde_json::json;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}");
}
fn allowed(line: &str, dir: &Path) -> bool {
    stays_in_workspace(&proposal("claude.Bash", json!({"command": line}), dir, "high"))
}

/// GPT review (Nov 15): a repo-local diff/textconv/filter/pager/ssh helper
/// would run from an auto-allowed git read. Each one now makes git ask.
#[test]
fn a_local_helper_in_git_config_makes_every_git_command_ask() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().canonicalize().unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    git(&dir, &["config", "branch.main.remote", "origin"]);
    assert!(allowed("git status && git diff && git log -1", &dir), "a plain clone stays allowed");
    let marker = dir.join("ran");
    let helper = format!("touch {}", marker.display());
    for (key, value) in [
        ("diff.external", helper.as_str()),
        ("diff.x.textconv", helper.as_str()),
        ("filter.x.clean", helper.as_str()),
        ("filter.x.process", helper.as_str()),
        ("core.pager", helper.as_str()),
        ("core.sshCommand", helper.as_str()),
        ("credential.helper", helper.as_str()),
        ("include.path", "/tmp/evil.gitconfig"),
        ("remote.origin.uploadpack", helper.as_str()),
        ("CORE.Pager", helper.as_str()),
    ] {
        git(&dir, &["config", key, value]);
        for line in ["git diff", "git log -p", "git show HEAD", "git add -A", "git commit -m x", "git status"] {
            assert!(!allowed(line, &dir), "{key}: {line}");
        }
        git(&dir, &["config", "--unset-all", key]);
    }
    assert!(allowed("git diff", &dir));
    assert!(!marker.exists(), "classification itself never runs a helper");
}

/// GPT review (Nov 15): `link/../x` was checked as `<cwd>/x` while the OS
/// opens the real parent of `link`'s target.
#[cfg(unix)]
#[test]
fn a_symlink_then_dotdot_cannot_escape() {
    let temp = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir(elsewhere.path().join("inner")).unwrap();
    std::fs::write(elsewhere.path().join("secret"), "x").unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("inner"), temp.path().join("link")).unwrap();
    std::fs::write(temp.path().join("secret"), "decoy").unwrap();
    assert!(!allowed("cat link/../secret", temp.path()));
    assert!(!allowed("sort link/../secret", temp.path()));
    assert!(allowed("cat secret", temp.path()));
}

/// GPT re-review: a `.git` file pointing at another git directory, and
/// per-command `-c`/`--git-dir` overrides, cannot smuggle a helper in.
#[test]
fn a_gitdir_file_or_command_line_override_still_asks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let evil = root.join("evil");
    std::fs::create_dir(&evil).unwrap();
    git(&evil, &["init", "-q"]);
    git(&evil, &["config", "core.pager", "touch ran"]);
    let wt = root.join("wt");
    std::fs::create_dir(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", evil.join(".git").display())).unwrap();
    assert!(!allowed("cd wt && git log -1", &root));
    let clean = root.join("clean");
    std::fs::create_dir(&clean).unwrap();
    git(&clean, &["init", "-q"]);
    for line in ["cd clean && git -c core.pager=x log", "cd clean && git --git-dir=../evil/.git log", "cd clean && env GIT_DIR=../evil/.git git log"] {
        assert!(!allowed(line, &root), "{line}");
    }
    assert!(allowed("cd clean && git log", &root));
}
