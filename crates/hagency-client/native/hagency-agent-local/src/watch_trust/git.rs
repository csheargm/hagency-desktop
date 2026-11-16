//! The `git push` shape push trust allows, and the repository facts it is
//! checked against (read with plain `git` queries that run no helper).
use crate::agent_env::GIT_HELPERS_OFF_ENV;
use std::path::Path;

/// Push options that change nothing about what is pushed or where.
const PUSH_FLAGS: [&str; 4] = ["-u", "--set-upstream", "-q", "--quiet"];
/// Branches that are never pushed under trust, whatever the remote says.
pub(super) const PROTECTED: [&str; 2] = ["main", "master"];

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

    /// The step the host runs in its place (see [`super::host_git`]).
    pub(crate) fn host_step(&self, dir: &Path, repo: &str) -> super::host_git::HostStep {
        super::host_git::HostStep::Push { dir: dir.to_path_buf(), remote: self.remote.into(), branch: self.branch.into(), repo: repo.into(), upstream: self.upstream }
    }
}

/// A plain remote name (never a URL or path).
pub(super) fn remote_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with(['-', '.']) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A branch name with no refspec syntax: no `:` (delete or rename), no `+`
/// (force), no `refs/` prefix (tags, other namespaces), no `..` or `@{`.
pub(super) fn branch_name(name: &str) -> bool {
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
pub(super) fn default_branch(dir: &Path, remote: &str) -> Option<String> {
    let head = query(dir, &["symbolic-ref", "--short", "-q", &format!("refs/remotes/{remote}/HEAD")])?;
    head.strip_prefix(&format!("{remote}/")).map(str::to_owned)
}

/// Every push URL (`pushurl`s, else `url`, after `insteadOf` rewriting) is `repo`.
fn push_urls_are(dir: &Path, remote: &str, repo: &str) -> bool {
    urls_are(dir, &["remote", "get-url", "--push", "--all", remote], repo)
}

/// Every fetch URL of `remote` (after `insteadOf` rewriting) is `repo`.
pub(super) fn fetch_urls_are(dir: &Path, remote: &str, repo: &str) -> bool {
    urls_are(dir, &["remote", "get-url", "--all", remote], repo)
}

fn urls_are(dir: &Path, args: &[&str], repo: &str) -> bool {
    query(dir, args).is_some_and(|urls| {
        let urls: Vec<&str> = urls.lines().collect();
        !urls.is_empty() && urls.iter().all(|url| super::github_repo_of(url).is_some_and(|r| r.eq_ignore_ascii_case(repo)))
    })
}

/// Trimmed stdout of a successful read-only `git -C dir ...`; `None` on failure.
// A failed query is an answer ("cannot tell"), which makes the command ask.
pub(super) fn query(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(GIT_HELPERS_OFF_ENV).env("GIT_TERMINAL_PROMPT", "0").env("GIT_NO_LAZY_FETCH", "1").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Git's built-in empty tree: attributes read from it select no filter.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The fix steps of an issue in checkout `dir`: `fetch [-q] [<remote>]` from
/// the watched repository (run by the host), and `checkout -b <new>
/// <remote>/<default>` (or `switch -c`), which needs no network and runs as
/// the Agent, pinned so no filter driver, hook or lazy fetch can run.
pub(crate) fn fix_step(dir: &Path, rest: &[&str], repo: &str) -> Option<super::TrustedStep> {
    use super::{TrustedStep, host_git::HostStep};
    let remote = match rest {
        ["fetch"] | ["fetch", "-q" | "--quiet"] => "origin",
        ["fetch", remote] | ["fetch", "-q" | "--quiet", remote] | ["fetch", remote, "-q" | "--quiet"] => remote,
        ["checkout", "-b", name, start] | ["switch", "-c", name, start] => return new_branch(dir, name, start, repo).map(TrustedStep::Command),
        _ => return None,
    };
    (remote_name(remote) && fetch_urls_are(dir, remote, repo)).then(|| TrustedStep::Host(HostStep::Fetch { dir: dir.to_path_buf(), remote: remote.into(), repo: repo.into() }))
}

/// The pinned `checkout -b`: attributes come from the empty tree (so a
/// worktree `.gitattributes` selects no filter), the attributes file and
/// hooks are off, and the checkout's config must hold data keys only, with
/// no `info/attributes` (GPT review, Nov 16).
fn new_branch(dir: &Path, name: &str, start: &str, repo: &str) -> Option<String> {
    let (remote, default) = start.split_once('/')?;
    let fresh = branch_name(name) && !PROTECTED.contains(&name) && query(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")]).is_none();
    let from_default = remote_name(remote) && default_branch(dir, remote).is_some_and(|d| d == default) && fetch_urls_are(dir, remote, repo);
    let attributes = query(dir, &["rev-parse", "--path-format=absolute", "--git-path", "info/attributes"])?;
    if !(fresh && from_default) || Path::new(&attributes).symlink_metadata().is_ok() || !crate::outside::git_config::local_config_safe(dir) {
        return None;
    }
    let pins = ["core.attributesFile=/dev/null", "core.hooksPath=/dev/null", "core.fsmonitor=false"];
    let mut words = vec!["git".to_owned(), "-C".to_owned(), dir.to_string_lossy().into_owned()];
    for pin in pins {
        words.extend(["-c".to_owned(), pin.to_owned()]);
    }
    words.extend(["checkout", "-q", "--no-track", "-b", name, &format!("refs/remotes/{start}")].map(str::to_owned));
    let line = words.iter().map(|w| super::quote(w)).collect::<Vec<_>>().join(" ");
    Some(super::pin_env(&format!("GIT_ATTR_SOURCE={EMPTY_TREE} GIT_NO_LAZY_FETCH=1 {line}")))
}
