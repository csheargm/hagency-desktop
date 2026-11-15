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
//! commands (and `gh` issue/PR reads), plus local-only steps of an issue fix
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

const READ_ONLY_PROGRAMS: [&str; 10] = ["ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "echo", "date"];
const READ_ONLY_GIT: [&str; 5] = ["status", "log", "diff", "show", "branch"];
/// `gh` reads of issues and pull requests: `(area, verb)`. They change nothing
/// on GitHub or on disk; `--web`/`-w` (opens a browser) still asks.
const READ_ONLY_GH: [(&str, &str); 4] = [("issue", "view"), ("issue", "list"), ("pr", "view"), ("pr", "list")];

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

/// Resolve `path` from `cwd` and require it to stay in `cwd`.
fn resolves_inside(path: &str, cwd: &Path) -> bool {
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
            ["cd", target] => {
                if target.starts_with('-') || !inside_all(target, &dirs, root) { return false; }
                let next: Vec<PathBuf> = dirs.iter().map(|dir| shell::lexical_join(dir, target)).collect();
                dirs.extend(next);
                true
            }
            _ => segment_allowed(&words) && paths(&words).all(|w| inside_all(w, &dirs, root)),
        }
    })
}

fn inside_all(path: &str, dirs: &[PathBuf], root: &Path) -> bool {
    dirs.iter().all(|dir| resolves_within(path, dir, root))
}

/// A known command shape (path words are checked by the caller).
fn segment_allowed(words: &[&str]) -> bool {
    let flags_in = |allowed: &[&str]| words[2..].iter().filter(|w| w.starts_with('-')).all(|w| allowed.contains(w));
    match words {
        ["git", "checkout", "-b", name] | ["git", "switch", "-c", name] => !name.starts_with('-'),
        ["git", "remote"] | ["git", "remote", "-v"] => true,
        ["git", "add", rest @ ..] => !rest.is_empty() && flags_in(&["-A", "-u", "--all", "-v", "--"]),
        ["git", "commit", ..] => flags_in(&["-", "-q", "--quiet", "-m", "--message", "-F", "--file", "-s", "--signoff", "-a", "--all"]),
        ["git", sub, ..] => READ_ONLY_GIT.contains(sub) && !words.iter().any(|w| w.starts_with("--output")),
        ["gh", "repo", "clone", repo, _path] => !repo.starts_with('-'),
        ["gh", "auth", "status", ..] => !words.iter().any(|w| *w == "--show-token" || *w == "-t"),
        ["gh", area, verb, ..] => READ_ONLY_GH.contains(&(*area, *verb)) && !words.iter().any(|w| *w == "--web" || *w == "-w"),
        ["mkdir", rest @ ..] => !rest.is_empty() && words[1..].iter().filter(|w| w.starts_with('-')).all(|w| *w == "-p"),
        [program, ..] => READ_ONLY_PROGRAMS.contains(program),
        [] => false,
    }
}

/// The words that may name files: non-flag arguments, minus the repository
/// of `gh repo clone`, a branch name, and a `git commit -m` message.
fn paths<'a>(words: &'a [&'a str]) -> impl Iterator<Item = &'a str> + 'a {
    let skip = match words {
        ["gh", "repo", "clone", ..] | ["git", "checkout", "-b", ..] | ["git", "switch", "-c", ..] => 4,
        _ => 1,
    };
    let mut after_message = false;
    words.iter().skip(skip).copied().filter(move |w| {
        let message = std::mem::replace(&mut after_message, words.get(1) == Some(&"commit") && matches!(*w, "-m" | "--message"));
        !message && !w.starts_with('-')
    })
}

/// `git commit ... -F -` (the heredoc is its message).
fn is_commit_from_stdin(words: &[&str]) -> bool {
    words.starts_with(&["git", "commit"]) && words.windows(2).any(|pair| matches!(pair, ["-F" | "--file", "-"]))
}

#[cfg(test)]
mod tests;
mod shell;
