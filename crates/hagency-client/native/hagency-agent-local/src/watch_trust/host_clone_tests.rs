use super::*;
use crate::watch_trust::{TrustedStep, pinned_line};
use std::process::Command;

const REPO: &str = "acme/app";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
fn ctx() -> HostCtx {
    HostCtx::new(None, std::sync::Arc::default(), Box::new(|| true))
}
fn agent_runs(command: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(command).output().unwrap();
    assert!(out.status.success(), "{command}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A bare `target.git` (stands in for github acme/app) with `main`, whose
/// tree carries a `.gitattributes` filter and a hook-like script.
fn upstream(root: &Path) -> Dest {
    let seed = root.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "hi\n").unwrap();
    std::fs::write(seed.join(".gitattributes"), "*.md filter=x\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "-m", "init"]);
    git(root, &["clone", "-q", "--bare", seed.to_str().unwrap(), "target.git"]);
    Dest { url: format!("file://{}", root.join("target.git").display()), protocol: "file" }
}

#[test]
fn clone_requests_name_the_watched_repo_and_a_new_folder_inside() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let clone = |line| pinned_line(line, &root, REPO);
    let into = |name: &str| Some(TrustedStep::Host(HostStep::Clone { target: root.join(name), repo: REPO.into() }));
    assert_eq!(clone("gh repo clone acme/app ./app"), into("app"));
    assert_eq!(clone("gh repo clone acme/app"), into("app"));
    assert_eq!(clone("git clone https://github.com/acme/app.git"), into("app"));
    assert_eq!(clone("git clone -q git@github.com:acme/app.git work"), into("work"));
    std::fs::create_dir(root.join("taken")).unwrap();
    std::os::unix::fs::symlink("/tmp", root.join("out")).unwrap();
    std::fs::create_dir(root.join("taken/.git")).unwrap();
    std::os::unix::fs::symlink(root.join("taken/.git"), root.join("gitdir")).unwrap();
    for asks in [
        "gh repo clone acme/other app",
        "git clone https://gitlab.com/acme/app.git",
        "git clone https://github.com/acme/app.git /tmp/app",
        "git clone https://github.com/acme/app.git ../app",
        "git clone https://github.com/acme/app.git out/app",
        "git clone https://github.com/acme/app.git taken",
        // GPT review: never into git metadata.
        "git clone https://github.com/acme/app.git taken/.git/x",
        "gh repo clone acme/app gitdir/x",
        "git clone --upload-pack=x https://github.com/acme/app.git app",
        "git clone -c core.hooksPath=x https://github.com/acme/app.git app",
        "gh repo clone acme/app app -- --upload-pack=x",
        "git clone https://github.com/acme/app.git app && cd app",
    ] {
        assert_eq!(clone(asks), None, "{asks}");
    }
}

#[test]
fn the_host_clone_runs_no_filter_or_hook_and_points_origin_at_github() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let dest = upstream(&root);
    let marker = root.join("ran");
    // The owner's own global config defines a smudge filter the cloned
    // tree's `.gitattributes` names: attributes come from the empty tree.
    let global = root.join(".gitconfig");
    std::fs::write(&global, format!("[filter \"x\"]\n\tsmudge = touch {}\n", marker.display())).unwrap();
    let step = HostStep::Clone { target: root.join("app"), repo: REPO.into() };
    let command = step.run_to(&dest, &ctx()).unwrap();
    assert!(!root.join("app").exists(), "the host wrote nothing into the workspace");
    let done = agent_runs(&format!("HOME={} {command}", root.display()));
    assert!(done.starts_with("Cloned acme/app"), "{done}");
    let app = root.join("app");
    assert_eq!(std::fs::read_to_string(app.join("README.md")).unwrap(), "hi\n");
    assert_eq!(git(&app, &["remote", "get-url", "origin"]), "https://github.com/acme/app.git");
    assert_eq!(git(&app, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]), "origin/main");
    assert!(!marker.exists(), "a smudge filter ran");
    assert!(crate::watch_trust::local_git::own_checkout(&app, REPO), "the new clone is the watched repository's own checkout");
}

#[test]
fn the_host_pull_fast_forwards_the_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let dest = upstream(&root);
    let app = root.join("app");
    agent_runs(&HostStep::Clone { target: app.clone(), repo: REPO.into() }.run_to(&dest, &ctx()).unwrap());
    let other = root.join("other");
    git(&root, &["clone", "-q", root.join("target.git").to_str().unwrap(), "other"]);
    git(&other, &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "--allow-empty", "-m", "upstream"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let head = git(&other, &["rev-parse", "HEAD"]);
    let step = HostStep::Pull { dir: app.clone(), remote: "origin".into(), branch: "main".into(), repo: REPO.into() };
    agent_runs(&step.run_to(&dest, &ctx()).unwrap());
    assert_eq!(git(&app, &["rev-parse", "HEAD"]), head);
    // GPT review: a branch deleted upstream is not followed through its stale tracking ref.
    git(&app, &["update-ref", "refs/remotes/origin/gone", "HEAD"]);
    let gone = HostStep::Pull { dir: app.clone(), remote: "origin".into(), branch: "gone".into(), repo: REPO.into() };
    assert!(gone.run_to(&dest, &ctx()).is_err_and(|e| e.contains("no longer exists")));
}

#[test]
fn the_scratch_repository_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let dest = upstream(&root);
    let command = HostStep::Clone { target: root.join("app"), repo: REPO.into() }.run_to(&dest, &ctx()).unwrap();
    let scratch = command.split("rm -rf -- '").nth(1).and_then(|r| r.split('\'').next()).unwrap();
    assert_eq!(std::fs::metadata(scratch).unwrap().permissions().mode() & 0o077, 0, "{scratch}");
    agent_runs(&command);
    assert!(!Path::new(scratch).exists(), "the call removes it");
}
