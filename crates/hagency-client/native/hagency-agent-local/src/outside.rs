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

/// True when `proposal` only LOOKS inside `cwd` (the Room workspace), ADR
/// 0025: a Claude `Read` whose only path is an absolute `file_path` inside it,
/// or a shell line of plain viewers ([`VIEWERS`]: no recursion, no option that
/// names a file or runs a program) and `cd` inside, every directory entered
/// and every file named resolved through links and none of it git metadata.
/// No git, `gh`, search tool, write or redirect.
pub(crate) fn inspects_workspace(proposal: &ToolProposal) -> bool {
    if proposal.risk == "permission_expansion" {
        return false;
    }
    let cwd = Path::new(&proposal.canonical_directory);
    let args = &proposal.arguments;
    match proposal.tool.as_str() {
        // Exactly the field Read opens (GPT review): a second path field asks.
        "claude.Read" => {
            let allowed = args.as_object().is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "file_path" | "offset" | "limit")));
            let Some(path) = args.get("file_path").and_then(Value::as_str).filter(|p| p.starts_with('/')) else { return false };
            allowed && resolves_inside(path, cwd) && not_metadata(cwd, path)
        }
        "claude.Bash" if args["run_in_background"] != true => args["command"].as_str().is_some_and(|line| plain_viewers(line, cwd)),
        _ => false,
    }
}

/// Programs a member may run to look, with the options each may take (none
/// of them names a file, follows links or recurses).
/// `ls -l` is left out: a long listing prints where links point (GPT review).
const VIEWERS: [(&str, &[&str]); 7] = [
    ("pwd", &[]),
    ("ls", &["-a", "-A", "-1", "-F"]),
    ("cat", &["-n"]),
    ("head", &["-n", "-c"]),
    ("tail", &["-n", "-c"]),
    ("wc", &["-l", "-w", "-c"]),
    ("date", &[]),
];

/// Whether `path` has a `..` step.
fn has_parent_step(path: &str) -> bool {
    path.split('/').any(|part| part == "..")
}

/// `path` from `cwd`, named and resolved through every link, is not git metadata.
fn not_metadata(cwd: &Path, path: &str) -> bool {
    !is_git_metadata(Path::new(path)) && real_path(cwd, path).is_some_and(|real| !is_git_metadata(&real))
}

/// Every segment of `line` a viewer (or `cd`) staying inside `root`.
fn plain_viewers(line: &str, root: &Path) -> bool {
    let Some(parsed) = shell::parse(line) else { return false };
    // No `$` at all: the reader keeps a quoted `$?` literal, the shell expands it.
    if parsed.heredoc_at.is_some() || line.contains(['>', '<', '$']) {
        return false;
    }
    let mut dirs = vec![root.to_path_buf()];
    parsed.segments.iter().all(|words| {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            // No `..`: the shell's logical `cd` and this check would part ways.
            ["cd", target] => !has_parent_step(target) && dirs.iter().all(|d| not_metadata(d, target)) && enter_dir(target, &mut dirs, root),
            [program, rest @ ..] => {
                let Some((_, options)) = VIEWERS.iter().find(|(p, _)| p == program) else { return false };
                // `pwd` and `date` take nothing (a `date` operand sets the clock).
                if matches!(*program, "pwd" | "date") && !rest.is_empty() {
                    return false;
                }
                let (mut words, mut operands) = (rest.iter(), false);
                while let Some(word) = words.next() {
                    // BSD tools stop reading options at the first file; `+` is tail's legacy follow form.
                    if word.starts_with('+') || has_parent_step(word) || (operands && word.starts_with('-')) {
                        return false;
                    }
                    if word.starts_with('-') {
                        // `head -n 5`: a counting option's value is a number.
                        let counted = matches!(*word, "-n" | "-c") && !matches!(*program, "ls" | "cat" | "wc");
                        if !options.contains(word) || (counted && !words.next().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))) {
                            return false;
                        }
                    } else if !(inside_all(word, &dirs, root) && dirs.iter().all(|d| not_metadata(d, word))) {
                        return false;
                    } else {
                        operands = true;
                    }
                }
                true
            }
            [] => false,
        }
    })
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
pub(crate) fn writable_in(value: Option<&Value>, cwd: &Path) -> bool {
    let Some(path) = value.and_then(Value::as_str) else { return false };
    let joined = shell::lexical_join(cwd, path);
    // The real target through every existing link (GPT review, Nov 16: an
    // alias of `.git/hooks` hid a new hook file from the lexical check).
    let Some(real) = physical_join(cwd, path).and_then(|p| real_target(&p)) else { return false };
    resolves_inside(path, cwd) && !is_git_metadata(&joined) && !is_git_metadata(&real)
}

pub(crate) fn is_git_metadata(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(name.as_str(), ".git" | ".gitattributes" | ".gitmodules" | ".gitconfig")
    })
}

/// `path` from `cwd` as the OS opens it, through every existing link; a
/// dangling link on the way is `None`.
pub(crate) fn real_path(cwd: &Path, path: &str) -> Option<PathBuf> {
    physical_join(cwd, path).and_then(|p| real_target(&p))
}

/// Resolve `path` from `cwd` and require it to stay in `cwd`.
pub(crate) fn resolves_inside(path: &str, cwd: &Path) -> bool {
    resolves_within(path, cwd, cwd)
}

/// Lexically resolve `path` from `base` (no `..` may leave `root`), then
/// resolve symlinks through the deepest existing ancestor so a link cannot lead out.
fn resolves_within(path: &str, base: &Path, root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else { return false };
    physical_join(base, path).and_then(|p| real_target(&p)).is_some_and(|real| real.starts_with(&root))
}

/// `path` with symlinks resolved through its deepest existing ancestor; a
/// dangling link on the way is refused (opening it would create its target).
fn real_target(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut tail = PathBuf::new();
    while !existing.exists() {
        if existing.symlink_metadata().is_ok() {
            return None;
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else { return None };
        tail = Path::new(name).join(&tail);
        existing = parent;
    }
    std::fs::canonicalize(existing).ok().map(|real| real.join(tail))
}

/// Join `path` onto `base` the way the OS opens it: a `..` steps out of the
/// real (symlink-resolved) directory, so `link/../x` with `link -> /etc` is
/// `/x`, not `<base>/x` (GPT review, Nov 15).
fn physical_join(base: &Path, path: &str) -> Option<PathBuf> {
    let mut resolved = PathBuf::new();
    for part in base.join(path).components() {
        match part {
            std::path::Component::ParentDir => {
                if resolved.exists() {
                    resolved = std::fs::canonicalize(&resolved).ok()?;
                }
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    Some(resolved)
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
/// A git command also needs every directory's local config to run no helper.
pub(crate) fn segment_in_workspace(words: &[&str], dirs: &[PathBuf], root: &Path) -> bool {
    rules::segment_allowed(words)
        && rules::paths(words).all(|w| inside_all(w, dirs, root))
        && (words.first() != Some(&"git") || dirs.iter().all(|dir| git_config::local_config_safe(dir)))
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
#[cfg(test)]
mod git_config_tests;
mod filters;
pub(crate) mod git_config;
pub(crate) mod rules;
pub(crate) mod shell;
