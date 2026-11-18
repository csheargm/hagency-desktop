//! "Let this Agent push branches and open pull requests here without asking":
//! an owner opt-in stored with the Agent's issue watch (this device's ledger,
//! written only through the owner API; nothing the Agent runs can set it).
//!
//! While the watch is on, the switch is on, the turn runs in the watch's Room
//! for the owner (the issue-watch notice is the owner's), the call is a
//! foreground Claude `Bash` call or a Codex call of the host's
//! `watched_repo_step` tool ([`codex_step`]), and the WHOLE line is one
//! publish step, the call runs without a card, whatever the high-risk tool
//! rule says (Deny included, ADR 0022 "Deny"). Fail-closed by design (GPT
//! review, Nov 15): no chaining (an earlier step could switch branch or edit
//! config), no reliance on the shell's persistent directory, and never
//! Codex's own shell (its approvals cannot rewrite the command, see
//! [`pinned`]). Steps:
//! - `git -C <checkout> push [-u] <remote> <branch>`: a local non-default
//!   branch named explicitly, from a checkout inside the Room workspace whose
//!   remote pushes to the watched repository (see [`git`]). The HOST pushes
//!   it ([`host_git`]), so no checkout config is read for the network step;
//! - `git [-C <checkout>] fetch [-q] [<remote>]` from the watched repository,
//!   also run by the host, and `git [-C <checkout>] checkout -b <new>
//!   <remote>/<default>`, pinned (the fix steps of an issue, see [`git`]);
//! - `gh pr create|ready|edit` naming the watched repository (see [`gh`]);
//! - the local fix steps in the watched repository's own checkout (`status`,
//!   `diff`, `log`, `show`, `branch`, `add`, `commit`, `restore`, pinned, see
//!   [`local_git`]), `pull` and a `clone` of it into the workspace (host-run,
//!   see [`host_clone`]), ADR 0022;
//! - Read/Glob/Grep, and Write/Edit/MultiEdit (never git metadata), of a path
//!   inside that checkout; for Codex, a native file change whose every path
//!   is inside it.
//!
//! Everything else keeps asking. Merges stay with `merge_policy`.
use crate::{ToolProposal, outside::shell};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub mod codex_step;
pub(crate) mod gh;
pub(crate) mod git;
pub(crate) mod host_clone;
pub mod host_git;
pub mod host_run;
pub(crate) mod local_git;
pub mod read_only;
pub mod room_scope;

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
/// Room: each active watch reporting here (the legacy issue watch or a
/// work-list entry, as the room view resolves them) whose owner set "push
/// freely".
fn trusted_repos(db: &Connection, proposal: &ToolProposal) -> crate::Result<Vec<String>> {
    Ok(room_entries(db, proposal)?.into_iter().filter(|e| e.push_freely).map(|e| e.repo).collect())
}

