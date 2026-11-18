//! The local fix steps "push freely" answers in the watched repository's own
//! checkout (ADR 0022): `status`, `diff`, `log`, `show`, `branch` (listing),
//! `add`, `commit` and `restore`, one command each, options from a closed
//! list, every path word inside the checkout. They need no network and run
//! as the Agent, pinned: hooks, fsmonitor, pager, editor, external diff,
//! textconv, lazy fetch, submodule recursion and every transport off, and
//! attributes read from the empty tree, so no filter driver runs.
//!
//! "Own checkout" ([`own_checkout`]): the top of a plain clone (a real `.git`
//! directory, no `commondir`) whose local config holds data keys only, with
//! no `.gitmodules` or `info/attributes`, and EVERY remote fetching from and
//! pushing to the watched repository on github.com.
use super::git::{fetch_urls_are, push_urls_are, query};
use std::path::{Path, PathBuf};

/// Git's built-in empty tree: attributes read from it select no filter.
pub(super) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// `-c` pins of every Agent-run local step, whatever the checkout's config says.
const PINS: [&str; 11] = [
    "core.attributesFile=/dev/null",
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.pager=cat",
    "core.editor=false",
    "commit.gpgSign=false",
    "protocol.allow=never",
    "submodule.recurse=false",
    "diff.ignoreSubmodules=all",
    "gc.auto=0",
    "maintenance.auto=false",
];
/// Variables an earlier command could have left in the shell that would move
/// git to another repository, config or program.
const UNSET: [&str; 14] = [
    "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR", "GIT_CONFIG", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "GIT_CONFIG_PARAMETERS",
    "GIT_EXEC_PATH", "GIT_EXTERNAL_DIFF", "GIT_SSH_COMMAND", "GIT_TEMPLATE_DIR",
];

const STATUS_FLAGS: [&str; 14] = [
    "-s", "--short", "-b", "--branch", "-sb", "--long", "--porcelain", "--porcelain=v1", "--porcelain=v2",
    "-uno", "-uall", "--untracked-files=no", "--untracked-files=normal", "--untracked-files=all",
];
const DIFF_FLAGS: [&str; 15] = [
    "--stat", "--cached", "--staged", "--name-only", "--name-status", "--no-color", "--color=never", "--shortstat",
    "--numstat", "--check", "-w", "--ignore-all-space", "-p", "--patch", "--summary",
];
const LOG_FLAGS: [&str; 18] = [
    "--oneline", "--stat", "--graph", "--decorate", "-p", "--patch", "--name-only", "--name-status", "--no-color",
    "--all", "--reverse", "--first-parent", "--no-merges", "--shortstat", "--pretty=oneline", "--pretty=short",
    "--pretty=medium", "--pretty=full",
];
const SHOW_FLAGS: [&str; 10] = ["--stat", "--name-only", "--name-status", "--no-color", "--oneline", "-s", "--no-patch", "--shortstat", "--summary", "--pretty=oneline"];
const BRANCH_FLAGS: [&str; 8] = ["-a", "--all", "-r", "--remotes", "-v", "-vv", "--list", "--show-current"];
const ADD_FLAGS: [&str; 6] = ["-A", "--all", "-u", "--update", "-v", "--verbose"];
const COMMIT_FLAGS: [&str; 7] = ["-q", "--quiet", "-a", "--all", "-s", "--signoff", "--allow-empty"];
const RESTORE_FLAGS: [&str; 6] = ["--staged", "-S", "--worktree", "-W", "-q", "--quiet"];

/// Whether `dir` is the watched repository's own checkout (see the module note).
pub(crate) fn own_checkout(dir: &Path, repo: &str) -> bool {
    let Ok(dir) = dir.canonicalize() else { return false };
    let top = query(&dir, &["rev-parse", "--show-toplevel"]).and_then(|t| Path::new(&t).canonicalize().ok());
    if top.as_deref() != Some(dir.as_path()) || super::host_git::objects_dir(&dir).is_err() {
        return false;
    }
    let Some(attributes) = query(&dir, &["rev-parse", "--path-format=absolute", "--git-path", "info/attributes"]) else { return false };
    if Path::new(&attributes).symlink_metadata().is_ok() || dir.join(".gitmodules").symlink_metadata().is_ok() {
        return false;
    }
    let remotes = query(&dir, &["remote"]).unwrap_or_default();
    crate::outside::git_config::local_config_safe(&dir)
        && !remotes.is_empty()
        && remotes.lines().all(|r| super::git::remote_name(r) && fetch_urls_are(&dir, r, repo) && push_urls_are(&dir, r, repo))
}

/// The nearest checkout (a directory holding `.git`) that contains `real`
/// (an already symlink-resolved path), without leaving `root`.
pub(crate) fn checkout_containing(real: &Path, root: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    real.ancestors().take_while(|dir| dir.starts_with(&root)).find(|dir| dir.join(".git").symlink_metadata().is_ok()).map(Path::to_path_buf)
}

/// The pinned `git -C <dir> <args>` the Agent's call runs in place of its own.
pub(crate) fn pinned_git(dir: &Path, args: &[&str]) -> String {
    pinned_with(dir, &[], args)
}

