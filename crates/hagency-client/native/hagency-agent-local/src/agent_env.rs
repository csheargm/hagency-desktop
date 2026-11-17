//! Extra environment one Agent's own CLI runtime (Claude or Codex) starts
//! with, on top of the runtime's fixed allowlist. Today this carries only the
//! Agent's GitHub token as `GH_TOKEN`, so `gh` and `git` in that Agent's
//! sessions act with that Agent's access and no other Agent's.
//!
//! Values are secrets: `Debug` shows the variable names only.
use std::collections::BTreeMap;

/// The variable the GitHub CLI reads first.
pub const GITHUB_TOKEN_VARIABLE: &str = "GH_TOKEN";

/// Codex drops every variable whose name contains KEY, SECRET or TOKEN from
/// the commands it runs unless told not to. The runtime starts from an empty
/// environment, so the only such variables present are the ones set here.
pub const CODEX_KEEP_SECRET_VARIABLES: &str = "shell_environment_policy.ignore_default_excludes=true";

/// Git configuration forced on the Claude runtime's commands (command-line
/// level, so it beats `.git/config`): no repository hooks, no fsmonitor helper,
/// no commit signing program. External diff and textconv drivers are left as
/// configured (blanking `diff.external` breaks `git diff`); the outside-mode
/// classifier refuses `--ext-diff`/`--textconv` and in-workspace writes to git
/// metadata instead. Not given to Codex: it drops `*KEY*` variables from its
/// commands, and a count without its keys makes every git command fail.
pub const GIT_HELPERS_OFF_ENV: [(&str, &str); 7] = [
    ("GIT_CONFIG_COUNT", "3"),
    ("GIT_CONFIG_KEY_0", "core.hooksPath"),
    ("GIT_CONFIG_VALUE_0", "/dev/null"),
    ("GIT_CONFIG_KEY_1", "core.fsmonitor"),
    ("GIT_CONFIG_VALUE_1", "false"),
    ("GIT_CONFIG_KEY_2", "commit.gpgSign"),
    ("GIT_CONFIG_VALUE_2", "false"),
];

#[derive(Clone, Default, PartialEq, Eq)]
pub struct AgentEnvironment(BTreeMap<String, String>);

impl AgentEnvironment {
    /// The environment that gives the runtime this GitHub token.
    pub fn github_token(token: &str) -> Self {
        Self(BTreeMap::from([(GITHUB_TOKEN_VARIABLE.to_owned(), token.to_owned())]))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
    /// The GitHub token this Agent's runtime sees: its own, else the
    /// environment's ([`crate::demo_env::resolve`], every build). Host-side
    /// GitHub steps for the Agent use exactly this, so they act as the same login.
    pub fn agent_github_token(&self) -> Option<String> {
        self.github_token_with(|name| std::env::var(name).ok())
    }
    pub(crate) fn github_token_with(&self, env: impl Fn(&str) -> Option<String>) -> Option<String> {
        crate::demo_env::resolve(self.get(GITHUB_TOKEN_VARIABLE), env)
    }
    /// The `-c` overrides Codex needs so its commands see these variables.
    pub fn codex_overrides(&self) -> Vec<String> {
        if self.is_empty() { Vec::new() } else { vec![CODEX_KEEP_SECRET_VARIABLES.to_owned()] }
    }
}

impl std::fmt::Debug for AgentEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_names_never_values() {
        let env = AgentEnvironment::github_token("github_pat_SECRET123");
        let shown = format!("{env:?}");
        assert!(shown.contains("GH_TOKEN"));
        assert!(!shown.contains("SECRET123"));
    }

    #[test]
    fn codex_keeps_secret_variables_only_when_some_are_set() {
        assert!(AgentEnvironment::default().codex_overrides().is_empty());
        let env = AgentEnvironment::github_token("t");
        assert_eq!(env.codex_overrides(), vec![CODEX_KEEP_SECRET_VARIABLES.to_owned()]);
    }

    #[test]
    fn git_helper_overrides_stop_repository_hooks() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(repo.path()).env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap()).env("HOME", repo.path())
                .envs(GIT_HELPERS_OFF_ENV).output().unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        let hooks = repo.path().join("hk");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\ntouch ran\nexit 1\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(hooks.join("pre-commit"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(git(&["config", "core.hooksPath", "hk"]).status.success());
        let commit = git(&["-c", "user.name=x", "-c", "user.email=x@x", "commit", "-q", "--allow-empty", "-m", "m"]);
        assert!(commit.status.success(), "{commit:?}");
        assert!(!repo.path().join("ran").exists(), "a repository hook ran");
    }

    fn profile(environment: AgentEnvironment) -> crate::codex::Profile {
        crate::codex::Profile {
            executable: "/usr/bin/true".into(),
            home: std::env::temp_dir(),
            codex_home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            model: "m".into(),
            effort: "low".into(),
            shared_auth: false,
            web_search: false,
            environment,
        }
    }

    #[test]
    fn codex_launch_gets_only_its_own_agents_token() {
        let with = profile(AgentEnvironment::github_token("tok-a")).launch(&[]).unwrap();
        assert_eq!(with.environment.get(std::ffi::OsStr::new("GH_TOKEN")).map(|v| v.to_str().unwrap()), Some("tok-a"));
        let keep = std::ffi::OsString::from(CODEX_KEEP_SECRET_VARIABLES);
        assert!(with.arguments.contains(&keep), "Codex must not filter GH_TOKEN out");
        let without = profile(AgentEnvironment::default()).launch(&[]).unwrap();
        assert!(!without.environment.contains_key(std::ffi::OsStr::new("GH_TOKEN")));
        assert!(!without.arguments.contains(&keep));
    }

    /// A `bash -c`/`zsh -c` wrapper the merge gate reads must not source init
    /// files: the launch starts from an empty environment, so BASH_ENV, ENV,
    /// ZDOTDIR and the like never reach Codex, whatever the parent has.
    #[test]
    fn codex_launch_carries_no_shell_startup_variables() {
        let launch = profile(AgentEnvironment::github_token("tok")).launch(&[]).unwrap();
        let allowed = ["HOME", "CODEX_HOME", "PATH", "USER", "TMPDIR", "SystemRoot", GITHUB_TOKEN_VARIABLE, crate::demo_env::GITHUB_TOKEN_ENV];
        for name in launch.environment.keys() {
            assert!(allowed.iter().any(|a| std::ffi::OsStr::new(a) == name), "{name:?} is not on the launch allowlist");
        }
        for name in ["BASH_ENV", "ENV", "ZDOTDIR", "SHELLOPTS", "BASHOPTS", "PROMPT_COMMAND"] {
            assert!(!launch.environment.contains_key(std::ffi::OsStr::new(name)), "{name}");
        }
    }
}
