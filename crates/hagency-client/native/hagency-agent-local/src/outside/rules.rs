//! Which command shapes outside mode lets run, and which words of them name
//! files (the caller path-checks those).
//!
//! Git: hooks and fsmonitor are switched off in the runtime's environment
//! (`agent_env::GIT_HELPERS_OFF_ENV`); here every option that runs a helper or
//! points git elsewhere (`-c`/`-C` before the subcommand, `--git-dir`,
//! `--ext-diff`, `--textconv`, ...) asks, and `git branch` may only list.
//!
//! `gh`: issue/PR reads go to the network with the Agent's own token, for any
//! repository it can read. That is a read of data the Agent may already see,
//! so it stays allowed; but only on github.com (`--hostname`, a
//! `HOST/OWNER/REPO` repository and `gh auth status -h` ask) and never opening
//! a browser (`--web`/`-w`).

use super::filters;

pub(super) const READ_ONLY_PROGRAMS: [&str; 10] = ["ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "echo", "date"];
/// Git subcommands that only read local history. `fetch` is deliberately not
/// here: `-u`/`--upload-pack`, URLs, `remote::` helpers, remotes, submodules
/// and refspecs make it a network and code-execution surface (GPT review,
/// Nov 15).
const READ_ONLY_GIT: [&str; 10] =
    ["status", "log", "diff", "show", "rev-parse", "ls-files", "blame", "shortlog", "describe", "merge-base"];
/// `git branch` forms that only list.
const BRANCH_LISTING: [&str; 8] = ["-a", "--all", "-r", "--remotes", "-v", "-vv", "--list", "--show-current"];
/// Git options that run a helper program or move git to another repository or
/// config. Global options (`-c`, `-C`, `--git-dir` before the subcommand) never
/// match a known shape, so they ask too.
const GIT_DENIED: [&str; 12] = [
    "--ext-diff", "--textconv", "--exec-path", "--git-dir", "--work-tree", "--config-env", "--output", "--upload-pack",
    "--receive-pack", "--open-files-in-pager", "--namespace", "--super-prefix",
];
/// `rg` options that run another program on the files it reads.
const RG_DENIED: [&str; 4] = ["--pre", "--pre-glob", "-z", "--search-zip"];
/// `gh` reads: `(area, verb)`. None of these changes anything on GitHub.
/// `gh run` is left out: Actions logs can carry secrets a job printed.
/// Shared with `remembered::network`: only these `gh` forms may be "Always allow"ed.
pub(crate) const READ_ONLY_GH: [(&str, &str); 12] = [
    ("issue", "view"), ("issue", "list"), ("issue", "status"),
    ("pr", "view"), ("pr", "list"), ("pr", "diff"), ("pr", "checks"), ("pr", "status"),
    ("repo", "view"), ("release", "list"), ("release", "view"), ("label", "list"),
];

/// A known command shape (path words are checked by the caller).
pub(super) fn segment_allowed(words: &[&str]) -> bool {
    let flags_in = |allowed: &[&str]| words[2..].iter().filter(|w| w.starts_with('-')).all(|w| allowed.contains(w));
    if !flag_words_safe(words) {
        return false;
    }
    match words {
        ["git", ..] if !git_options_safe(words) => false,
        ["git", "checkout", "-b", name] | ["git", "switch", "-c", name] => !name.starts_with('-'),
        ["git", "remote"] | ["git", "remote", "-v"] => true,
        ["git", "branch", rest @ ..] => rest.iter().all(|w| BRANCH_LISTING.contains(w)),
        ["git", "add", rest @ ..] => !rest.is_empty() && flags_in(&["-A", "-u", "--all", "-v", "--"]),
        ["git", "commit", ..] => flags_in(&["-", "-q", "--quiet", "-m", "--message", "-F", "--file", "-s", "--signoff", "-a", "--all"]),
        ["git", sub, ..] => READ_ONLY_GIT.contains(sub),
        ["gh", ..] if !gh_options_safe(words) => false,
        ["gh", "repo", "clone", repo, _path] => !repo.starts_with('-'),
        ["gh", "auth", "status", rest @ ..] => rest.is_empty(),
        ["gh", "api", rest @ ..] => filters::gh_api_read(rest) && gh_filters_safe(rest),
        ["gh", area, verb, rest @ ..] => READ_ONLY_GH.contains(&(*area, *verb)) && gh_targets_safe(&gh_without_data(rest)) && gh_filters_safe(rest),
        ["rg", ..] if words.iter().any(|w| RG_DENIED.iter().any(|d| option_is(w, d))) => false,
        ["mkdir", rest @ ..] => !rest.is_empty() && words[1..].iter().filter(|w| w.starts_with('-')).all(|w| *w == "-p"),
        [program, rest @ ..] if filters::FILTERS.contains(program) => filters::filter_allowed(program, rest) && (*program != "jq" || gh_filters_safe(rest)),
        [program, ..] => READ_ONLY_PROGRAMS.contains(program),
        [] => false,
    }
}

/// `word` is option `name`, bare or as `name=value`.
fn option_is(word: &str, name: &str) -> bool {
    word == name || word.strip_prefix(name).is_some_and(|rest| rest.starts_with('='))
}

/// A flag word may carry a value only as `--opt=value`, and that value is
/// path-checked by `paths`; a short flag with an attached path (`-f/etc/x`) or
/// a `~` anywhere in a flag asks.
fn flag_words_safe(words: &[&str]) -> bool {
    words.iter().filter(|w| w.starts_with('-')).all(|w| match w.split_once('=') {
        Some((name, value)) => name.starts_with("--") && !value.starts_with('~'),
        None => !w.contains('/') && !w.contains('~'),
    })
}

fn git_options_safe(words: &[&str]) -> bool {
    !words[1..].iter().any(|w| GIT_DENIED.iter().any(|d| option_is(w, d)))
}

fn gh_options_safe(words: &[&str]) -> bool {
    let mut repo_next = false;
    words.iter().all(|w| {
        let repo_value = std::mem::replace(&mut repo_next, matches!(*w, "-R" | "--repo"));
        let repo = if repo_value { Some(*w) } else { w.strip_prefix("--repo=") };
        if repo.is_some_and(|r| !github_repo_name(r)) {
            return false;
        }
        !["--web", "-w", "--browse", "--hostname", "-h"].iter().any(|d| option_is(w, d)) && short_cluster_safe(w)
    }) && !repo_next
}

/// Short letters gh rejects here (`-w` web, `-h` hostname), and letters that
/// take a value, which consumes the rest of the word (`-qenv.X`).
const GH_SHORT_DENIED: [char; 2] = ['w', 'h'];
const GH_SHORT_VALUED: [char; 8] = ['q', 't', 'R', 'b', 'B', 'H', 'L', 'S'];

/// pflag expands `-cw` to `-c -w`: check each letter of a short cluster.
fn short_cluster_safe(word: &str) -> bool {
    let Some(letters) = word.strip_prefix('-').filter(|l| !l.starts_with('-')) else { return true };
    for c in letters.chars() {
        if GH_SHORT_DENIED.contains(&c) { return false; }
        if GH_SHORT_VALUED.contains(&c) { return true; }
    }
    true
}

/// Positional `gh` arguments: a number, a branch/tag-like word, or
/// `OWNER/REPO`; never a `HOST/OWNER/REPO` or a URL, which could pick another
/// configured host. Option values are checked by [`gh_options_safe`].
fn gh_targets_safe(rest: &[&str]) -> bool {
    rest.iter().all(|w| w.starts_with('-') || !(w.contains(':') || w.matches('/').count() > 1 || (w.contains('/') && !github_repo_name(w))))
}

/// Options whose value is an output filter or field list that `gh` itself
/// reads (jq, Go template, `--json` fields): data, never a path or target.
/// Only with `--json`; the shell tokenizer has already refused substitutions
/// and redirects outside quotes.
const GH_DATA_OPTIONS: [&str; 5] = ["--json", "-q", "--jq", "-t", "--template"];

/// `words` minus the values of [`GH_DATA_OPTIONS`] (and `--jq=...` forms)
/// when the command reads `--json`; otherwise unchanged.
pub(super) fn gh_without_data<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let json = words.iter().any(|w| option_is(w, "--json"));
    let mut value_next = false;
    words.iter().copied().filter(|w| {
        if !json { return true; }
        // A value is consumed before it is read as an option (`--template --jq URL`).
        if std::mem::take(&mut value_next) { return false; }
        value_next = GH_DATA_OPTIONS.contains(w);
        !GH_DATA_OPTIONS.iter().any(|d| d.starts_with("--") && w.strip_prefix(d).is_some_and(|r| r.starts_with('=')))
    }).collect()
}

