//! The `git push` shape push trust allows, and the repository facts it is
//! checked against (read with plain `git` queries that run no helper).
use crate::agent_env::GIT_HELPERS_OFF_ENV;
use std::path::Path;

/// Push options that change nothing about what is pushed or where.
const PUSH_FLAGS: [&str; 4] = ["-u", "--set-upstream", "-q", "--quiet"];
/// Branches that are never pushed under trust, whatever the remote says.
const PROTECTED: [&str; 2] = ["main", "master"];
/// Config the trusted push runs with, whatever the checkout's config says by
/// then: no hooks, no fsmonitor, no ssh command, no extra tags.
const PINNED_CONFIG: [&str; 4] = ["core.hooksPath=/dev/null", "core.fsmonitor=false", "core.sshCommand=false", "push.followTags=false"];

/// `git push [flags] <remote> <branch>`: both named, so neither HEAD nor
/// `push.default` decides what goes (GPT review: `git switch -c master &&
/// git push origin HEAD`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Push<'a> {
    remote: &'a str,
    branch: &'a str,
    upstream: bool,
}

impl<'a> Push<'a> {
    pub(crate) fn parse(rest: &[&'a str]) -> Option<Self> {
        let mut positional = Vec::new();
        let mut upstream = false;
        for word in rest {
            if word.starts_with('-') {
                if !PUSH_FLAGS.contains(word) {
                    return None;
                }
                upstream |= matches!(*word, "-u" | "--set-upstream");
            } else {
                positional.push(*word);
            }
        }
        let [remote, branch] = positional.as_slice() else { return None };
        (remote_name(remote) && branch_name(branch) && *branch != "HEAD").then_some(Self { remote, branch, upstream })
    }

    /// The branch is a local, non-default branch, and every push URL of the
    /// remote is `repo` on github.com.
    pub(crate) fn allowed_in(&self, dir: &Path, repo: &str) -> bool {
        !PROTECTED.contains(&self.branch)
            // An unknown default branch could be the one pushed (GPT review).
            && default_branch(dir, self.remote).is_some_and(|d| d != self.branch)
            && query(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{}", self.branch)]).is_some()
            && push_urls_are(dir, self.remote, repo)
    }

    /// The command that runs: the checked checkout, pinned config, the
    /// watched repository's URL in place of the remote name and an explicit
    /// refspec, so a config change after the check cannot redirect it.
    pub(crate) fn pinned(&self, dir: &Path, repo: &str) -> String {
        let mut words = vec!["git".to_owned(), "-C".to_owned(), dir.to_string_lossy().into_owned()];
        for pin in PINNED_CONFIG {
            words.extend(["-c".to_owned(), pin.to_owned()]);
        }
        words.push("push".to_owned());
        if self.upstream {
            words.push("--set-upstream".to_owned());
        }
        words.push(format!("https://github.com/{repo}.git"));
        words.push(format!("refs/heads/{0}:refs/heads/{0}", self.branch));
        words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
    }
}

/// `word` in single quotes, safe for any shell.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
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

/// Trimmed stdout of a successful read-only `git -C dir ...`; `None` on failure.
// A failed query is an answer ("cannot tell"), which makes the command ask.
fn query(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(GIT_HELPERS_OFF_ENV).env("GIT_TERMINAL_PROMPT", "0").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
