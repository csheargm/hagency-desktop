//! `pull` and `clone` of the watched repository under "push freely" (ADR
//! 0021). Like fetch and push (ADR 0015), the host does the network part in
//! a fresh scratch repository with no checkout config; the Agent's call then
//! applies it locally, pinned, with only the `file` transport allowed.
use super::host_git::{Dest, HostStep, Scratch, fetch_with, remote_default, say};
use super::host_run::{HostCtx, run};
use super::local_git::{pinned_git, pinned_git_file};
use std::path::{Path, PathBuf};

/// The host fetches; the Agent's call then fast-forwards the checkout's
/// current branch to the commit `<branch>` has in that fetch (never a merge
/// commit, never a stale tracking ref).
pub(super) fn pull(dir: &Path, remote: &str, branch: &str, dest: &Dest, ctx: &HostCtx) -> Result<String, String> {
    let (fetched, sha) = fetch_with(dir, remote, Some(branch), dest, ctx)?;
    let sha = sha.filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("the fetched branch has no commit")?;
    Ok(format!("{fetched} && {}", pinned_git(dir, &["merge", "--ff-only", "-q", &sha])))
}

/// The host fetches every branch of the watched repository into a scratch
/// repository whose HEAD is the remote's default branch; the Agent's call
/// clones from that local path into `target` and points `origin` at GitHub.
pub(super) fn clone(target: &Path, repo: &str, dest: &Dest, ctx: &HostCtx) -> Result<String, String> {
    let parent = target.parent().ok_or("the clone has no parent folder")?;
    let scratch = Scratch::new(None, ctx)?;
    let default = remote_default(&scratch, dest, ctx)?;
    if !super::git::branch_name(&default) {
        return Err("the remote's default branch name is not plain".into());
    }
    run(scratch.git(dest).args(["fetch", "-q", "--no-tags", "--no-write-fetch-head", &dest.url, "+refs/heads/*:refs/heads/*"]), None, ctx)?;
    run(scratch.git(dest).args(["symbolic-ref", "HEAD", &format!("refs/heads/{default}")]), None, ctx)?;
    let local = scratch.keep().to_string_lossy().into_owned();
    let target_text = target.to_string_lossy();
    let copy = pinned_git_file(parent, &["clone", "-q", "--no-hardlinks", "--template=", "--origin", "origin", "--", &local, &target_text]);
    let github = format!("https://github.com/{repo}.git");
    let point = pinned_git(target, &["remote", "set-url", "origin", &github]);
    let done = say(&format!("Cloned {repo} into {target_text} (branch {default})."));
    Ok(format!("{copy} && {point}; status=$?; rm -rf -- {}; [ $status -eq 0 ] && {done}", super::quote(&local)))
}

/// `git clone [-q] <github url of repo> [<dir>]` or `gh repo clone <repo>
/// [<dir>]` (the words after `clone`): a host clone into a new folder
/// inside `root`, else `None`.
pub(crate) fn request(root: &Path, git: bool, rest: &[&str], repo: &str) -> Option<HostStep> {
    let words: Vec<&str> = rest.iter().copied().filter(|w| !(git && matches!(*w, "-q" | "--quiet"))).collect();
    let (source, dir) = match words.as_slice() {
        [source] => (*source, None),
        [source, dir] => (*source, Some(*dir)),
        _ => return None,
    };
    let named = if git { super::github_repo_of(source) } else { crate::merge_policy::gate::repo_name(source) };
    if !named.is_some_and(|n| n.eq_ignore_ascii_case(repo)) || source.starts_with('-') || dir.is_some_and(|d| d.starts_with('-')) {
        return None;
    }
    let name = repo.split_once('/')?.1;
    Some(HostStep::Clone { target: target(root, dir.unwrap_or(name))?, repo: repo.into() })
}

/// A path that does not exist yet (not even as a link) in an existing,
/// real folder inside `root`.
fn target(root: &Path, dir: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let path = crate::outside::shell::lexical_join(&root, dir);
    let (parent, name) = (path.parent()?.canonicalize().ok()?, path.file_name()?);
    let plain = name.to_str().is_some_and(|n| !n.starts_with('.') && n.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')));
    let target = parent.join(name);
    // Never inside git metadata (`<checkout>/.git/...`), named or resolved
    // (GPT review, Nov 17).
    let metadata = crate::outside::is_git_metadata(&path) || crate::outside::is_git_metadata(&target);
    (plain && !metadata && parent.starts_with(&root) && target.symlink_metadata().is_err()).then_some(target)
}

#[cfg(test)]
#[path = "host_clone_tests.rs"]
mod tests;
