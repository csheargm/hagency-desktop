//! An Agent's work list (ADR 0016): the repositories its owner assigned on
//! this device, each with a role, set by the owner and never by the Agent.
//! `Developer` takes issues (claims, fixes, opens pull requests), `Reviewer`
//! reviews other people's pull requests, `Maintainer` reviews and merges them
//! under the owner's rules below.
//!
//! As maintainer, the Agent may review and merge other people's pull requests
//! when the author is trusted, the change is in scope (docs-only under a line
//! limit by default, or any path), no always-human path is touched, every
//! reported CI check is green on the exact head and an approval qualifies.
//! A repository's `.hagency/merge-policy.yml`, when present, is judged as
//! well, so it can only tighten. Without rules the file alone decides, as
//! before; with neither, the owner's card does.
use super::gate::GhPr;
use super::github::{PrSource, PrView};
use super::{parse_policy, MergePolicy};
use crate::issue_watch::valid_repo;
use crate::{key, Error, Ledger, Result};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS agent_work (agent TEXT NOT NULL, repo TEXT NOT NULL, rules TEXT NOT NULL, PRIMARY KEY(agent,repo)) STRICT;
CREATE TABLE IF NOT EXISTS agent_work_claims (agent TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(agent,repo,number)) STRICT;";

/// What the Agent does in one repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Developer,
    Reviewer,
    Maintainer,
}

/// Which issues a developer takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueFilter {
    /// Opened by the repository's team (OWNER, MEMBER, COLLABORATOR).
    #[default]
    Trusted,
    All,
}

/// The repository permissions with which an owner's own GitHub login may
/// try a merge (GitHub still enforces branch protection and required checks).
pub const CAN_MERGE: [&str; 3] = ["admin", "maintain", "write"];

/// Issues a developer claims per day across all its repositories.
pub const DEFAULT_ISSUES_PER_DAY: u32 = 5;
const DAY_SECS: i64 = 86_400;

/// "Anyone" in a trusted list.
pub const ANYONE: &str = "*";
/// The docs-only scope's paths and default line limit.
pub const DOCS_PATHS: [&str; 2] = ["**/*.md", "docs/**"];
pub const DOCS_MAX_LINES: u64 = 50;

/// One repository of the work list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerRules {
    pub repo: String,
    #[serde(default)]
    pub role: Role,
    /// Developer: which issues to take.
    #[serde(default)]
    pub issues: IssueFilter,
    /// The Matrix room the Agent's notices for this repository go to.
    pub room_id: String,
    /// Unix seconds: only issues and pull requests opened since count.
    #[serde(default)]
    pub since: i64,
    /// GitHub logins whose pull requests may be merged (`*`: anyone); this
    /// device's own login always counts.
    #[serde(default)]
    pub trusted_authors: Vec<String>,
    /// GitHub logins whose APPROVED review on the exact head counts.
    #[serde(default)]
    pub trusted_reviewers: Vec<String>,
    /// Any path instead of docs-only.
    #[serde(default)]
    pub any_path: bool,
    #[serde(default)]
    pub max_lines: Option<u64>,
    /// Globs that always need a person, besides the built-in protected paths.
    #[serde(default)]
    pub always_human: Vec<String>,
}

impl Judged {
    /// Every policy that applies; each must pass.
    pub fn policies(&self) -> impl Iterator<Item = &MergePolicy> {
        self.file.iter().chain(self.rules.iter())
    }
}

