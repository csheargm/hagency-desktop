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
    git(&root, &["init", "-q", "--bare", "-b", "main", "target.git"]);
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

/// Run what the host left for the Agent's call, as the Agent's shell would.
fn agent_runs(command: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(command).output().unwrap();
    assert!(out.status.success(), "{command}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn push_step(w: &World) -> HostStep {
    HostStep::Push { dir: w.app.clone(), remote: "origin".into(), branch: "docs/faq".into(), repo: REPO.into(), upstream: true }
}

#[test]
fn the_host_push_reads_no_checkout_config() {
    let w = world();
    poison(&w);
    let command = push_step(&w).run_to(&dest(&w), &ctx()).unwrap();
    assert_eq!(git(&w.app, &["for-each-ref", "refs/remotes/origin/docs"]), "", "the host wrote nothing into the checkout");
    let done = agent_runs(&command);
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
    let command = step.run_to(&dest(&w), &ctx()).unwrap();
    assert_ne!(git(&w.app, &["rev-parse", "refs/remotes/origin/main"]), upstream, "the host wrote nothing into the checkout");
    let done = agent_runs(&command);
    assert!(done.starts_with("Fetched"), "{done}");
    assert_eq!(git(&w.app, &["rev-parse", "refs/remotes/origin/main"]), upstream);
    git(&w.app, &["cat-file", "-e", &format!("{upstream}^{{commit}}")]);
    assert!(!w.marker.exists(), "no helper, hook or ssh command ran");
}

#[test]
fn a_failed_host_step_is_reported_as_a_failed_call() {
    let w = world();
    let bad = Dest { url: format!("file://{}", w.root.join("missing.git").display()), protocol: "file" };
    let result = push_step(&w).run_to(&bad, &ctx());
    assert!(result.is_err());
    assert!(report_command(&result).ends_with("exit 1"));
    assert_eq!(agent_runs(&say("Pushed it's done")), "Pushed it's done\n");
    // Only https leaves the machine in production.
    let https = host_command(&w.root, Some("https"), Some("agent-token"));
    let args: Vec<_> = https.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.contains(&"protocol.allow=never".to_owned()) && args.contains(&"protocol.https.allow=always".to_owned()));
}

#[test]
fn the_branch_step_is_pinned_and_refuses_filter_config() {
    let w = world();
    let line = "git -C app checkout -b fix/27 origin/main";
    git(&w.app, &["config", "filter.x.smudge", &format!("touch {}", w.marker.display())]);
    assert_eq!(pinned_line(line, &w.root, REPO), None, "a filter driver in config asks");
    git(&w.app, &["config", "--unset", "filter.x.smudge"]);
    std::fs::write(w.app.join(".git/info/attributes"), "* filter=x\n").unwrap();
    assert_eq!(pinned_line(line, &w.root, REPO), None, "info/attributes asks");
    std::fs::remove_file(w.app.join(".git/info/attributes")).unwrap();
    let Some(TrustedStep::Command(pinned)) = pinned_line(line, &w.root, REPO) else { panic!("trusted") };
    assert!(pinned.contains("GIT_ATTR_SOURCE=4b825dc642cb6eb9a060e54bf8d69288fbee4904") && pinned.contains("'core.attributesFile=/dev/null'"));
    let out = Command::new("sh").arg("-c").arg(&pinned).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(git(&w.app, &["branch", "--show-current"]), "fix/27");
    assert!(!w.marker.exists());
}

/// GPT review (Nov 16): the remote's own default branch counts, not the
/// checkout's forgeable `origin/HEAD`.
#[test]
fn the_host_push_refuses_the_remotes_default_branch() {
    let w = world();
    git(&w.target, &["symbolic-ref", "HEAD", "refs/heads/docs/faq"]);
    let result = push_step(&w).run_to(&dest(&w), &ctx());
    assert!(result.as_ref().is_err_and(|e| e.contains("default branch")), "{result:?}");
    assert_eq!(git(&w.target, &["for-each-ref", "refs/heads/docs"]), "");
}

