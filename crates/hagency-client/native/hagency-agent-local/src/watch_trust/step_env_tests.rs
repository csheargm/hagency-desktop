use super::*;
use std::path::PathBuf;

/// A parent environment holding every credential and config location a
/// step could otherwise borrow the owner's GitHub login from.
fn owner_env(name: &str) -> Option<OsString> {
    let value = match name {
        "PATH" => "/usr/bin:/bin",
        "HOME" => "/Users/owner",
        "GH_TOKEN" => "owner-gh-token",
        "GITHUB_TOKEN" => "owner-github-token",
        "GH_ENTERPRISE_TOKEN" => "owner-enterprise-token",
        "GITHUB_ENTERPRISE_TOKEN" => "owner-enterprise-token-2",
        "GH_CONFIG_DIR" => "/Users/owner/.config/gh",
        "XDG_CONFIG_HOME" => "/Users/owner/.config",
        "GIT_CONFIG_GLOBAL" => "/Users/owner/.gitconfig",
        "GIT_ASKPASS" => "/Users/owner/askpass",
        _ => return None,
    };
    Some(value.into())
}

fn step_env(token: Option<&str>) -> String {
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "env"]);
    isolate_with(&mut sh, Path::new("/nonexistent/home"), Path::new("/nonexistent/gh"), token, owner_env);
    String::from_utf8(sh.output().unwrap().stdout).unwrap()
}

#[test]
fn a_parent_token_or_config_never_reaches_the_step() {
    let env = step_env(None);
    assert!(!env.contains("owner-"), "an owner credential reached the step: {env}");
    assert!(!env.contains("/Users/owner"), "an owner config location reached the step: {env}");
    assert!(env.lines().any(|l| l == "HOME=/nonexistent/home"), "{env}");
    assert!(env.lines().any(|l| l == "GH_CONFIG_DIR=/nonexistent/gh"), "{env}");
    assert!(!env.lines().any(|l| l.starts_with("GH_TOKEN=") || l.starts_with("GITHUB_TOKEN=")), "{env}");
    assert!(env.lines().any(|l| l == "GIT_CONFIG_NOSYSTEM=1") && env.lines().any(|l| l == "GIT_CONFIG_GLOBAL=/dev/null"), "{env}");
}

#[test]
fn the_agents_token_is_the_only_credential() {
    let env = step_env(Some("agent-token"));
    assert!(env.lines().any(|l| l == "GH_TOKEN=agent-token"), "{env}");
    assert!(!env.contains("owner-"), "{env}");
}

fn gh_available() -> bool {
    Command::new("gh").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn empty_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("hagency-step-env-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}

/// Regression (Nov 17): with the owner's `HOME`, `gh` read the owner's
/// keychain login although `GH_CONFIG_DIR` was empty. With no token of its
/// own the step's `gh` has no login at all (checked locally: `gh auth token`
/// makes no network call). On a host without gh or a keychain login this
/// passes trivially.
#[test]
fn no_token_means_gh_is_signed_out() {
    if !gh_available() {
        return;
    }
    let dir = empty_dir("gh");
    let mut gh = Command::new("gh");
    gh.args(["auth", "token", "--hostname", "github.com"]);
    isolate(&mut gh, &dir.join("home"), &dir, None);
    let out = gh.output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!out.status.success() && out.stdout.is_empty(), "gh found a login the step must not have");
}

#[test]
fn identity_comes_from_the_host_not_the_steps_home() {
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "env"]);
    isolate_with(&mut sh, Path::new("/nonexistent/home"), Path::new("/nonexistent/gh"), None, owner_env);
    with_identity(&mut sh);
    let env = String::from_utf8(sh.output().unwrap().stdout).unwrap();
    let host_name = std::env::var_os("GIT_AUTHOR_NAME").or_else(|| global_config("user.name"));
    let step_name = env.lines().find_map(|l| l.strip_prefix("GIT_AUTHOR_NAME=")).map(OsString::from);
    assert_eq!(step_name, host_name.filter(|n| !n.is_empty()));
}
