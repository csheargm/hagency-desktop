//! The git steps push trust answers that touch the network (push, fetch)
//! are run by the host, never by the Agent's git. The checkout's config (`url.*.insteadOf`, `credential.helper`,
//! `core.sshCommand`, `include.path`, hooks) is data the Agent can write at
//! any time, so the step that talks to GitHub runs in a fresh, host-owned
//! bare repository with no system or global config, whose only link to the
//! checkout is `objects/info/alternates` (object files, never config).
//! See docs/adr/0015-host-side-trusted-push.md.
use super::git::query;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Config of every host network command; with no system or global config
/// read, this is all the config there is (the scratch repository has none).
const HOST_CONFIG: [&str; 10] = [
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.sshCommand=false",
    // Alternate refs would run `git for-each-ref` inside the checkout.
    "core.alternateRefsCommand=true",
    "protocol.allow=never",
    "credential.helper=",
    "credential.helper=!gh auth git-credential",
    // Keep fetched objects as one pack the host can hand to the checkout.
    "fetch.unpackLimit=1",
    "transfer.unpackLimit=1",
    "push.followTags=false",
];
/// The only variables a host network command inherits (gh reads its login
/// from HOME or GH_CONFIG_DIR, or a token variable).
const HOST_ENV: [&str; 9] = ["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "GH_TOKEN", "GITHUB_TOKEN", "GH_CONFIG_DIR"];

/// A step the host runs itself when push trust answers the Agent's command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStep {
    Push { dir: PathBuf, remote: String, branch: String, repo: String, upstream: bool },
    Fetch { dir: PathBuf, remote: String, repo: String },
}

/// Where a network step goes: the watched repository on github.com over
/// https in production; tests point it at a local bare repository.
pub(crate) struct Dest {
    pub(crate) url: String,
    pub(crate) protocol: &'static str,
}

impl Dest {
    fn github(repo: &str) -> Self {
        Self { url: format!("https://github.com/{repo}.git"), protocol: "https" }
    }
}

impl HostStep {
    /// Run the step; the result is what the Agent is told.
    pub fn run(&self) -> Result<String, String> {
        let (Self::Push { repo, .. } | Self::Fetch { repo, .. }) = self;
        self.run_to(&Dest::github(repo))
    }

    pub(crate) fn run_to(&self, dest: &Dest) -> Result<String, String> {
        match self {
            Self::Push { dir, remote, branch, upstream, .. } => push(dir, remote, branch, *upstream, dest),
            Self::Fetch { dir, remote, .. } => fetch(dir, remote, dest),
        }
    }
}

/// The Bash command the Agent's tool call runs instead: it only prints what
/// the host did (and fails when the host step failed).
pub fn report_command(result: &Result<String, String>) -> String {
    match result {
        Ok(done) => format!("printf '%s\\n' {}", super::quote(done)),
        Err(why) => format!("printf '%s\\n' {} >&2; exit 1", super::quote(&format!("Hagency could not run this step: {why}"))),
    }
}

fn push(dir: &Path, remote: &str, branch: &str, upstream: bool, dest: &Dest) -> Result<String, String> {
    let head = format!("refs/heads/{branch}");
    let sha = query(dir, &["rev-parse", "--verify", "-q", &format!("{head}^{{commit}}")]).ok_or("the branch has no commit")?;
    let scratch = Scratch::new(&objects_dir(dir)?)?;
    // The remote's own default branch, not the checkout's forgeable
    // `refs/remotes/<remote>/HEAD` (GPT review, Nov 16).
    let advertised = run(scratch.git(dest).args(["ls-remote", "--symref", &dest.url, "HEAD"]), None)?;
    let default = advertised.lines().find_map(|l| l.strip_prefix("ref: refs/heads/")?.strip_suffix("\tHEAD")).ok_or("the remote's default branch is unknown")?;
    if default == branch {
        return Err(format!("{branch} is the repository's default branch"));
    }
    run(scratch.git(dest).args(["update-ref", &head, &sha]), None)?;
    run(scratch.git(dest).args(["push", "-q", &dest.url, &format!("{head}:{head}")]), None)?;
    let short = &sha[..sha.len().min(12)];
    if upstream {
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let set = query(dir, &["update-ref", "--no-deref", &tracking, &sha]).is_some()
            && query(dir, &["config", &format!("branch.{branch}.remote"), remote]).is_some()
            && query(dir, &["config", &format!("branch.{branch}.merge"), &head]).is_some();
        if !set {
            return Ok(format!("Pushed {branch} ({short}) to {}; its upstream could not be set.", dest.url));
        }
    }
    Ok(format!("Pushed {branch} ({short}) to {}.", dest.url))
}

