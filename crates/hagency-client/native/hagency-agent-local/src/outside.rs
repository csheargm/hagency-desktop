//! "Ask only for outside changes": an owner-chosen approval mode (stored per
//! Agent on this device) under which a tool request that cannot change
//! anything outside the Agent's own workspace is allowed without asking.
//!
//! It narrows what an Ask-owner policy asks; it never allows more than the
//! policy layers already permit, never touches Deny, and never applies to a
//! `permission_expansion` request (Codex asking to leave its Room-folder
//! sandbox, reach the network or write elsewhere). The classifier is
//! deliberately small: Codex's own commands and file changes (its sandbox
//! keeps them in the Room folder), Claude's reads and file edits whose path
//! stays in the working directory (never git metadata), and a short list of plain read-only shell
//! commands (and `gh` issue/PR reads; see `rules` for the git and `gh` limits), plus local-only steps of an issue fix
//! (`cd`/`mkdir -p` inside, `gh repo clone` into it, a new branch, `git add`,
//! `git commit`) chained with `&&`/`;`/`|`. Everything else, including git
//! pushes and `gh` writes, asks.
use crate::ToolProposal;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The stored mode of an Agent that asks for every high-risk tool (the default).
pub const MODE_ASK: &str = "ask";
/// The stored mode that skips the question for changes inside the workspace.
pub const MODE_OUTSIDE: &str = "outside";

/// Create the per-Agent mode table.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS approval_modes (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('ask','outside'))) STRICT")
}

/// True when `proposal` can only touch the Agent's own workspace.
pub(crate) fn stays_in_workspace(proposal: &ToolProposal) -> bool {
    if proposal.risk == "permission_expansion" {
        return false;
    }
    let cwd = Path::new(&proposal.canonical_directory);
    let args = &proposal.arguments;
    match proposal.tool.as_str() {
        "codex.command" | "codex.file_change" => true,
        "claude.Read" | "claude.Glob" | "claude.Grep" => path_in(args.get("path").or(args.get("file_path")), cwd, true),
        "claude.Write" | "claude.Edit" | "claude.MultiEdit" => writable_in(args.get("file_path"), cwd),
        "claude.NotebookEdit" => writable_in(args.get("notebook_path"), cwd),
        "claude.Bash" => args["command"].as_str().is_some_and(|line| read_only_shell(line, cwd)),
        _ => false,
    }
}

/// A path argument that resolves inside `cwd`; an absent one means `cwd` itself
/// where `absent_ok` (the search tools default to the working directory).
fn path_in(value: Option<&Value>, cwd: &Path, absent_ok: bool) -> bool {
    match value.map(Value::as_str) {
        None => absent_ok,
        Some(Some(path)) => resolves_inside(path, cwd),
        Some(None) => false,
    }
}

/// A write target inside `cwd` that is not git metadata: a `.git` directory
/// (config, hooks) or a `.gitattributes`/`.gitmodules`/`.gitconfig` file can make
/// a later auto-allowed git command run a program, so those writes ask.
fn writable_in(value: Option<&Value>, cwd: &Path) -> bool {
    let Some(path) = value.and_then(Value::as_str) else { return false };
    let joined = shell::lexical_join(cwd, path);
    let real = std::fs::canonicalize(&joined).unwrap_or_else(|_| joined.clone());
    resolves_inside(path, cwd) && !is_git_metadata(&joined) && !is_git_metadata(&real)
}

fn is_git_metadata(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(name.as_str(), ".git" | ".gitattributes" | ".gitmodules" | ".gitconfig")
    })
}

/// Resolve `path` from `cwd` and require it to stay in `cwd`.
pub(crate) fn resolves_inside(path: &str, cwd: &Path) -> bool {
    resolves_within(path, cwd, cwd)
}

/// Lexically resolve `path` from `base` (no `..` may leave `root`), then
/// resolve symlinks through the deepest existing ancestor so a link cannot lead out.
fn resolves_within(path: &str, base: &Path, root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else { return false };
    let resolved = shell::lexical_join(base, path);
    let mut existing = resolved.as_path();
    let mut tail = PathBuf::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else { return false };
        tail = Path::new(name).join(&tail);
        existing = parent;
    }
    std::fs::canonicalize(existing).is_ok_and(|real| real.join(tail).starts_with(&root))
}

/// Every segment of `line` is a known command whose path words stay in `root`.
/// A `cd` adds a working directory; later paths must stay inside from every
/// directory the line may be in (a failed `cd` leaves the earlier one).
fn read_only_shell(line: &str, root: &Path) -> bool {
    let Some(parsed) = shell::parse(line) else { return false };
    let mut dirs = vec![root.to_path_buf()];
    parsed.segments.iter().enumerate().all(|(index, words)| {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        if parsed.heredoc_at == Some(index) && !is_commit_from_stdin(&words) {
            return false;
        }
        match words.as_slice() {
            ["cd", target] => enter_dir(target, &mut dirs, root),
            _ => segment_in_workspace(&words, &dirs, root),
        }
    })
}

/// A `cd <target>` inside `root` from every directory in `dirs`; on success
/// `dirs` also holds where it leads (the line may be in either afterwards).
pub(crate) fn enter_dir(target: &str, dirs: &mut Vec<PathBuf>, root: &Path) -> bool {
    if target.starts_with('-') || !inside_all(target, dirs, root) {
        return false;
    }
    let next: Vec<PathBuf> = dirs.iter().map(|dir| shell::lexical_join(dir, target)).collect();
    dirs.extend(next);
    true
}

/// One non-`cd` segment that outside mode lets run from any of `dirs`.
pub(crate) fn segment_in_workspace(words: &[&str], dirs: &[PathBuf], root: &Path) -> bool {
    rules::segment_allowed(words) && rules::paths(words).all(|w| inside_all(w, dirs, root))
}

pub(crate) fn inside_all(path: &str, dirs: &[PathBuf], root: &Path) -> bool {
    dirs.iter().all(|dir| resolves_within(path, dir, root))
}

/// `git commit ... -F -` (the heredoc is its message).
pub(crate) fn is_commit_from_stdin(words: &[&str]) -> bool {
    words.starts_with(&["git", "commit"]) && words.windows(2).any(|pair| matches!(pair, ["-F" | "--file", "-"]))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod bypass_tests;
#[cfg(test)]
mod filter_tests;
mod filters;
pub(crate) mod rules;
pub(crate) mod shell;
