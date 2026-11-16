//! The `git push` shape push trust allows, and the repository facts it is
//! checked against (read with plain `git` queries that run no helper).
use crate::agent_env::GIT_HELPERS_OFF_ENV;
use std::path::Path;

/// Push options that change nothing about what is pushed or where.
const PUSH_FLAGS: [&str; 4] = ["-u", "--set-upstream", "-q", "--quiet"];
/// Branches that are never pushed under trust, whatever the remote says.
const PROTECTED: [&str; 2] = ["main", "master"];
/// `push.default` values that push only the current branch to its same-named
/// remote branch (`matching` pushes every branch; `upstream` may target main).
const SAFE_PUSH_DEFAULTS: [&str; 3] = ["", "simple", "current"];

/// `git push [flags] [<remote> [<branch>]]`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Push<'a> {
    remote: Option<&'a str>,
    branch: Option<&'a str>,
}

impl<'a> Push<'a> {
    pub(crate) fn parse(rest: &[&'a str]) -> Option<Self> {
        let mut positional = Vec::new();
        for word in rest {
            if word.starts_with('-') {
                if !PUSH_FLAGS.contains(word) {
                    return None;
                }
            } else {
                positional.push(*word);
            }
        }
        let (remote, branch) = match positional.as_slice() {
            [] => (None, None),
            [remote] => (Some(*remote), None),
            [remote, branch] => (Some(*remote), Some(*branch)),
            _ => return None,
        };
        (remote.is_none_or(remote_name) && branch.is_none_or(branch_name)).then_some(Self { remote, branch })
    }

    /// The branch it pushes is a local, non-default branch, and every push URL
    /// of the remote it pushes to is `repo` on github.com.
    pub(crate) fn allowed_in(&self, dir: &Path, repo: &str) -> bool {
        self.target(dir).is_some_and(|(remote, branch)| {
            !PROTECTED.contains(&branch.as_str())
                && default_branch(dir, &remote).is_none_or(|d| d != branch)
                && query(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_some()
                && query(dir, &["config", "--bool", &format!("remote.{remote}.mirror")]).is_none_or(|m| m != "true")
                && push_urls_are(dir, &remote, repo)
        })
    }

    /// `(remote, branch)` git will push, or `None` when config could widen it.
    fn target(&self, dir: &Path) -> Option<(String, String)> {
        let current = || query(dir, &["symbolic-ref", "--short", "-q", "HEAD"]);
        let branch = match self.branch {
            Some("HEAD") | None => current()?,
            Some(name) => name.to_owned(),
        };
        if self.branch.is_none() {
            // No refspec: push.default and remote.<name>.push decide what goes.
            let mode = query(dir, &["config", "push.default"]).unwrap_or_default();
            if !SAFE_PUSH_DEFAULTS.contains(&mode.as_str()) {
                return None;
            }
        }
        let remote = match self.remote {
            Some(remote) => remote.to_owned(),
            None => query(dir, &["for-each-ref", "--format=%(push:remotename)", &format!("refs/heads/{branch}")]).filter(|r| !r.is_empty())?,
        };
        if self.branch.is_none() && query(dir, &["config", "--get-all", &format!("remote.{remote}.push")]).is_some() {
            return None;
        }
        Some((remote, branch))
    }
}

/// A plain remote name (never a URL or path).
fn remote_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with(['-', '.']) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A branch name with no refspec syntax: no `:` (delete or rename), no `+`
/// (force), no `refs/` prefix (tags, other namespaces), no `..` or `@{`.
fn branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && !name.contains("//")
        && !name.ends_with(".lock")
        && !name.starts_with("refs/")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

/// The branch `refs/remotes/<remote>/HEAD` points at, when known.
fn default_branch(dir: &Path, remote: &str) -> Option<String> {
    let head = query(dir, &["symbolic-ref", "--short", "-q", &format!("refs/remotes/{remote}/HEAD")])?;
    head.strip_prefix(&format!("{remote}/")).map(str::to_owned)
}

/// Every push URL (`pushurl`s, else `url`, after `insteadOf` rewriting) is `repo`.
fn push_urls_are(dir: &Path, remote: &str, repo: &str) -> bool {
    query(dir, &["remote", "get-url", "--push", "--all", remote]).is_some_and(|urls| {
        let urls: Vec<&str> = urls.lines().collect();
        !urls.is_empty() && urls.iter().all(|url| super::github_repo_of(url).is_some_and(|r| r.eq_ignore_ascii_case(repo)))
    })
}

/// Every fetch and push URL of every remote in `dir` is `repo` (so whichever
/// remote `gh` picks as the base repository, it is `repo`).
pub(crate) fn all_remotes_are(dir: &Path, repo: &str) -> bool {
    let Some(remotes) = query(dir, &["remote"]) else { return false };
    let remotes: Vec<&str> = remotes.lines().collect();
    !remotes.is_empty()
        && remotes.iter().all(|remote| {
            push_urls_are(dir, remote, repo)
                && query(dir, &["remote", "get-url", "--all", remote])
                    .is_some_and(|urls| urls.lines().all(|url| super::github_repo_of(url).is_some_and(|r| r.eq_ignore_ascii_case(repo))))
        })
}

/// Trimmed stdout of a successful read-only `git -C dir ...`; `None` on failure.
// A failed query is an answer ("cannot tell"), which makes the command ask.
fn query(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(GIT_HELPERS_OFF_ENV).env("GIT_TERMINAL_PROMPT", "0").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
