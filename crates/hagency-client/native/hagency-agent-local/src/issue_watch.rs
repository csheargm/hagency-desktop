//! GitHub issue watch: an owner-chosen, per-Agent setting that hands new
//! GitHub issues to the Agent. `Off` (the default) does nothing; `Labelled`
//! hands off open issues carrying the owner's label; `Trusted` (the
//! recommended choice) hands off issues opened by the repository's own team;
//! `All` hands off every open issue. `Trusted` and `All` only count issues
//! created after the watch was switched on, so turning it on is no flood.
//!
//! Trust comes from the issue's `author_association` (OWNER, MEMBER or
//! COLLABORATOR), which GitHub includes in the issue list for free, instead
//! of one `collaborators/{user}/permission` call per author. COLLABORATOR
//! covers anyone invited to the repository, including read-only invites on
//! private repositories; that is accepted as "the team" here. The desktop poller
//! (which also does the HTTP, see `wire`) posts "@<agent> please fix <url>" into `room_id`, so the normal dispatch and
//! approval flow applies unchanged. Nothing here merges or writes to GitHub.
use crate::{Error, Ledger, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub mod wire;

/// The label `Labelled` uses when the owner names none.
pub const DEFAULT_LABEL: &str = "agent-fix";

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issue_watch (agent TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK(mode IN ('off','labelled','trusted','all')), label TEXT NOT NULL, repo TEXT NOT NULL, room_id TEXT NOT NULL, enabled_at INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS issue_watch_seen (agent TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL, PRIMARY KEY(agent,repo,number)) STRICT;";

/// Create the issue-watch tables (new in this build; nothing to migrate).
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> Result<()> {
    Ok(db.execute_batch(SCHEMA)?)
}

/// Which issues the watch hands to the Agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchMode {
    #[default]
    Off,
    Labelled,
    /// Issues whose author is the repository's own team (no label needed).
    Trusted,
    All,
}
impl WatchMode {
    fn stored(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Labelled => "labelled",
            Self::Trusted => "trusted",
            Self::All => "all",
        }
    }
    fn from_stored(text: &str) -> Self {
        match text {
            "labelled" => Self::Labelled,
            "trusted" => Self::Trusted,
            "all" => Self::All,
            _ => Self::Off,
        }
    }
}

/// What the owner sets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WatchSetting {
    pub mode: WatchMode,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub room_id: String,
}

/// The stored watch: the setting plus when it was last switched on (unix
/// seconds; 0 while it has never been on).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueWatch {
    #[serde(flatten)]
    pub setting: WatchSetting,
    pub enabled_at: i64,
}

/// One open GitHub issue (or pull request: GitHub lists both).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: i64,
    pub url: String,
    pub labels: Vec<String>,
    pub is_pr: bool,
    /// GitHub's `author_association` (OWNER, MEMBER, COLLABORATOR, CONTRIBUTOR, NONE, ...).
    pub author_association: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// Authors `Trusted` hands off: the repository's owner, org members and collaborators.
pub const TRUSTED_ASSOCIATIONS: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

/// `owner/name` with GitHub's allowed characters.
pub(crate) fn valid_repo(repo: &str) -> bool {
    let ok = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    matches!(repo.split_once('/'), Some((owner, name)) if ok(owner) && ok(name) && !name.contains('/'))
}