#[test]
fn the_host_refuses_a_checkout_whose_storage_is_elsewhere() {
    let w = world();
    let elsewhere = w.root.join("elsewhere");
    std::fs::rename(w.app.join(".git"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, w.app.join(".git")).unwrap();
    let step = HostStep::Fetch { dir: w.app.clone(), remote: "origin".into(), repo: REPO.into() };
    assert!(step.run_to(&dest(&w), &ctx()).is_err(), "a linked .git");
    std::fs::remove_file(w.app.join(".git")).unwrap();
    std::fs::write(w.app.join(".git"), format!("gitdir: {}\n", elsewhere.display())).unwrap();
    assert!(step.run_to(&dest(&w), &ctx()).is_err(), "a .git file");
}

#[test]
fn fix_steps_in_the_watched_checkout_are_the_hosts() {
    let w = world();
    let fetch = Some(TrustedStep::Host(HostStep::Fetch { dir: w.app.clone(), remote: "origin".into(), repo: REPO.into() }));
    assert_eq!(pinned_line("git -C app fetch origin", &w.root, REPO), fetch);
    assert_eq!(pinned_line("git -C app fetch", &w.root, REPO), fetch);
    assert_eq!(pinned_line("git fetch origin", &w.app, REPO), fetch, "inside the checkout");
    let branch = pinned_line("git -C app checkout -b fix/27 origin/main", &w.root, REPO);
    assert!(matches!(&branch, Some(TrustedStep::Command(c)) if c.contains("'refs/remotes/origin/main'")), "{branch:?}");
    assert!(pinned_line("git switch -c fix/27 origin/main", &w.app, REPO).is_some());
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

fn ctx() -> HostCtx {
    HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true))
}

#[test]
fn the_host_step_runs_as_the_agent_never_the_daemons_login() {
    let w = world();
    let https = host_command(&w.root, Some("https"), Some("agent-token"));
    let envs: Vec<_> = https.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
    assert!(envs.contains(&("GH_TOKEN".into(), Some("agent-token".into()))));
    assert!(envs.iter().all(|(k, _)| k != "GITHUB_TOKEN"), "the daemon's token variable is never inherited");
    let config = envs.iter().find(|(k, _)| k == "GH_CONFIG_DIR").and_then(|(_, v)| v.clone()).unwrap();
    assert!(config.starts_with(w.root.to_str().unwrap()), "gh reads no stored login: {config}");
    assert_eq!(push_step(&w).run(&ctx()), Err("this Agent has no GitHub token of its own".into()));
}

#[test]
fn a_push_is_refused_when_trust_was_turned_off_after_the_check() {
    let w = world();
    let off = HostCtx::new(None, std::sync::Arc::default(), Box::new(|| false));
    assert_eq!(push_step(&w).run_to(&dest(&w), &off), Err("the owner no longer lets this Agent push here".into()));
    assert_eq!(git(&w.target, &["for-each-ref", "refs/heads/docs"]), "", "nothing was pushed");
}

/// Regression (Nov 17, issue #35): the step script ran under the agent's zsh,
/// where `status` is read-only, so every host fetch failed. It must run the
/// same under zsh, bash and sh: say `done` only on success, always clean up.
#[test]
fn clean_up_script_runs_under_zsh_bash_and_sh() {
    let dir = std::env::temp_dir().join(format!("hagency-rc-{}", std::process::id()));
    for shell in ["zsh", "bash", "sh"] {
        if std::process::Command::new(shell).arg("-c").arg("true").status().is_err() { continue; }
        for (step, ok) in [("true", true), ("false", false)] {
            std::fs::create_dir_all(&dir).unwrap();
            let script = super::then_clean_up(step, &dir.to_string_lossy(), "echo done");
            let out = std::process::Command::new(shell).arg("-c").arg(&script).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert_eq!(text.trim() == "done", ok, "{shell} {step}: {text} {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(out.status.success(), ok, "{shell} {step}");
            assert!(!dir.exists(), "{shell} {step}: scratch copy left behind");
        }
    }
}
