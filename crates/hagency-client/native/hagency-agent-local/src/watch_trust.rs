//! "Let this Agent push branches and open pull requests here without asking":
//! an owner opt-in stored with the Agent's issue watch (this device's ledger,
//! written only through the owner API; nothing the Agent runs can set it).
//!
//! While the watch is on, the switch is on, and the turn runs in the watch's
//! Room, an Ask-owner shell line is allowed without a card when every segment is
//! either a trusted publish step for the watched repository or a step outside
//! mode would allow anyway (only when the Agent is in outside mode), and at
//! least one segment is a publish step. Publish steps:
//! - `git push [-u|--set-upstream|-q] [<remote> [<branch>|HEAD]]` of a local
//!   non-default branch to a remote whose every push URL is the watched
//!   repository on github.com (see [`git`]);
//! - `gh pr create`, `gh pr ready`, `gh pr edit` aimed at the watched
//!   repository (see [`gh`]).
//!
//! Everything else (force, delete, mirror, tags, refspecs with `:`, pushes to
//! main/master/the remote's default branch, other repositories, other Rooms,
//! `--web`, merges) keeps asking. Merges stay with `merge_policy`.
use crate::{ToolProposal, outside, outside::shell};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

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
/// `outside_mode`: the Agent's approval mode lets in-workspace steps run.
pub(crate) fn allows(db: &Connection, proposal: &ToolProposal, outside_mode: bool) -> crate::Result<bool> {
    if proposal.risk == "permission_expansion" || !matches!(proposal.tool.as_str(), "claude.Bash" | "codex.command") {
        return Ok(false);
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(false) };
    let Some(repo) = trusted_repo(db, proposal)? else { return Ok(false) };
    Ok(line_allowed(line, Path::new(&proposal.canonical_directory), &repo, outside_mode))
}

/// Every segment is a publish step for `repo` (checked from every directory
/// the line may be in) or, in outside mode, an in-workspace step; at least one
/// is a publish step.
pub(crate) fn line_allowed(line: &str, root: &Path, repo: &str, outside_mode: bool) -> bool {
    let Some(parsed) = shell::parse(line) else { return false };
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    // While every join so far is `&&`, a failed `cd` stops the line, so the
    // line can only be in the last `cd`'s directory.
    let mut exact = Some(root.to_path_buf());
    let mut published = false;
    for (index, words) in parsed.segments.iter().enumerate() {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        if parsed.heredoc_at == Some(index) && !outside::is_commit_from_stdin(&words) {
            return false;
        }
        if index > 0 && !parsed.after_and[index] {
            exact = None;
        }
        let here: Vec<PathBuf> = exact.clone().map_or_else(|| dirs.clone(), |dir| vec![dir]);
        let ok = match words.as_slice() {
            ["cd", target] => {
                exact = exact.map(|dir| shell::lexical_join(&dir, target));
                outside::enter_dir(target, &mut dirs, root)
            }
            ["git", "push", ..] | ["gh", "pr", ..] if is_publish(&words, &here, root, repo) => {
                published = true;
                true
            }
            _ => outside_mode && outside::segment_in_workspace(&words, &dirs, root),
        };
        if !ok {
            return false;
        }
    }
    published
}

fn is_publish(words: &[&str], dirs: &[PathBuf], root: &Path, repo: &str) -> bool {
    match words {
        ["git", "push", rest @ ..] => git::Push::parse(rest).is_some_and(|push| dirs.iter().all(|dir| push.allowed_in(dir, repo))),
        ["gh", "pr", verb, rest @ ..] => gh::PrCommand::parse(verb, rest).is_some_and(|command| command.allowed(dirs, root, repo)),
        _ => false,
    }
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
