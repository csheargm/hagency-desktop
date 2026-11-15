//! Bypasses found in review of outside mode: each must ask.
use super::*;
use serde_json::json;

fn allowed(tool: &str, arguments: Value, dir: &Path) -> bool {
    let mut p = tests::proposal(tool, arguments, dir, "high");
    p.risk = "high".into();
    stays_in_workspace(&p)
}

#[test]
fn writes_to_git_metadata_ask() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("repo/.git/hooks")).unwrap();
    for path in [".git/config", "repo/.git/config", "repo/.git/hooks/pre-commit", ".gitattributes", "repo/.gitattributes", ".gitmodules", "sub/.gitconfig", "repo/.GIT/config", "x/../.git/info/attributes"] {
        for tool in ["claude.Write", "claude.Edit", "claude.MultiEdit"] {
            assert!(!allowed(tool, json!({"file_path":path}), dir.path()), "{tool} {path}");
        }
        assert!(!allowed("claude.NotebookEdit", json!({"notebook_path":path}), dir.path()), "{path}");
    }
    assert!(allowed("claude.Edit", json!({"file_path":".github/workflows/ci.yml"}), dir.path()));
    assert!(allowed("claude.Read", json!({"file_path":"repo/.git/config"}), dir.path()), "reading stays allowed");
}

#[cfg(unix)]
#[test]
fn a_symlink_into_git_metadata_asks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config"), "").unwrap();
    std::os::unix::fs::symlink(dir.path().join(".git/config"), dir.path().join("notes.txt")).unwrap();
    assert!(!allowed("claude.Write", json!({"file_path":"notes.txt"}), dir.path()));
}

#[test]
fn git_helpers_and_relocation_options_ask() {
    let dir = tempfile::tempdir().unwrap();
    for line in [
        "git diff --ext-diff", "git log -p --ext-diff", "git show --textconv HEAD", "git diff --textconv",
        "git -c core.fsmonitor=x status", "git -C /tmp log", "git --git-dir=/tmp/x log", "git --work-tree=/tmp status",
        "git --exec-path=/tmp log", "git --config-env=core.pager=X log", "git diff --output=/tmp/x", "git log --output /tmp/x",
        "git status --git-dir /tmp", "git diff --no-index /etc/passwd README.md",
    ] {
        assert!(!allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
}

#[test]
fn git_branch_may_only_list() {
    let dir = tempfile::tempdir().unwrap();
    for line in ["git branch", "git branch -a", "git branch -r", "git branch -vv", "git branch --list", "git branch --show-current"] {
        assert!(allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
    for line in ["git branch x", "git branch -d main", "git branch -D main", "git branch -m new", "git branch -M a b", "git branch --set-upstream-to=origin/x",
        "git branch -u origin/x", "git branch --unset-upstream", "git branch -c a b", "git branch --edit-description", "git branch --list x", "git branch -f main HEAD"] {
        assert!(!allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
}

#[test]
fn option_values_that_are_paths_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    for line in [
        "rg --ignore-file=/etc/passwd x .", "grep --exclude-from=/etc/passwd x .", "grep -r --file=/etc/passwd .", "grep -f /etc/passwd .",
        "grep -f/etc/passwd .", "rg -f ../secret x", "rg --ignore-file ../x y", "grep --include=../../x y .", "rg --ignore-file=~/x y",
        "rg --pre=./run x", "rg --pre ./run x", "rg -z x", "git log --format=x --output=../x", "cat --x=/etc/passwd",
        "git add --pathspec-from-file=../list",
    ] {
        assert!(!allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
    for line in ["rg --ignore-file=.rgignore x .", "grep --exclude-from=./skip -r x .", "rg --type=rust todo", "git log --format=%h -3"] {
        assert!(allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
}

#[test]
fn gh_reads_stay_on_github_com_and_out_of_the_browser() {
    let dir = tempfile::tempdir().unwrap();
    for line in ["gh issue view 1 --repo=OWNER/REPO", "gh pr list --repo csheargm/hagency", "gh issue view 6 -R a.b/c-d", "gh auth status"] {
        assert!(allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
    for line in [
        "gh auth status --hostname=evil", "gh auth status --hostname evil", "gh auth status -h evil", "gh issue view 1 --hostname=evil",
        "gh issue view 1 --repo=evil.example/o/r", "gh pr list -R evil.example/o/r", "gh pr list --repo", "gh issue list -R ../x",
        "gh issue view 1 --web", "gh issue view 1 --web=true", "gh pr view -w", "gh issue view 1 --repo=/etc/passwd",
    ] {
        assert!(!allowed("claude.Bash", json!({"command":line}), dir.path()), "{line}");
    }
}