/// The Agent's effective watches (`watch_info::entries`, so a work-list
/// entry replaces a legacy watch of the same repository, GPT review) that
/// are active and report to `proposal`'s Room.
fn room_entries(db: &Connection, proposal: &ToolProposal) -> crate::Result<Vec<crate::watch_info::WatchEntry>> {
    use crate::issue_watch::{IssueWatch, WatchMode, WatchSetting};
    let (agent, room) = (&proposal.scope.agent, &proposal.scope.room);
    let legacy: Option<(String, String, String, i64)> = db
        .query_row("SELECT mode,repo,room_id,push_trusted FROM issue_watch WHERE agent=?", [agent], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let watch = legacy.map_or_else(IssueWatch::default, |(mode, repo, room_id, trusted)| IssueWatch {
        setting: WatchSetting { mode: WatchMode::from_stored(&mode), label: String::new(), repo, room_id, push_trusted: trusted == 1 },
        enabled_at: 0,
    });
    let mut work = Vec::new();
    let mut rows = db.prepare("SELECT rules FROM agent_work WHERE agent=? ORDER BY repo")?;
    for rules in rows.query_map([agent], |r| r.get::<_, String>(0))? {
        // A row that no longer parses grants nothing.
        if let Ok(rules) = serde_json::from_str::<crate::merge_policy::work::OwnerRules>(&rules?) {
            work.push(rules);
        }
    }
    let entries = crate::watch_info::entries(&watch, &work);
    Ok(entries.into_iter().filter(|e| e.active && &e.room_id == room).collect())
}

/// Whether `proposal` is a one-line shell step (a foreground Claude `Bash`
/// call or a Codex [`codex_step`] call) read-only GitHub query on a
/// repository of this Room's repository set ([`read_scope`]), push trust or
/// not. Any requester in that Room: what it reads is what the room already
/// tracks (ADR 0019 "Anyone in the report Room", widened by ADR 0023).
pub(crate) fn read_only_query(db: &Connection, proposal: &ToolProposal) -> crate::Result<bool> {
    if proposal.risk == "permission_expansion" || !is_shell_step(proposal) {
        return Ok(false);
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(false) };
    Ok(read_scope(db, proposal)?.iter().any(|repo| read_only::allowed(line, repo)))
}

/// What a read-only query may name (ADR 0023): the room's repository set,
/// not only this Agent's own watches. This device's other Agents' active
/// watches reporting to the Room, and the set the host recorded for this
/// requester (linked repositories, and published watches from publishers
/// who could have linked them).
fn read_scope(db: &Connection, proposal: &ToolProposal) -> crate::Result<Vec<String>> {
    let mut repos = Vec::new();
    let mut agents = db.prepare("SELECT agent FROM issue_watch UNION SELECT agent FROM agent_work ORDER BY 1")?;
    for agent in agents.query_map([], |r| r.get::<_, String>(0))? {
        let scope = crate::Scope { agent: agent?, ..proposal.scope.clone() };
        repos.extend(room_entries(db, &ToolProposal { scope, ..proposal.clone() })?.into_iter().map(|e| e.repo));
    }
    repos.extend(room_scope::repos(&proposal.scope.room, &proposal.scope.requester));
    Ok(repos)
}

/// A foreground Claude `Bash` call, or a Codex call of the host's
/// [`codex_step::TOOL`] (Codex's own shell is never answered here: its
/// command cannot be pinned, see [`codex_step`]).
fn is_shell_step(proposal: &ToolProposal) -> bool {
    match proposal.tool.as_str() {
        // A background call's launch result says nothing about what it did.
        "claude.Bash" => proposal.arguments["run_in_background"] != true,
        codex_step::TOOL => true,
        _ => false,
    }
}

/// `proposal` as push trust must see it: rooted at the Room `workspace`.
/// The permission callback derives `canonical_directory` from the call's own
/// path argument (an absolute file's parent), which must never become the
/// containment root (GPT review, Nov 17).
pub(crate) fn for_workspace(proposal: &ToolProposal, workspace: &Path) -> ToolProposal {
    let root = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    ToolProposal { canonical_directory: root.to_string_lossy().into(), ..proposal.clone() }
}

/// A check, run off the async threads, that push trust still covers
/// `proposal` for `repo` (the step's own destination): a fresh read-only connection to the ledger at `path` each
/// time; no path or an unreadable ledger is a no.
pub(crate) fn recheck(path: Option<std::path::PathBuf>, proposal: ToolProposal, repo: String) -> Box<dyn Fn() -> bool + Send + Sync> {
    Box::new(move || {
        let Some(path) = &path else { return false };
        let Ok(db) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false };
        trusted_repos(&db, &proposal).is_ok_and(|r| r.iter().any(|t| t.eq_ignore_ascii_case(&repo)))
    })
}

