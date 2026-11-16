use super::*;
use crate::watch_trust::{TrustedStep, pinned_line};

const REPO: &str = "acme/app";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
fn commit(dir: &Path, message: &str) {
    git(dir, &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "--allow-empty", "-m", message]);
}

/// A workspace with checkout `app` (branch `docs/faq`, remote `origin` =
/// github acme/app), bare `target.git` holding `main`, and bare `evil.git`.
struct World {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: PathBuf,
    target: PathBuf,
    evil: PathBuf,
    marker: PathBuf,
}
fn world() -> World {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (app, target, evil) = (root.join("app"), root.join("target.git"), root.join("evil.git"));
    std::fs::create_dir(&app).unwrap();
    git(&root, &["init", "-q", "--bare", "target.git"]);
    git(&root, &["init", "-q", "--bare", "evil.git"]);
    git(&app, &["init", "-q", "-b", "main"]);
    commit(&app, "init");
    git(&app, &["push", "-q", target.to_str().unwrap(), "main"]);
    git(&app, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    git(&app, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&app, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
    git(&app, &["checkout", "-q", "-b", "docs/faq"]);
    commit(&app, "docs: faq");
    let marker = root.join("ran");
    World { _temp: temp, root, app, target, evil, marker }
}
fn dest(w: &World) -> Dest {
    Dest { url: format!("file://{}", w.target.display()), protocol: "file" }
}

/// Config the Agent could write after the check: every key here would
/// redirect the network step or run a program if the host's git read it.
fn poison(w: &World) {
    let target_url = dest(w).url;
    let evil_url = format!("file://{}", w.evil.display());
    let touch = format!("!touch {}", w.marker.display());
    let included = w.root.join("included.cfg");
    let hooks = w.root.join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    for hook in ["pre-push", "reference-transaction", "post-checkout"] {
        let path = hooks.join(hook);
        std::fs::write(&path, format!("#!/bin/sh\ntouch {}\n", w.marker.display())).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    std::fs::write(&included, format!("[core]\n\thooksPath = {}\n[url \"{evil_url}\"]\n\tpushInsteadOf = {target_url}\n", hooks.display())).unwrap();
    git(&w.app, &["config", &format!("url.{evil_url}.insteadOf"), &target_url]);
    git(&w.app, &["config", "credential.helper", &touch]);
    git(&w.app, &["config", "core.sshCommand", &format!("touch {}", w.marker.display())]);
    git(&w.app, &["config", "uploadpack.packObjectsHook", &format!("touch {}", w.marker.display())]);
    git(&w.app, &["config", "include.path", included.to_str().unwrap()]);
}

fn push_step(w: &World) -> HostStep {
    HostStep::Push { dir: w.app.clone(), remote: "origin".into(), branch: "docs/faq".into(), repo: REPO.into(), upstream: true }
}

#[test]
fn the_host_push_reads_no_checkout_config() {
    let w = world();
    poison(&w);
    let done = push_step(&w).run_to(&dest(&w)).unwrap();
    assert!(done.starts_with("Pushed docs/faq"), "{done}");
    let sha = git(&w.app, &["rev-parse", "refs/heads/docs/faq"]);
    assert_eq!(git(&w.target, &["rev-parse", "refs/heads/docs/faq"]), sha, "the push went to the checked destination");
    assert_eq!(git(&w.evil, &["for-each-ref"]), "", "insteadOf/pushInsteadOf were not followed");
    assert!(!w.marker.exists(), "no helper, hook or ssh command ran");
    assert_eq!(git(&w.app, &["rev-parse", "refs/remotes/origin/docs/faq"]), sha, "-u records the upstream");
}

#[test]
fn the_host_fetch_reads_no_checkout_config_and_hands_over_objects() {
    let w = world();
    let other = w.root.join("other");
    git(&w.root, &["clone", "-q", "-b", "main", w.target.to_str().unwrap(), "other"]);
    commit(&other, "upstream change");
    git(&other, &["push", "-q", "origin", "main"]);
    let upstream = git(&other, &["rev-parse", "HEAD"]);
    poison(&w);
    let step = HostStep::Fetch { dir: w.app.clone(), remote: "origin".into(), repo: REPO.into() };
    let done = step.run_to(&dest(&w)).unwrap();
    assert!(done.starts_with("Fetched"), "{done}");
    assert_eq!(git(&w.app, &["rev-parse", "refs/remotes/origin/main"]), upstream);
    git(&w.app, &["cat-file", "-e", &format!("{upstream}^{{commit}}")]);
    assert!(!w.marker.exists(), "no helper, hook or ssh command ran");
}

#[test]
fn a_failed_host_step_is_reported_as_a_failed_call() {
    let w = world();
    let bad = Dest { url: format!("file://{}", w.root.join("missing.git").display()), protocol: "file" };
    let result = push_step(&w).run_to(&bad);
    assert!(result.is_err());
    assert!(report_command(&result).ends_with("exit 1"));
    let ok = report_command(&Ok("Pushed it's done".into()));
    let out = Command::new("sh").arg("-c").arg(&ok).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Pushed it's done\n");
    // Only https leaves the machine in production.
    let https = host_command(&w.root, Some("https"));
    let args: Vec<_> = https.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.contains(&"protocol.allow=never".to_owned()) && args.contains(&"protocol.https.allow=always".to_owned()));
}

#[test]
fn the_host_branch_step_rechecks_the_config_and_runs_no_hook() {
    let w = world();
    let step = HostStep::Branch { dir: w.app.clone(), name: "fix/27".into(), start: "refs/remotes/origin/main".into() };
    git(&w.app, &["config", "filter.x.smudge", &format!("touch {}", w.marker.display())]);
    assert!(step.run().is_err(), "a filter driver in config refuses");
    git(&w.app, &["config", "--unset", "filter.x.smudge"]);
    assert!(step.run().unwrap().contains("fix/27"));
    assert_eq!(git(&w.app, &["branch", "--show-current"]), "fix/27");
    assert!(!w.marker.exists());
}

#[test]
fn fix_steps_in_the_watched_checkout_are_the_hosts() {
    let w = world();
    let fetch = Some(TrustedStep::Host(HostStep::Fetch { dir: w.app.clone(), remote: "origin".into(), repo: REPO.into() }));
    assert_eq!(pinned_line("git -C app fetch origin", &w.root, REPO), fetch);
    assert_eq!(pinned_line("git -C app fetch", &w.root, REPO), fetch);
    assert_eq!(pinned_line("git fetch origin", &w.app, REPO), fetch, "inside the checkout");
    let branch = Some(TrustedStep::Host(HostStep::Branch { dir: w.app.clone(), name: "fix/27".into(), start: "refs/remotes/origin/main".into() }));
    assert_eq!(pinned_line("git -C app checkout -b fix/27 origin/main", &w.root, REPO), branch);
    assert_eq!(pinned_line("git switch -c fix/27 origin/main", &w.app, REPO), branch);
    git(&w.app, &["remote", "add", "fork", "https://github.com/mallory/app.git"]);
    for line in [
        "git fetch origin",
        "git -C app fetch fork",
        "git -C app fetch origin main",
        "git -C app fetch --all",
        "git -C app fetch https://github.com/acme/app.git",
        "git -C app fetch origin && git -C app checkout -b fix/27 origin/main",
        "git -C app checkout -b main origin/main",
        "git -C app checkout -b docs/faq origin/main",
        "git -C app checkout -b fix/27 origin/docs/faq",
        "git -C app checkout -b fix/27 fork/main",
        "git -C app checkout -b fix/27",
        "git -C app checkout main",
        "git -C /tmp fetch origin",
    ] {
        assert_eq!(pinned_line(line, &w.root, REPO), None, "{line}");
    }
    assert_eq!(pinned_line("git push -u origin docs/faq", &w.app, REPO), None, "a bare push still asks");
}

/// Regression guard for the prompt: the fix steps the note teaches are the
/// shapes trust answers.
#[test]
fn the_taught_fix_steps_are_the_trusted_ones() {
    let w = world();
    for taught in hagency_runtime::TRUSTED_FIX_STEPS {
        let line = taught.replace("<repo path>", "app").replace("<branch>", "fix/27").replace("<default branch>", "main");
        assert!(pinned_line(&line, &w.root, REPO).is_some(), "{line}");
        assert!(hagency_runtime::AGENT_MERGE_NOTE.contains(taught));
    }
    assert!(hagency_runtime::AGENT_MERGE_NOTE.contains("Edit or Write tool"));
}
