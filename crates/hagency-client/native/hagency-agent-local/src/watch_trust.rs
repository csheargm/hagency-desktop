//! "Let this Agent push branches and open pull requests here without asking":
//! an owner opt-in stored with the Agent's issue watch (this device's ledger,
//! written only through the owner API; nothing the Agent runs can set it).
//!
//! While the watch is on, the switch is on, the turn runs in the watch's Room
//! for the owner (the issue-watch notice is the owner's), the call is a
//! foreground Claude `Bash` call, and the WHOLE line is one publish step, the
//! call runs without a card. Fail-closed by design (GPT review, Nov 15): no
//! chaining (an earlier step could switch branch or edit config), no reliance
//! on the shell's persistent directory, and no Codex (its approvals cannot
//! rewrite the command, see [`pinned`]). Steps:
//! - `git -C <checkout> push [-u] <remote> <branch>`: a local non-default
//!   branch named explicitly, from a checkout inside the Room workspace whose
//!   remote pushes to the watched repository (see [`git`]). The HOST pushes
//!   it ([`host_git`]), so no checkout config is read for the network step;
//! - `git [-C <checkout>] fetch [-q] [<remote>]` from the watched repository,
//!   also run by the host, and `git [-C <checkout>] checkout -b <new>
//!   <remote>/<default>`, pinned (the fix steps of an issue, see [`git`]);
//! - `gh pr create|ready|edit` naming the watched repository (see [`gh`]);
//! - Write/Edit/MultiEdit of a file inside the workspace, never git metadata.
//!
//! Everything else keeps asking. Merges stay with `merge_policy`.
use crate::{ToolProposal, outside::shell};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub(crate) mod gh;
pub(crate) mod git;
pub mod host_git;
pub mod host_run;

/// How push trust answers a tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustedStep {
    /// Run unchanged (a file edit inside the workspace).
    Allow,
    /// Run as this pinned Bash command.
    Command(String),
    /// The host runs this step; the call then only reports what it did.
    Host(host_git::HostStep),
}

/// The repositories `proposal`'s Agent may publish to from this proposal's
/// Room: the legacy issue watch's, and each work-list entry whose owner set
/// "push freely" for this Room.
fn trusted_repos(db: &Connection, proposal: &ToolProposal) -> crate::Result<Vec<String>> {
    let (agent, room) = (&proposal.scope.agent, &proposal.scope.room);
    let row: Option<(String, String, String, i64)> = db
        .query_row("SELECT mode,repo,room_id,push_trusted FROM issue_watch WHERE agent=?", [agent], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let mut repos: Vec<String> = row.and_then(|(mode, repo, r, trusted)| (mode != "off" && trusted == 1 && &r == room).then_some(repo)).into_iter().collect();
    let mut work = db.prepare("SELECT rules FROM agent_work WHERE agent=? ORDER BY repo")?;
    for rules in work.query_map([agent], |r| r.get::<_, String>(0))? {
        // A row that no longer parses grants nothing.
        let Ok(rules) = serde_json::from_str::<crate::merge_policy::work::OwnerRules>(&rules?) else { continue };
        if rules.push_freely && &rules.room_id == room && !repos.iter().any(|r| r.eq_ignore_ascii_case(&rules.repo)) {
            repos.push(rules.repo);
        }
    }
    Ok(repos)
}

/// A check, run off the async threads, that push trust still covers
/// `proposal`: a fresh read-only connection to the ledger at `path` each
/// time; no path or an unreadable ledger is a no.
pub(crate) fn recheck(path: Option<std::path::PathBuf>, proposal: ToolProposal) -> Box<dyn Fn() -> bool + Send + Sync> {
    Box::new(move || {
        let Some(path) = &path else { return false };
        let Ok(db) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false };
        trusted_repos(&db, &proposal).is_ok_and(|r| !r.is_empty())
    })
}

