//! Which shell commands "Always allow" must never remember: anything that can
//! reach the network or write somewhere other than this machine.
//!
//! The demo promise is "pushes and network steps never become automatic". A
//! line is read with the strict `outside::shell` reader; a line it cannot read
//! (`$(...)`, backticks, variables, other redirects) is treated as networked,
//! so it fails closed. Every word of every segment is checked, so a network
//! program behind `env`, `xargs`, `find -exec` or a `bash -c` wrapper still
//! counts. Over-refusing (e.g. a commit message containing the word `push`)
//! is accepted: the owner then approves once.
//!
//! Residual risk (not closed here): a remembered command runs whatever its
//! scripts, `PATH`, build files and git config say at the time it runs, so
//! `npm test` or `cargo test` remembered today runs tomorrow's code. Rules
//! expire after `remembered::RULE_LIFETIME_SECS` to bound that window.
use crate::outside::{rules::READ_ONLY_GH, shell};

/// Programs whose whole purpose is talking to another host.
const NETWORK_PROGRAMS: [&str; 18] = [
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "telnet", "ftp", "socat", "aria2c", "http",
    "https", "xh", "open", "osascript",
];
/// Interpreters that run inline code this reader cannot see into.
const INLINE_CODE: [&str; 14] =
    ["eval", "python", "python3", "node", "perl", "ruby", "php", "deno", "bun", "source", "awk", "gawk", "mawk", "nawk"];
/// Git options that load config (aliases, `core.sshCommand`) from the line.
const GIT_CONFIG_OPTIONS: [&str; 2] = ["-c", "--config-env"];
/// Git subcommands that talk to a remote (or can, via helpers).
const GIT_NETWORK: [&str; 10] =
    ["push", "fetch", "pull", "clone", "ls-remote", "remote", "submodule", "send-email", "request-pull", "archive"];
/// Package and container tools, and their verbs that download or publish.
const PACKAGE_TOOLS: [&str; 13] =
    ["npm", "npx", "pnpm", "yarn", "pip", "pip3", "cargo", "gem", "go", "brew", "docker", "podman", "uv"];
const PACKAGE_NETWORK: [&str; 15] = [
    "install", "i", "ci", "add", "publish", "push", "pull", "login", "get", "update", "upgrade", "download", "yank",
    "owner", "exec",
];
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];
/// How deep `bash -c "bash -c ..."` is followed before giving up (fail closed).
const MAX_DEPTH: usize = 3;

fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// True when this shell line could reach the network (or cannot be read).
pub(crate) fn line_reaches_network(line: &str) -> bool {
    line_at(line, 0)
}

/// True when this exact argv could reach the network.
pub(crate) fn argv_reaches_network(argv: &[&str]) -> bool {
    words_reach_network(argv, 0)
}

fn line_at(line: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Some(parsed) = shell::parse(line) else { return true };
    // A heredoc body is input this reader does not check (`make -f - <<EOF`).
    if parsed.heredoc_at.is_some() {
        return true;
    }
    parsed.segments.iter().any(|segment| {
        let words: Vec<&str> = segment.iter().map(String::as_str).collect();
        words_reach_network(&words, depth)
    })
}

/// Checks the words, then every name-like token inside them, so a network
/// step hidden in one argument (`awk '{system("curl x")}'`, a git alias
/// `'!git push'`) still counts.
fn words_reach_network(words: &[&str], depth: usize) -> bool {
    let tokens: Vec<&str> = words
        .iter()
        .flat_map(|w| w.split(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c))))
        .filter(|t| !t.is_empty())
        .collect();
    any_reaches(words, depth) || any_reaches(&tokens, depth)
}

fn any_reaches(words: &[&str], depth: usize) -> bool {
    words.iter().enumerate().any(|(at, word)| {
        let rest = &words[at + 1..];
        match base(word) {
            w if NETWORK_PROGRAMS.contains(&w) || INLINE_CODE.contains(&w) => true,
            w if SHELLS.contains(&w) => shell_wrapper_reaches(rest, depth),
            "git" => rest.iter().any(|r| GIT_NETWORK.contains(r) || GIT_CONFIG_OPTIONS.iter().any(|o| *r == *o || r.starts_with(&format!("{o}=")))),
            "gh" => !matches!(rest, [area, verb, ..] if READ_ONLY_GH.contains(&(*area, *verb))),
            w if PACKAGE_TOOLS.contains(&w) => rest.iter().any(|r| PACKAGE_NETWORK.contains(r)),
            _ => false,
        }
    })
}

/// `bash -c '<line>'` / `zsh -lc '<line>'`: read the script it runs. A shell
/// started any other way (a script file, stdin) runs code this cannot see.
fn shell_wrapper_reaches(rest: &[&str], depth: usize) -> bool {
    match rest {
        ["-c" | "-lc", script, ..] => line_at(script, depth + 1),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_network_tools_and_writes_elsewhere_are_networked() {
        for line in [
            "git push origin fix-6", "cargo test && git push", "git fetch", "git pull --rebase", "git clone x",
            "git ls-remote origin", "git remote add o u", "gh pr merge 1", "gh pr create", "gh api repos/x",
            "gh run view 1", "curl -sS https://x", "wget x", "ssh host ls", "scp a b:c", "rsync -a a b:c",
            "nc host 80", "npm install", "pip install x", "cargo install ripgrep", "cargo publish", "docker push i",
            "env FOO=1 curl x", "find . -exec curl x ;", "bash -c 'git push'", "zsh -lc \"curl x\"",
            "bash -c \"bash -c 'git push'\"", "bash script.sh", "python -c 'print(1)'", "echo $(curl x)",
            "`curl x`", "cat a > b", "awk 'BEGIN { system(\"curl https://x\") }'",
            "git -c alias.p='!git push origin main' p", "make -f - push <<'EOF'\npush:\n\tgit push origin main\nEOF\n",
            "sed 's/x/y/' f | xargs /usr/bin/curl",
        ] {
            assert!(line_reaches_network(line), "{line} must not be remembered");
        }
    }

    #[test]
    fn local_steps_and_gh_reads_are_not() {
        for line in ["cargo test", "cargo test 2>&1 | tail -20", "git status", "git commit -m fix", "gh pr view 3", "ls -la", "bash -lc 'cargo test'"] {
            assert!(!line_reaches_network(line), "{line}");
        }
        assert!(argv_reaches_network(&["git", "push"]));
        assert!(!argv_reaches_network(&["cargo", "test"]));
        assert!(argv_reaches_network(&["bash", "-lc", "git push origin x"]));
    }
}
