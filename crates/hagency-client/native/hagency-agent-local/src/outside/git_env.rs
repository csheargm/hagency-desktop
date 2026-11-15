//! Git configuration forced on the Claude runtime's commands (command-line
//! level, so it beats `.git/config`): no repository hooks, no fsmonitor helper,
//! no commit signing program. External diff and textconv drivers are left as
//! configured (blanking `diff.external` breaks `git diff`); the outside-mode
//! classifier refuses `--ext-diff`/`--textconv` and in-workspace writes to git
//! metadata instead. Not given to Codex: it drops `*KEY*` variables from its
//! commands, and a count without its keys makes every git command fail.
pub const GIT_HELPERS_OFF_ENV: [(&str, &str); 7] = [
    ("GIT_CONFIG_COUNT", "3"),
    ("GIT_CONFIG_KEY_0", "core.hooksPath"),
    ("GIT_CONFIG_VALUE_0", "/dev/null"),
    ("GIT_CONFIG_KEY_1", "core.fsmonitor"),
    ("GIT_CONFIG_VALUE_1", "false"),
    ("GIT_CONFIG_KEY_2", "commit.gpgSign"),
    ("GIT_CONFIG_VALUE_2", "false"),
];

#[cfg(test)]
mod tests {
    use super::GIT_HELPERS_OFF_ENV;

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
}
