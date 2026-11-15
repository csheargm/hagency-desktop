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
//! stays in the working directory, and a short list of plain read-only shell
//! commands (and `gh` issue/PR reads). Everything else, including git pushes
//! and `gh` writes, asks.
use crate::ToolProposal;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// The stored mode of an Agent that asks for every high-risk tool (the default).
pub const MODE_ASK: &str = "ask";
/// The stored mode that skips the question for changes inside the workspace.
pub const MODE_OUTSIDE: &str = "outside";

/// Create the per-Agent mode table.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS approval_modes (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('ask','outside'))) STRICT")
}

const READ_ONLY_PROGRAMS: [&str; 10] = ["ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "echo", "date"];
const READ_ONLY_GIT: [&str; 5] = ["status", "log", "diff", "show", "branch"];
/// `gh` reads of issues and pull requests: `(area, verb)`. They change nothing
/// on GitHub or on disk; `--web`/`-w` (opens a browser) still asks.
const READ_ONLY_GH: [(&str, &str); 4] = [("issue", "view"), ("issue", "list"), ("pr", "view"), ("pr", "list")];
const SHELL_CONTROL: [char; 12] = [';', '&', '|', '`', '$', '>', '<', '\n', '\r', '(', ')', '\\'];

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
        "claude.Write" | "claude.Edit" | "claude.MultiEdit" => path_in(args.get("file_path"), cwd, false),
        "claude.NotebookEdit" => path_in(args.get("notebook_path"), cwd, false),
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

/// Lexically resolve `path` from `cwd` (no `..` may leave it), then resolve
/// symlinks through the deepest existing ancestor so a link cannot lead out.
fn resolves_inside(path: &str, cwd: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(cwd) else { return false };
    let mut resolved = PathBuf::new();
    for part in cwd.join(path).components() {
        match part {
            Component::ParentDir => { resolved.pop(); }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let mut existing = resolved.as_path();
    let mut tail = PathBuf::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else { return false };
        tail = Path::new(name).join(&tail);
        existing = parent;
    }
    std::fs::canonicalize(existing).is_ok_and(|real| real.join(tail).starts_with(&root))
}

/// One plain command of a known read-only program whose path words stay in `cwd`.
fn read_only_shell(line: &str, cwd: &Path) -> bool {
    if line.is_empty() || line.chars().any(|c| SHELL_CONTROL.contains(&c)) {
        return false;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let allowed = match words.first() {
        Some(&"git") => words.get(1).is_some_and(|sub| READ_ONLY_GIT.contains(sub)) && !words.iter().any(|w| w.starts_with("--output") || *w == "-c"),
        Some(&"gh") => matches!((words.get(1), words.get(2)), (Some(area), Some(verb)) if READ_ONLY_GH.contains(&(*area, *verb)))
            && !words.iter().any(|w| *w == "--web" || *w == "-w"),
        Some(program) => READ_ONLY_PROGRAMS.contains(program),
        None => false,
    };
    allowed && words.iter().skip(1).filter(|w| !w.starts_with('-')).all(|w| resolves_inside(w, cwd))
}

#[cfg(test)]
mod tests;
