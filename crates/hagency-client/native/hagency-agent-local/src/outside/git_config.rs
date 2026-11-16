//! Repository-local git config is workspace data an Agent can write (Codex's
//! sandbox may edit `.git/config`), and many keys run a program from an
//! otherwise read-only command: `diff.external`, `diff.<x>.textconv`,
//! `filter.<x>.clean|smudge|process` (with `.gitattributes`), `core.pager`,
//! `core.sshCommand`, `credential.helper`, `include.path`, ... Denying each
//! known key loses to the next one, so the local config is checked against an
//! allowlist of keys that only name data; any other key makes the git command
//! ask. Attributes alone cannot run anything: drivers they name are defined
//! in config, so with no local driver only the owner's own global/system
//! config can define one. A directory outside any repository has no local config.
use crate::agent_env::GIT_HELPERS_OFF_ENV;
use std::path::Path;

/// Exact local keys (lowercase, as `git config --name-only` prints them).
const SAFE_KEYS: [&str; 12] = [
    "core.repositoryformatversion", "core.filemode", "core.bare", "core.logallrefupdates", "core.ignorecase",
    "core.precomposeunicode", "user.name", "user.email", "push.default", "push.autosetupremote", "pull.rebase",
    "init.defaultbranch",
];
/// `<section>.<name>.<key>` forms: remotes and branch tracking (URLs are
/// checked by whoever pushes; `uploadpack`/`receivepack`/`proxy` are not here).
const SAFE_SUBSECTION_KEYS: [(&str, &str); 7] = [
    ("remote", "url"), ("remote", "fetch"), ("remote", "pushurl"), ("remote", "gh-resolved"),
    ("branch", "remote"), ("branch", "merge"), ("branch", "gh-merge-base"),
];

/// True when `dir`'s repository-local config holds only data keys (or `dir`
/// is in no repository). Any failure to read it counts as unsafe.
pub(crate) fn local_config_safe(dir: &Path) -> bool {
    let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(dir).args(args).envs(GIT_HELPERS_OFF_ENV).output().ok();
    let Some(inside) = git(&["rev-parse", "--git-dir"]) else { return false };
    if !inside.status.success() {
        return !dir.exists() || String::from_utf8_lossy(&inside.stderr).contains("not a git repository");
    }
    let Some(list) = git(&["config", "--local", "--no-includes", "--name-only", "--list"]) else { return false };
    list.status.success() && String::from_utf8_lossy(&list.stdout).lines().all(safe_key)
}

fn safe_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if SAFE_KEYS.contains(&key.as_str()) {
        return true;
    }
    let Some((section, rest)) = key.split_once('.') else { return false };
    let Some((_, name)) = rest.rsplit_once('.') else { return false };
    SAFE_SUBSECTION_KEYS.contains(&(section, name))
}