/// How push trust answers `proposal`, if it does. A command reaches Claude's
/// permission reply as `updatedInput`, so what runs is exactly what was
/// checked; Codex cannot rewrite its command, so it is never trusted.
pub(crate) fn pinned(db: &Connection, proposal: &ToolProposal, owner: &str) -> crate::Result<Option<TrustedStep>> {
    let edit = matches!(proposal.tool.as_str(), "claude.Write" | "claude.Edit" | "claude.MultiEdit");
    if proposal.risk == "permission_expansion"
        || !(edit || proposal.tool == "claude.Bash")
        || proposal.scope.requester != owner
        // A background call's launch result says nothing about what it did.
        || proposal.arguments["run_in_background"] == true
    {
        return Ok(None);
    }
    let root = Path::new(&proposal.canonical_directory);
    if edit {
        let inside = crate::outside::writable_in(proposal.arguments.get("file_path"), root);
        return Ok((inside && !trusted_repos(db, proposal)?.is_empty()).then_some(TrustedStep::Allow));
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(None) };
    Ok(trusted_repos(db, proposal)?.iter().find_map(|repo| pinned_line(line, root, repo)))
}

/// How `line` is answered when it is a single trusted step for `repo` from
/// inside `root`, else `None`.
pub(crate) fn pinned_line(line: &str, root: &Path, repo: &str) -> Option<TrustedStep> {
    let parsed = shell::parse(line)?;
    let [words] = parsed.segments.as_slice() else { return None };
    if parsed.heredoc_at.is_some() {
        return None;
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["git", "-C", dir, "push", rest @ ..] => {
            let dir = checkout(root, dir)?;
            let push = git::Push::parse(rest)?;
            (crate::outside::git_config::local_config_safe(&dir) && push.allowed_in(&dir, repo)).then(|| TrustedStep::Host(push.host_step(&dir, repo)))
        }
        ["git", "-C", dir, rest @ ..] => git::fix_step(&checkout(root, dir)?, rest, repo),
        ["git", rest @ ..] => git::fix_step(&toplevel(root)?, rest, repo),
        ["gh", "pr", verb, rest @ ..] => gh::PrCommand::parse(verb, rest)?.allowed(root, repo).then(|| TrustedStep::Command(pin_env(line))),
        _ => None,
    }
}

/// `root` itself when it is the top of a git checkout (a bare `git fetch`
/// is then answered for `root`, never for the shell's later directory).
fn toplevel(root: &Path) -> Option<std::path::PathBuf> {
    let root = root.canonicalize().ok()?;
    let top = git::query(&root, &["rev-parse", "--show-toplevel"])?;
    (Path::new(&top).canonicalize().ok()? == root).then_some(root)
}

/// `command` run with git's hooks and helpers off, whatever the checkout's
/// config says (`gh` runs git in the shell's directory).
pub(crate) fn pin_env(command: &str) -> String {
    let env: Vec<String> = crate::agent_env::GIT_HELPERS_OFF_ENV.iter().map(|(k, v)| format!("{k}={}", quote(v))).collect();
    format!("{} {command}", env.join(" "))
}

/// `word` in single quotes, safe for any POSIX shell.
pub(crate) fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// `dir` (absolute, or relative to `root`) resolved to a directory inside `root`.
fn checkout(root: &Path, dir: &str) -> Option<std::path::PathBuf> {
    let root = root.canonicalize().ok()?;
    let dir = root.join(dir).canonicalize().ok()?;
    dir.starts_with(&root).then_some(dir)
}

/// `owner/name` of a github.com remote URL (https, ssh or scp form).
pub(crate) fn github_repo_of(url: &str) -> Option<&str> {
    let rest = ["https://github.com/", "ssh://git@github.com/", "git@github.com:"].iter().find_map(|p| url.strip_prefix(p))?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    crate::merge_policy::gate::repo_name(rest)
}

#[cfg(test)]
mod tests;
