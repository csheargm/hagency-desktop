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
use std::path::Path;
use std::process::Command;

/// The only variables a step inherits from the host. Never `HOME`, never a
/// token variable, never a gh or git config location.
pub(crate) const INHERITED: [&str; 5] = ["PATH", "USER", "LOGNAME", "TMPDIR", "LANG"];
/// The commit identity a step keeps (no credential in it): the host's, so a
/// commit reads as it did before `HOME` was emptied.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "user.name"),
    ("GIT_AUTHOR_EMAIL", "user.email"),
    ("GIT_COMMITTER_NAME", "user.name"),
    ("GIT_COMMITTER_EMAIL", "user.email"),
];

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
    command.env("HOME", home).env("GH_CONFIG_DIR", gh_config);
    command.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1");
    if let Some(token) = token {
        command.env(crate::agent_env::GITHUB_TOKEN_VARIABLE, token);
    }
    command
}

/// The host's commit identity for a step that may commit: the host's own
/// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` when set, else its global `user.name`
/// and `user.email` (read here, as the step's `HOME` holds no config).
pub(crate) fn with_identity(command: &mut Command) -> &mut Command {
    for (name, key) in IDENTITY {
        let value = std::env::var_os(name).or_else(|| global_config(key));
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            command.env(name, value);
        }
    }
    command
}

/// `git config --global --get <key>` of the host, or `None`. Reading config
/// runs no helper, so this is safe with the owner's `HOME`.
fn global_config(key: &str) -> Option<OsString> {
    let out = Command::new("git").args(["config", "--global", "--get", key]).env("GIT_CONFIG_NOSYSTEM", "1").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let value = text.trim_end_matches(['\r', '\n']);
    (out.status.success() && !value.is_empty()).then(|| OsString::from(value))
}

#[cfg(test)]
#[path = "step_env_tests.rs"]
mod tests;