/// Filter words that could read the process environment (and so print
/// GH_TOKEN into the room): jq `env`/`$ENV`, `$__loc__`, `input_filename`, or
/// any `$`. Matched loosely on purpose; a false hit only asks the owner.
const GH_FILTER_DENIED: [&str; 4] = ["env", "$", "input", "__loc__"];

/// No word of a gh read names anything in [`GH_FILTER_DENIED`]. Every word
/// is checked, not only parsed filter values: pflag also takes attached and
/// clustered short values (`-qenv.X`, `-cqenv.X`).
fn gh_filters_safe(words: &[&str]) -> bool {
    words.iter().all(|w| {
        let lower = w.to_ascii_lowercase();
        !GH_FILTER_DENIED.iter().any(|d| lower.contains(d))
    })
}

/// `OWNER/REPO` on github.com (no host prefix).
fn github_repo_name(value: &str) -> bool {
    let mut parts = value.split('/');
    let ok = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && p != "..");
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// The words that may name files: non-flag arguments and `--opt=value`
/// values, minus the repository of `gh repo clone`, a branch name, and a
/// `git commit -m` message.
pub(super) fn paths<'a>(words: &'a [&'a str]) -> impl Iterator<Item = &'a str> + 'a {
    let gh: Option<Vec<&'a str>> = (words.first() == Some(&"gh")).then(|| gh_without_data(words));
    let mut words: Vec<&'a str> = gh.unwrap_or_else(|| words.to_vec());
    match words.as_slice() {
        // `gh api` names an endpoint and filters, never a file.
        ["gh", "api", ..] => words.clear(),
        // jq's first positional word is its filter program, not a file.
        ["jq", rest @ ..] => {
            let filter = filters::jq_filter(rest);
            if let Some(at) = words.iter().skip(1).position(|w| Some(*w) == filter) { words.remove(at + 1); }
        }
        _ => {}
    }
    let skip = match words.as_slice() {
        ["gh", "repo", "clone", ..] | ["git", "checkout", "-b", ..] | ["git", "switch", "-c", ..] => 4,
        _ => 1,
    };
    let commit = words.get(1) == Some(&"commit");
    let mut after_message = false;
    words.into_iter().skip(skip).filter_map(move |w| {
        let message = std::mem::replace(&mut after_message, commit && matches!(w, "-m" | "--message"));
        if message || (commit && w.starts_with("--message=")) {
            return None;
        }
        match w.split_once('=') {
            Some((name, value)) if name.starts_with("--") => Some(value),
            _ => (!w.starts_with('-')).then_some(w),
        }
    })
}
