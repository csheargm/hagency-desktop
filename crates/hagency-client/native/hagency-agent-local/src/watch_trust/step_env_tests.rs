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
fn a_pinned_gh_line_is_recognised_and_a_git_line_is_not() {
    assert!(runs_gh("GH_HOST='github.com' gh pr review 7 --approve -R a/b -b 'x'"));
    assert!(runs_gh("GIT_CONFIG_COUNT='3' GH_HOST='github.com' gh api repos/a/b/issues"));
    assert!(runs_gh("gh pr create --repo a/b"));
    assert!(!runs_gh("GIT_CONFIG_COUNT='3' env -u GIT_DIR git -C 'app' commit -m 'fix gh thing'"));
    assert!(!runs_gh("printf %s gh"));
}

/// GPT review (Nov 18): every production gh shape a pinned line can take is
/// one the tokenless guard sees; a pinned git line is not.
#[test]
fn every_production_gh_step_is_seen_by_the_guard() {
    let review = super::super::review_post::ReviewPost::parse("gh pr review 7 -R a/b --approve -b 'Hagency verdict: APPROVE x'").unwrap();
    assert!(runs_gh(&review.command()));
    assert!(runs_gh(&super::super::read_only::command("gh issue list -R a/b")));
    assert!(runs_gh(&super::super::pin_env("gh pr create --repo a/b --head fix/1")));
    assert!(!runs_gh(&super::super::local_git::pinned_git(Path::new("/w/app"), &["commit", "-m", "gh"])));
}

#[test]
fn a_relative_path_entry_never_reaches_the_step() {
    let parent = |name: &str| (name == "PATH").then(|| OsString::from(".:/usr/bin::bin:/bin:"));
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "printf %s \"$PATH\""]);
    isolate_with(&mut sh, Path::new("/nonexistent/home"), Path::new("/nonexistent/gh"), None, parent);
    assert_eq!(String::from_utf8(sh.output().unwrap().stdout).unwrap(), "/usr/bin:/bin");
}

/// The identity a commit runs with: `dir`'s `git var` under the step's
/// global config `global` and an empty system config.
fn ident(dir: &Path, global: &Path, var: &str) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["var", var]).env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", global.parent().unwrap()).env("GIT_CONFIG_GLOBAL", global).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

/// GPT review (Nov 18): the host identity is a fallback below the
/// checkout's own; author and committer settings are kept too.
#[test]
fn the_host_identity_is_a_global_fallback_the_checkout_overrides() {
    let dir = empty_dir("ident");
    let entries = [("user.name", "Host Name"), ("user.email", "host@x"), ("committer.name", "Host Committer")].map(|(k, v)| (k.to_owned(), v.to_owned()));
    let global = write_global_config(&dir.join("home"), &entries).unwrap();
    let repo = dir.join("repo");
    assert!(Command::new("git").args(["init", "-q"]).arg(&repo).env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap().success());
    assert!(ident(&repo, &global, "GIT_AUTHOR_IDENT").starts_with("Host Name <host@x>"));
    assert!(ident(&repo, &global, "GIT_COMMITTER_IDENT").starts_with("Host Committer <host@x>"));
    assert!(Command::new("git").arg("-C").arg(&repo).args(["config", "user.name", "Checkout"]).status().unwrap().success());
    assert!(ident(&repo, &global, "GIT_AUTHOR_IDENT").starts_with("Checkout <host@x>"), "the checkout's identity wins");
    assert!(write_global_config(&dir.join("home"), &[]).is_err(), "an existing home is never reused");
    std::fs::remove_dir_all(&dir).unwrap();
}
