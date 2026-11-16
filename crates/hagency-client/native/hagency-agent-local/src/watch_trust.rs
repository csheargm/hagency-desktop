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
//! rewrite the command, see [`pinned`]). Publish steps:
//! - `git -C <checkout> push [-u] <remote> <branch>`: a local non-default
//!   branch named explicitly, from a checkout inside the Room workspace whose
//!   remote pushes to the watched repository (see [`git`]). It runs rewritten
//!   (see [`git::Push::pinned`]) so config written after this check cannot
//!   change where it goes;
//! - `gh pr create|ready|edit` naming the watched repository (see [`gh`]).
//!
//! Everything else keeps asking. Merges stay with `merge_policy`.
use crate::{ToolProposal, outside::shell};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub(crate) mod gh;
pub(crate) mod git;

/// The watched repository when `proposal`'s Agent trusts publishing there
/// from this proposal's Room.
fn trusted_repo(db: &Connection, proposal: &ToolProposal) -> crate::Result<Option<String>> {
    let row: Option<(String, String, String, i64)> = db
        .query_row("SELECT mode,repo,room_id,push_trusted FROM issue_watch WHERE agent=?", [&proposal.scope.agent], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    Ok(row.and_then(|(mode, repo, room, trusted)| (mode != "off" && trusted == 1 && room == proposal.scope.room).then_some(repo)))
}

/// True when the owner's push trust answers `proposal` without a card.
pub(crate) fn allows(db: &Connection, proposal: &ToolProposal, owner: &str) -> crate::Result<bool> {
    Ok(pinned(db, proposal, owner)?.is_some())
}

/// The command that must run in place of `proposal`'s when push trust answers
/// it. The Claude permission reply carries it as `updatedInput`, so what runs
/// is exactly what was checked; Codex cannot do that, so it is never trusted.
pub(crate) fn pinned(db: &Connection, proposal: &ToolProposal, owner: &str) -> crate::Result<Option<String>> {
    if proposal.risk == "permission_expansion"
        || proposal.tool != "claude.Bash"
        || proposal.scope.requester != owner
        // A background call's launch result says nothing about what it did.
        || proposal.arguments["run_in_background"] == true
    {
        return Ok(None);
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(None) };
    let Some(repo) = trusted_repo(db, proposal)? else { return Ok(None) };
    Ok(pinned_line(line, Path::new(&proposal.canonical_directory), &repo))
}

/// `line` as it must run when it is a single publish step for `repo` from
/// inside `root`, else `None`.
pub(crate) fn pinned_line(line: &str, root: &Path, repo: &str) -> Option<String> {
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
            (crate::outside::git_config::local_config_safe(&dir) && push.allowed_in(&dir, repo)).then(|| push.pinned(&dir, repo))
        }
        ["gh", "pr", verb, rest @ ..] => gh::PrCommand::parse(verb, rest)?.allowed(root, repo).then(|| line.to_owned()),
        _ => None,
    }
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