fn login_ok(login: &str) -> bool {
    login == ANYONE || (!login.is_empty() && login.len() <= 39 && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

impl OwnerRules {
    /// Check rules before storing them.
    pub fn validate(self) -> Result<Self> {
        if !valid_repo(&self.repo) || !self.room_id.starts_with('!') {
            return Err(Error::Invalid("rules need a repository owner/name and a room ID"));
        }
        if !self.trusted_authors.iter().chain(&self.trusted_reviewers).all(|l| login_ok(l)) || self.trusted_reviewers.iter().any(|l| l == ANYONE) {
            return Err(Error::Invalid("trusted lists hold GitHub logins (authors may also be *)"));
        }
        if self.always_human.iter().any(|g| g.trim().is_empty()) {
            return Err(Error::Invalid("always-human paths cannot be empty"));
        }
        Ok(self)
    }
    /// The rules as a policy for [`super::evaluate`]; CI must be green when
    /// the head reports any check.
    pub fn policy(&self) -> MergePolicy {
        let mut policy = parse_policy("").expect("the empty policy parses");
        if self.any_path {
            policy.trusted = true;
            policy.trusted_max_lines = self.max_lines;
        } else {
            policy.low_risk_paths = DOCS_PATHS.iter().map(|p| (*p).to_owned()).collect();
            policy.max_changed_lines = self.max_lines.unwrap_or(DOCS_MAX_LINES);
        }
        policy.always_human = self.always_human.clone();
        policy.require_ci = true;
        policy.ci_if_present = true;
        policy
    }
    fn trusts_author(&self, author: &str, own: Option<&str>) -> bool {
        !author.is_empty() && (own.is_some_and(|o| o.eq_ignore_ascii_case(author)) || self.trusted_authors.iter().any(|t| t == ANYONE || t.eq_ignore_ascii_case(author)))
    }
}

impl Ledger {
    /// Store (or with `None`, remove) the owner's rules for `repo`.
    pub fn set_owner_rules(&mut self, owner: &str, agent: &str, repo: &str, rules: Option<OwnerRules>) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        let repo = repo.to_ascii_lowercase();
        match rules {
            Some(rules) => {
                let rules = rules.validate()?;
                if !rules.repo.eq_ignore_ascii_case(&repo) {
                    return Err(Error::Invalid("the rules are for another repository"));
                }
                let text = serde_json::to_string(&rules).map_err(|_| Error::Invalid("unreadable rules"))?;
                self.db.execute("INSERT INTO agent_work VALUES (?,?,?) ON CONFLICT(agent,repo) DO UPDATE SET rules=excluded.rules", (agent, &repo, text))?;
            }
            None => {
                self.db.execute("DELETE FROM agent_work WHERE agent=? AND repo=?", (agent, &repo))?;
            }
        }
        Ok(())
    }
    /// The owner's rules for `repo`, if set. Unreadable stored rules are an error.
    pub fn owner_rules(&self, agent: &str, repo: &str) -> Result<Option<OwnerRules>> {
        use rusqlite::OptionalExtension;
        let text: Option<String> = self.db.query_row("SELECT rules FROM agent_work WHERE agent=? AND repo=?", (agent, repo.to_ascii_lowercase()), |r| r.get(0)).optional()?;
        text.map(|t| serde_json::from_str(&t).map_err(|_| Error::Invalid("stored rules are unreadable"))).transpose()
    }
    /// Reserve issue `number` for the Agent at `now`, before its hand-off:
    /// true when it is reserved (now, or by an earlier pass whose hand-off
    /// failed), false when the daily limit is used up. One statement, so the
    /// count and the insert cannot interleave with another writer.
    pub fn record_work_claim(&mut self, agent: &str, repo: &str, number: i64, now: i64) -> Result<bool> {
        let repo = repo.to_ascii_lowercase();
        if self.work_claimed(agent, &repo, number)? {
            return Ok(true);
        }
        let inserted = self.db.execute(
            "INSERT OR IGNORE INTO agent_work_claims SELECT ?1,?2,?3,?4 WHERE (SELECT COUNT(*) FROM agent_work_claims WHERE agent=?1 AND at>?4-?5) < ?6",
            rusqlite::params![agent, repo, number, now, DAY_SECS, DEFAULT_ISSUES_PER_DAY],
        )?;
        Ok(inserted == 1 || self.work_claimed(agent, &repo, number)?)
    }
    pub fn work_claimed(&self, agent: &str, repo: &str, number: i64) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM agent_work_claims WHERE agent=? AND repo=? AND number=?)", (agent, repo.to_ascii_lowercase(), number), |r| r.get(0))?)
    }
    /// How many more issues the Agent may claim today, across every repository.
    pub fn claims_left_today(&self, agent: &str, now: i64) -> Result<u32> {
        let used: i64 = self.db.query_row("SELECT COUNT(*) FROM agent_work_claims WHERE agent=? AND at>?", (agent, now - DAY_SECS), |r| r.get(0))?;
        Ok(DEFAULT_ISSUES_PER_DAY.saturating_sub(u32::try_from(used).unwrap_or(u32::MAX)))
    }
    /// The whole work list, replaced in one transaction by the caller.
    pub fn set_work(&mut self, owner: &str, agent: &str, work: Vec<OwnerRules>) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        let mut repos = std::collections::HashSet::new();
        if work.iter().any(|w| !repos.insert(w.repo.to_ascii_lowercase())) {
            return Err(Error::Invalid("a repository is listed twice"));
        }
        self.db.execute("DELETE FROM agent_work WHERE agent=?", [agent])?;
        for entry in work {
            let repo = entry.repo.clone();
            self.set_owner_rules(owner, agent, &repo, Some(entry))?;
        }
        Ok(())
    }
    /// Every repository on the Agent's work list here.
    pub fn all_owner_rules(&self, agent: &str) -> Result<Vec<OwnerRules>> {
        let mut statement = self.db.prepare("SELECT rules FROM agent_work WHERE agent=? ORDER BY repo")?;
        let rows = statement.query_map([agent], |r| r.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).map_err(|_| Error::Invalid("stored rules are unreadable"))).collect()
    }
}