/// Matrix spec: `!` + opaque id, optionally `:server` (room v12 IDs have no
/// server part), at most 255 bytes, no whitespace or control characters.
fn valid_room_id(room_id: &str) -> bool {
    const MAX_ID_BYTES: usize = 255;
    let Some(rest) = room_id.strip_prefix('!') else { return false };
    let (opaque, server) = match rest.split_once(':') {
        Some((opaque, server)) => (opaque, Some(server)),
        None => (rest, None),
    };
    room_id.len() <= MAX_ID_BYTES
        && !opaque.is_empty()
        && server.is_none_or(|s| !s.is_empty())
        && !room_id.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Check a setting before storing it; `Off` needs nothing else. A `Labelled`
/// setting with no label gets `DEFAULT_LABEL`.
pub fn validate(mut setting: WatchSetting) -> Result<WatchSetting> {
    setting.label = setting.label.trim().to_owned();
    if setting.mode == WatchMode::Off {
        return Ok(setting);
    }
    if setting.mode == WatchMode::Labelled && setting.label.is_empty() {
        setting.label = DEFAULT_LABEL.to_owned();
    }
    if !valid_repo(&setting.repo) {
        return Err(Error::Invalid("repository must be owner/name"));
    }
    if !valid_room_id(&setting.room_id) {
        return Err(Error::Invalid("room ID must be a Matrix room ID such as !abc:example.org"));
    }
    Ok(setting)
}

/// The issues to hand off now: not pull requests, not already handed off,
/// carrying the label (`Labelled`), or created after the watch was switched
/// on (`Trusted`, by a trusted author; `All`, by anyone). Labels count only in
/// `Labelled` mode. The cutoff is inclusive: both times are whole seconds, so
/// an issue from the switch-on second is handed off (the seen set dedupes).
pub fn select<'a>(watch: &IssueWatch, issues: &'a [Issue], seen: &HashSet<i64>) -> Vec<&'a Issue> {
    let setting = &watch.setting;
    issues
        .iter()
        .filter(|issue| !issue.is_pr && !seen.contains(&issue.number))
        .filter(|issue| match setting.mode {
            WatchMode::Off => false,
            WatchMode::Labelled => issue.labels.iter().any(|l| l.eq_ignore_ascii_case(&setting.label)),
            WatchMode::Trusted => issue.created_at >= watch.enabled_at && TRUSTED_ASSOCIATIONS.contains(&issue.author_association.as_str()),
            WatchMode::All => issue.created_at >= watch.enabled_at,
        })
        .collect()
}

impl Ledger {
    /// The Agent's issue watch; `Off` when the owner never set one.
    pub fn issue_watch(&self, agent: &str) -> Result<IssueWatch> {
        let row = self
            .db
            .query_row("SELECT mode,label,repo,room_id,enabled_at FROM issue_watch WHERE agent=?", [agent], |r| {
                let mode: String = r.get(0)?;
                Ok(IssueWatch {
                    setting: WatchSetting { mode: WatchMode::from_stored(&mode), label: r.get(1)?, repo: r.get(2)?, room_id: r.get(3)? },
                    enabled_at: r.get(4)?,
                })
            })
            .optional()?;
        Ok(row.unwrap_or_default())
    }
    /// Store the owner's setting. Switching it on (from `Off`) or to another
    /// repository restarts `enabled_at` at `now`; otherwise it is kept.
    pub fn set_issue_watch(&mut self, owner: &str, agent: &str, setting: WatchSetting, now: i64) -> Result<IssueWatch> {
        self.owner(owner)?;
        let setting = validate(setting)?;
        let old = self.issue_watch(agent)?;
        let restart = old.setting.mode == WatchMode::Off || old.setting.repo != setting.repo;
        let enabled_at = if setting.mode == WatchMode::Off { old.enabled_at } else if restart { now } else { old.enabled_at };
        self.db.execute(
            "INSERT INTO issue_watch VALUES (?,?,?,?,?,?) ON CONFLICT(agent) DO UPDATE SET mode=excluded.mode,label=excluded.label,repo=excluded.repo,room_id=excluded.room_id,enabled_at=excluded.enabled_at",
            (agent, setting.mode.stored(), &setting.label, &setting.repo, &setting.room_id, enabled_at),
        )?;
        Ok(IssueWatch { setting, enabled_at })
    }
    /// Run `change` as one SQLite transaction: any error rolls back every
    /// write it made, so a multi-part owner change is all-or-nothing.
    pub fn atomically<T>(&mut self, change: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        match change(self) {
            Ok(value) => {
                self.db.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                // A failed rollback leaves the ledger state uncertain, which
                // outranks the change's own error, so that is what returns.
                self.db.execute_batch("ROLLBACK")?;
                Err(error)
            }
        }
    }
    /// Record that this issue was handed to the Agent.
    pub fn mark_issue_seen(&mut self, agent: &str, repo: &str, number: i64) -> Result<()> {
        self.db.execute("INSERT INTO issue_watch_seen VALUES (?,?,?) ON CONFLICT DO NOTHING", (agent, repo, number))?;
        Ok(())
    }
    pub fn is_issue_seen(&self, agent: &str, repo: &str, number: i64) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM issue_watch_seen WHERE agent=? AND repo=? AND number=?)", (agent, repo, number), |r| r.get(0))?)
    }
    /// Every issue number already handed off for this repository.
    pub fn seen_issues(&self, agent: &str, repo: &str) -> Result<HashSet<i64>> {
        let mut statement = self.db.prepare("SELECT number FROM issue_watch_seen WHERE agent=? AND repo=?")?;
        let rows = statement.query_map((agent, repo), |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests;