/// [`pinned_git`] that may read a local repository path (a host scratch).
pub(crate) fn pinned_git_file(dir: &Path, args: &[&str]) -> String {
    pinned_with(dir, &["protocol.file.allow=always"], args)
}

fn pinned_with(dir: &Path, extra: &[&str], args: &[&str]) -> String {
    let mut words = vec!["git".to_owned(), "--no-pager".to_owned(), "-C".to_owned(), dir.to_string_lossy().into_owned()];
    for pin in PINS.iter().chain(extra) {
        words.extend(["-c".to_owned(), (*pin).to_owned()]);
    }
    words.extend(args.iter().map(|a| (*a).to_owned()));
    let line = words.iter().map(|w| super::quote(w)).collect::<Vec<_>>().join(" ");
    let unset: Vec<String> = UNSET.iter().map(|v| format!("-u {v}")).collect();
    super::pin_env(&format!("env {} GIT_ATTR_SOURCE={EMPTY_TREE} GIT_NO_LAZY_FETCH=1 GIT_PAGER=cat GIT_EDITOR=false GIT_TERMINAL_PROMPT=0 {line}", unset.join(" ")))
}

/// One local step `rest` (the words after `git [-C <dir>]`) in checkout
/// `dir`, pinned; `heredoc` is the `<<'TAG'` text that feeds `commit -F -`.
/// `None` for anything else.
pub(crate) fn step(dir: &Path, rest: &[&str], heredoc: Option<&str>, repo: &str) -> Option<String> {
    let (sub, args) = rest.split_first()?;
    if heredoc.is_some() && *sub != "commit" {
        return None;
    }
    let ok = match *sub {
        "status" => words_ok(dir, args, &STATUS_FLAGS),
        "diff" => words_ok(dir, args, &DIFF_FLAGS),
        "log" => log_ok(dir, args),
        "show" => words_ok(dir, args, &SHOW_FLAGS),
        "branch" => branch_ok(args),
        "add" => !args.is_empty() && words_ok(dir, args, &ADD_FLAGS),
        "restore" => args.iter().any(|w| !w.starts_with('-')) && words_ok(dir, args, &RESTORE_FLAGS),
        "commit" => commit_ok(args, heredoc.is_some()),
        _ => false,
    };
    if !ok || !own_checkout(dir, repo) {
        return None;
    }
    let mut words = vec![*sub];
    if matches!(*sub, "diff" | "log" | "show") {
        words.extend(["--no-ext-diff", "--no-textconv"]);
    }
    if *sub == "commit" {
        words.push("--no-verify");
    }
    words.extend(args);
    let pinned = pinned_git(dir, &words);
    Some(match heredoc {
        Some(body) => format!("{pinned} {body}"),
        None => pinned,
    })
}

/// Every option is in `flags` (or `--`), and every other word, a revision or
/// a path, resolves inside `dir` (so `git diff /etc/x` cannot fall back to
/// `--no-index` outside it).
fn words_ok(dir: &Path, args: &[&str], flags: &[&str]) -> bool {
    let mut after_dashes = false;
    args.iter().all(|word| {
        if *word == "--" && !after_dashes {
            after_dashes = true;
            return true;
        }
        if word.starts_with('-') && !after_dashes {
            return flags.contains(word);
        }
        !word.is_empty() && crate::outside::resolves_inside(word, dir)
    })
}

/// `branch` listing: flags from [`BRANCH_FLAGS`], and with `--list` (git
/// then reads every other word as a pattern, never a branch to create) plain
/// ref patterns, `*` allowed.
fn branch_ok(args: &[&str]) -> bool {
    let listing = args.contains(&"--list");
    let pattern = |w: &str| !w.is_empty() && !w.starts_with('-') && w.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'*'));
    args.iter().all(|w| BRANCH_FLAGS.contains(w) || (listing && pattern(w)))
}

/// `log` with its count forms (`-5`, `-n 5`, `-n5`, `--max-count=5`).
fn log_ok(dir: &Path, args: &[&str]) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut kept = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let count = word.strip_prefix('-').is_some_and(digits)
            || word.strip_prefix("-n").is_some_and(digits)
            || word.strip_prefix("--max-count=").is_some_and(digits);
        if *word == "-n" {
            if !words.next().is_some_and(|n| digits(n)) {
                return false;
            }
        } else if !count {
            kept.push(*word);
        }
    }
    words_ok(dir, &kept, &LOG_FLAGS)
}

/// `commit` with a message: `-m <text>` / `--message=<text>`, or ONE `-F -`
/// fed by the heredoc, never both. `-F` may name no file: git takes the last
/// one, which would put a file from anywhere into a pushed commit (GPT
/// review, Nov 17). No paths, no editor, no amend.
fn commit_ok(args: &[&str], heredoc: bool) -> bool {
    let (mut message, mut stdin) = (false, 0);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match *word {
            "-m" | "--message" => message |= words.next().is_some(),
            "-F" | "--file" if words.next() == Some(&"-") => stdin += 1,
            w if w.starts_with("--message=") => message = true,
            w if COMMIT_FLAGS.contains(&w) => {}
            _ => return false,
        }
    }
    if heredoc { stdin == 1 && !message } else { message && stdin == 0 }
}

#[cfg(test)]
#[path = "local_git_tests.rs"]
mod tests;