/// What the gate judges a pull request by.
pub(crate) struct Judged {
    /// The repository's own file, judged with its own approval rule (an Agent
    /// other than the merger; GitHub reviews never count for it).
    pub file: Option<MergePolicy>,
    /// The owner's rules (or a developer's or reviewer's readying policy).
    pub rules: Option<MergePolicy>,
    pub trusted_reviewers: Vec<String>,
    /// This device's own GitHub login, when known (never counted twice).
    pub own_login: Option<String>,
    /// The Agent whose own approval does not count: the merger, unless the
    /// owner's rules apply and someone else's GitHub login wrote the change.
    pub excluded: String,
    /// Set when the owner's rules apply and the author is not trusted.
    pub author_reason: Option<String>,
    /// The Agent is a developer or reviewer here: its ready and review steps
    /// skip the card, its merges never do.
    pub review_only: bool,
}

/// The policies for `view` and the approval rules. `None` (the owner's card)
/// when the file or the rules cannot be read, or neither exists.
pub(crate) async fn judged(ledger: &mut Ledger, agent: &str, command: &GhPr, repo: &str, view: &PrView, source: &impl PrSource) -> Option<Judged> {
    let file = match source.policy_file(command, repo, &view.base_sha).await? {
        Some(text) => Some(parse_policy(&text)?),
        None => None,
    };
    let rules = ledger.owner_rules(agent, repo).ok()?;
    let mut judged = Judged { file, rules: None, trusted_reviewers: Vec::new(), own_login: None, excluded: agent.to_owned(), author_reason: None, review_only: false };
    if let Some(rules) = rules.as_ref().filter(|r| r.role != Role::Maintainer) {
        judged.review_only = true;
        // Readying or reviewing any path (protected ones still ask); never a merge.
        judged.rules = Some(OwnerRules { any_path: true, max_lines: None, ..rules.clone() }.policy());
    }
    if let Some(rules) = rules.filter(|r| r.role == Role::Maintainer) {
        let own = source.viewer_login(command).await;
        if !rules.trusts_author(&view.author, own.as_deref()) {
            judged.author_reason = Some(format!("{} is not a trusted author", if view.author.is_empty() { "an unknown account" } else { view.author.as_str() }));
        }
        // The Agent did not write this change if its owner's login did not.
        if own.as_deref().is_some_and(|o| !view.author.is_empty() && !o.eq_ignore_ascii_case(&view.author)) {
            judged.excluded = String::new();
        }
        judged.trusted_reviewers = rules.trusted_reviewers.clone();
        judged.rules = Some(rules.policy());
        judged.own_login = own;
    }
    (judged.file.is_some() || judged.rules.is_some()).then_some(judged)
}
