//! The one environment every host-run step for an Agent starts from: the
//! host's network steps ([`super::host_git::host_command`]) and the pinned
//! commands of Codex's host tool ([`super::codex_step`]).
//!
//! A cleared environment alone is not enough. With the owner's `HOME`, `gh`
//! finds the owner's login in the macOS keychain (`gh:github.com`) even with
//! an empty `GH_CONFIG_DIR`, and git reads `~/.gitconfig` (a `store` or
//! `osxkeychain` credential helper). A step with no token of its own then
//! acted as the owner (Nov 17: a review post with no Agent token reached
//! GitHub as the owner). So `HOME` is a path that holds nothing, and the
//! Agent's token is the one GitHub credential a step can see.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variables a step inherits from the host besides the absolute entries
/// of its `PATH`. Never `HOME`, never a token variable, never a gh or git
/// config location.
pub(crate) const INHERITED: [&str; 4] = ["USER", "LOGNAME", "TMPDIR", "LANG"];
/// The identity config a step keeps from the host's global config (no
/// credential in it), so a commit reads as it did with the owner's `HOME`.
const IDENTITY_KEYS: &str = "^(user|author|committer)\\.(name|email)$";
/// Why a tokenless `gh` step does not run: gh would fall back to a stored
/// login (the OS keyring), which may be the owner's.
pub(crate) const NO_TOKEN: &str = "this Agent has no GitHub token of its own";

/// [`isolate_with`] from this process's environment.
pub(crate) fn isolate<'a>(command: &'a mut Command, home: &Path, gh_config: &Path, token: Option<&str>) -> &'a mut Command {
    isolate_with(command, home, gh_config, token, |name| std::env::var_os(name))
}

/// Clear `command`'s environment and give it only [`INHERITED`] from
/// `parent`, `home` as `HOME` (owner-only and empty, or absent), `gh_config`
/// as gh's config, git with no system config and no prompts, and `token`
/// (the Agent's) as `GH_TOKEN`: no token means `gh` is signed out.
pub(crate) fn isolate_with<'a>(command: &'a mut Command, home: &Path, gh_config: &Path, token: Option<&str>, parent: impl Fn(&str) -> Option<OsString>) -> &'a mut Command {
    command.env_clear();
    for key in INHERITED {
        if let Some(value) = parent(key) {
            command.env(key, value);
        }
    }
    // A relative entry (`.`, empty) would find a `git` or `gh` in the
    // Agent's workspace, the step's working directory (GPT review, Nov 18).
    if let Some(path) = parent("PATH") {
        command.env("PATH", std::env::join_paths(std::env::split_paths(&path).filter(|p| p.is_absolute())).unwrap_or_default());
    }
    command.env("HOME", home).env("GH_CONFIG_DIR", gh_config);
    command.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1");
    if let Some(token) = token.filter(|t| !t.trim().is_empty()) {
        command.env(crate::agent_env::GITHUB_TOKEN_VARIABLE, token);
    }
    command
}

/// Whether pinned line `command` runs `gh`: its first word after the leading
/// `NAME=value` assignments (pinned lines are host-produced, never the model's).
pub(crate) fn runs_gh(command: &str) -> bool {
    command.split_whitespace().find(|w| !is_assignment(w)) == Some("gh")
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
}

/// Create `home` (owner-only) holding a `.gitconfig` with only `entries`,
/// and return that file: the step's global config, so a checkout's own
/// identity still wins over it.
pub(crate) fn write_global_config(home: &Path, entries: &[(String, String)]) -> Result<PathBuf, String> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(home).map_err(|e| format!("step home: {e}"))?;
    let file = home.join(".gitconfig");
    std::fs::write(&file, "").map_err(|e| format!("step config: {e}"))?;
    for (key, value) in entries {
        let status = Command::new("git").arg("config").arg("--file").arg(&file).arg(key).arg(value).env("GIT_CONFIG_NOSYSTEM", "1").status();
        if !status.is_ok_and(|s| s.success()) {
            return Err(format!("step config: could not set {key}"));
        }
    }
    Ok(file)
}

/// The host's global commit identity ([`IDENTITY_KEYS`]). Reading config
/// runs no helper, so this is safe with the owner's `HOME`; none is fine.
pub(crate) fn host_identity() -> Vec<(String, String)> {
    let Ok(out) = Command::new("git").args(["config", "--global", "--includes", "-z", "--get-regexp", IDENTITY_KEYS]).env("GIT_CONFIG_NOSYSTEM", "1").output() else {
        // No git to read with: the step has no global identity, as git itself would not.
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).split('\0').filter_map(|entry| entry.split_once('\n')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

#[cfg(test)]
#[path = "step_env_tests.rs"]
mod tests;