/// How push trust answers `proposal`, if it does. A command reaches Claude's
/// permission reply as `updatedInput`, and a Codex step is run by the host
/// ([`codex_step`]), so what runs is exactly what was checked. Codex's own
/// shell cannot rewrite its command, so it is never trusted; its file
/// changes are, inside the checkout ([`codex_step::file_change_paths`]).
pub(crate) fn pinned(db: &Connection, proposal: &ToolProposal, owner: &str) -> crate::Result<Option<TrustedStep>> {
    let edit = matches!(proposal.tool.as_str(), "claude.Write" | "claude.Edit" | "claude.MultiEdit");
    let read = matches!(proposal.tool.as_str(), "claude.Read" | "claude.Glob" | "claude.Grep");
    let patch = proposal.tool == "codex.file_change";
    if proposal.risk == "permission_expansion" || !(edit || read || patch || is_shell_step(proposal)) || proposal.scope.requester != owner {
        return Ok(None);
    }
    let root = Path::new(&proposal.canonical_directory);
    if patch {
        let Some(paths) = codex_step::file_change_paths(&proposal.arguments) else { return Ok(None) };
        let repos = trusted_repos(db, proposal)?;
        let trusted = paths.iter().all(|p| edit_in_own_checkout(Some(&serde_json::Value::from(p.as_str())), p, true, root, &repos));
        return Ok(trusted.then_some(TrustedStep::Allow));
    }
    if edit || read {
        let args = &proposal.arguments;
        let path = args.get("file_path").or(args.get("path")).map_or(Some("."), serde_json::Value::as_str);
        let Some(path) = path else { return Ok(None) };
        let repos = trusted_repos(db, proposal)?;
        return Ok(edit_in_own_checkout(args.get("file_path"), path, edit, root, &repos).then_some(TrustedStep::Allow));
    }
    let Some(line) = proposal.arguments["command"].as_str() else { return Ok(None) };
    Ok(trusted_repos(db, proposal)?.iter().find_map(|repo| pinned_line(line, root, repo)))
}

/// Whether `path` (an edit's `file_path` value when `edit`, else a read's
/// path) stays inside `root`, out of git metadata (named or resolved), in
/// the own checkout of one of `repos`.
fn edit_in_own_checkout(file_path: Option<&serde_json::Value>, path: &str, edit: bool, root: &Path, repos: &[String]) -> bool {
    let inside = if edit { crate::outside::writable_in(file_path, root) } else { crate::outside::resolves_inside(path, root) };
    let real = if inside { crate::outside::real_path(root, path) } else { None };
    // Reads stay out of git metadata too (GPT review, Nov 17).
    let metadata = crate::outside::is_git_metadata(Path::new(path)) || real.as_deref().is_some_and(crate::outside::is_git_metadata);
    let checkout = real.filter(|_| !metadata).and_then(|real| local_git::checkout_containing(&real, root));
    checkout.is_some_and(|checkout| repos.iter().any(|repo| local_git::own_checkout(&checkout, repo)))
}

/// How `line` is answered when it is a single trusted step for `repo` from
/// inside `root`, else `None`.
pub(crate) fn pinned_line(line: &str, root: &Path, repo: &str) -> Option<TrustedStep> {
    let parsed = shell::parse(line)?;
    let [words] = parsed.segments.as_slice() else { return None };
    // Only `git commit -F -` may read a heredoc: its message (see `local_git`).
    let heredoc = match parsed.heredoc_at {
        Some(_) => Some(&line[line.find("<<")?..]),
        None => None,
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    if heredoc.is_some() && !matches!(words.as_slice(), ["git", "-C", _, "commit", ..] | ["git", "commit", ..]) {
        return None;
    }
    match words.as_slice() {
        ["git", "clone", rest @ ..] => host_clone::request(root, true, rest, repo).map(TrustedStep::Host),
        ["gh", "repo", "clone", rest @ ..] => host_clone::request(root, false, rest, repo).map(TrustedStep::Host),
        ["git", "-C", dir, "push", rest @ ..] => {
            let dir = checkout(root, dir)?;
            let push = git::Push::parse(rest)?;
            (crate::outside::git_config::local_config_safe(&dir) && push.allowed_in(&dir, repo)).then(|| TrustedStep::Host(push.host_step(&dir, repo)))
        }
        ["git", "-C", dir, rest @ ..] => git::fix_step(&checkout(root, dir)?, rest, heredoc, repo),
        ["git", rest @ ..] => git::fix_step(&toplevel(root)?, rest, heredoc, repo),
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
#[cfg(test)]
mod read_only_flow_tests;
#[cfg(test)]
mod room_scope_tests;
#[cfg(test)]
pub(crate) mod fix_flow_tests;
#[cfg(test)]
mod deny_override_tests;