fn fetch(dir: &Path, remote: &str, dest: &Dest) -> Result<String, String> {
    let objects = objects_dir(dir)?;
    let scratch = Scratch::new(&objects)?;
    let prefix = format!("refs/remotes/{remote}/");
    // What the checkout already has, so only new objects come down.
    let known = query(dir, &["for-each-ref", "--format=%(objectname) %(refname)", &prefix]).ok_or("cannot read the checkout's refs")?;
    let seed: String = known.lines().filter_map(|l| l.split_once(' ')).filter_map(|(sha, name)| Some(format!("update refs/heads/{} {sha}\n", name.strip_prefix(&prefix)?))).filter(|l| !l.contains(" refs/heads/HEAD ")).collect();
    run(scratch.git(dest).args(["update-ref", "--stdin"]), Some(&seed))?;
    run(scratch.git(dest).args(["fetch", "-q", "--no-tags", "--no-write-fetch-head", "--prune", &dest.url, "+refs/heads/*:refs/heads/*"]), None)?;
    hand_over_packs(&scratch.0.join("objects/pack"), &objects.join("pack"))?;
    let fetched = run(scratch.git(dest).args(["for-each-ref", "--format=%(objectname) %(refname:lstrip=2)", "refs/heads"]), None)?;
    let updates: String = fetched.lines().filter_map(|l| l.split_once(' ')).map(|(sha, name)| format!("update {prefix}{name} {sha}\n")).collect();
    // --no-deref: a planted symbolic tracking ref cannot steer the write to
    // a local branch (GPT review, Nov 16).
    local_stdin(dir, &["update-ref", "--no-deref", "--stdin"], &updates)?;
    Ok(format!("Fetched {} branches of {} into {remote}.", fetched.lines().count(), dest.url))
}

/// Copy fetched packs into the checkout, each `.idx` after its `.pack` so a
/// pack is visible only once complete.
fn hand_over_packs(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| e.to_string())?;
    let mut packs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "pack")).collect();
    packs.sort();
    for pack in packs {
        for ext in ["pack", "rev", "idx"] {
            let file = pack.with_extension(ext);
            let target = to.join(file.file_name().ok_or("pack name")?);
            if file.exists() && target.symlink_metadata().is_err() {
                copy_new(&file, &target)?;
            }
        }
    }
    Ok(())
}

/// `from` copied to a new file `to`; never through a link or over a file.
fn copy_new(from: &Path, to: &Path) -> Result<(), String> {
    let mut source = std::fs::File::open(from).map_err(|e| e.to_string())?;
    let mut target = std::fs::OpenOptions::new().write(true).create_new(true).open(to).map_err(|e| format!("copy pack: {e}"))?;
    std::io::copy(&mut source, &mut target).map_err(|e| format!("copy pack: {e}"))?;
    target.sync_all().map_err(|e| e.to_string())
}

/// The object directory of checkout `dir`, which must be a plain clone:
/// `.git`, `objects` and `objects/pack` real directories (no links, no
/// `.git` file, no `commondir`), so the host writes only inside `dir`
/// (GPT review, Nov 16).
fn objects_dir(dir: &Path) -> Result<PathBuf, String> {
    let git_dir = dir.join(".git");
    let objects = git_dir.join("objects");
    let real_dirs = [&git_dir, &objects, &objects.join("pack")].iter().all(|d| d.symlink_metadata().is_ok_and(|m| m.is_dir()));
    if !real_dirs || git_dir.join("commondir").symlink_metadata().is_ok() || objects.to_string_lossy().contains('\n') {
        return Err("the checkout is not a plain clone".into());
    }
    let top = query(dir, &["rev-parse", "--path-format=absolute", "--git-dir"]).ok_or("not a git checkout")?;
    if Path::new(&top).canonicalize().ok() != git_dir.canonicalize().ok() {
        return Err("the checkout's git directory is elsewhere".into());
    }
    Ok(objects)
}

/// A fresh host-owned bare repository, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(objects: &Path) -> Result<Self, String> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("hagency-trusted-git-{}-{nanos}-{n}", std::process::id()));
        // create_dir fails on anything already there (a planted link included).
        std::fs::create_dir(&path).map_err(|e| format!("scratch repository: {e}"))?;
        let scratch = Self(path);
        run(host_command(&scratch.0, None).args(["init", "-q", "--bare", "--template="]), None)?;
        let info = scratch.0.join("objects/info");
        std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
        std::fs::write(info.join("alternates"), format!("{}\n", objects.display())).map_err(|e| e.to_string())?;
        Ok(scratch)
    }

    fn git(&self, dest: &Dest) -> Command {
        host_command(&self.0, Some(dest.protocol))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover scratch repository holds no secret and
        // nothing reads it again (each step makes a new one).
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git --git-dir <scratch>` with no system, global or repository config but
/// [`HOST_CONFIG`], and only [`HOST_ENV`] inherited.
pub(crate) fn host_command(git_dir: &Path, protocol: Option<&str>) -> Command {
    let mut command = Command::new("git");
    command.env_clear();
    for key in HOST_ENV {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0");
    command.arg("--git-dir").arg(git_dir);
    for pin in HOST_CONFIG {
        command.arg("-c").arg(pin);
    }
    if let Some(protocol) = protocol {
        command.arg("-c").arg(format!("protocol.{protocol}.allow=always"));
    }
    command
}

/// Stdout of a successful command; stderr as the error.
fn run(command: &mut Command, stdin: Option<&str>) -> Result<String, String> {
    command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// A local `git -C dir` with hooks and helpers off, fed `input`.
fn local_stdin(dir: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args).envs(crate::agent_env::GIT_HELPERS_OFF_ENV).env("GIT_TERMINAL_PROMPT", "0").env("GIT_NO_LAZY_FETCH", "1");
    run(&mut command, Some(input))
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
