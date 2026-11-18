//! The Safeguards card of one issue watch (ADR 0026), without widgets: what
//! the Agent may do alone, what needs the owner, and the limits, each line
//! built from the values the code enforces ([`Enforced::current`]) and the
//! room's merge rules (ADR 0029) as the gate parses them. Nothing here is a second
//! copy of a number: change a constant and the card changes with it.
use ::hagency::issue_watch::safeguards::{self as enforced, MergePolicy, RoomRulesState, room_autonomy::Level};
use crate::i18n::{format, tr};
use serde_json::Value;

/// The engine's limits and protected names, as the card states them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Enforced {
    pub max_attempts: u32,
    pub free_retries: u32,
    pub handoff_secs: i64,
    pub protected_paths: Vec<String>,
    pub codeowners: String,
    pub protected_branches: Vec<String>,
    /// The low-risk paths the room's rules editor suggests.
    pub suggested_paths: Vec<String>,
    pub min_approvals: u32,
}

impl Enforced {
    /// The values the issue loop, push-freely and the merge gate enforce.
    pub(crate) fn current() -> Self {
        let owned = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        Self {
            max_attempts: enforced::MAX_ATTEMPTS,
            free_retries: enforced::MAX_FREE_RETRIES,
            handoff_secs: enforced::HANDOFF_TIMEOUT_SECS,
            protected_paths: owned(&enforced::PROTECTED_PREFIXES),
            codeowners: enforced::CODEOWNERS.to_owned(),
            protected_branches: owned(&enforced::PROTECTED_BRANCHES),
            suggested_paths: owned(&enforced::SUGGESTED_LOW_RISK_PATHS),
            min_approvals: enforced::MIN_AGENT_APPROVALS,
        }
    }
}

/// One repository the Agent works on, and its room's setting (ADR 0030).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Watch {
    pub repo: String,
    /// The room it works in: its level, Pause and merge rules apply.
    pub room_id: String,
    pub push_freely: bool,
    /// The room's level (`Off` until read), whether it is paused, and
    /// whether this owner is the room's owner or a maintainer (whose Agents merge).
    pub level: Level,
    pub paused: bool,
    pub maintainer: bool,
}

/// The Agent's first work-list entry (the room loop's), else its old
/// single watch while it is on; `None` when it works nowhere.
pub(crate) fn watch_of(saved: &Value) -> Option<Watch> {
    if let Some(entry) = saved["work"].as_array().and_then(|w| w.first()) {
        let repo = entry["repo"].as_str().filter(|r| !r.is_empty())?;
        return Some(Watch { repo: repo.to_owned(), room_id: entry["roomId"].as_str().unwrap_or_default().to_owned(), push_freely: entry["pushFreely"] == true, ..Default::default() });
    }
    let watch = &saved["issueWatch"];
    let repo = watch["repo"].as_str().filter(|r| !r.is_empty())?;
    if watch["mode"].as_str().is_none_or(|m| m == "off") {
        return None;
    }
    let room_id = watch["roomId"].as_str().unwrap_or_default().to_owned();
    Some(Watch { repo: repo.to_owned(), room_id, push_freely: watch["pushTrusted"] == true, ..Default::default() })
}

/// The room's setting line: "Agent work in this room: Assist — agents work, a person merges".
pub(crate) fn level_line(watch: &Watch) -> String {
    if watch.paused {
        return tr("Agent work in this room: paused — nothing new starts until the owner or a maintainer resumes it").to_owned();
    }
    let label = crate::hagency::room_autonomy::lines::LEVEL_LABELS[crate::hagency::room_autonomy::lines::level_index(watch.level)];
    format("Agent work in this room: {level}", &[("level", tr(label).to_owned())])
}

/// What is known about the repository's default branch and the room's rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Repo {
    Failed(String),
    Read { branch: String, protected: Option<bool>, rules: RoomRulesState },
}

/// The card's text, already translated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Card {
    pub alone: Vec<String>,
    pub needs_you: Vec<String>,
    pub limits: Vec<String>,
    pub protection: String,
}

/// The card for `watch`, from `repo` and the enforced values.
pub(crate) fn card(watch: &Watch, repo: &Repo, enforced: &Enforced) -> Card {
    let policy = match repo {
        Repo::Read { rules, .. } => Some(rules),
        _ => None,
    };
    Card {
        alone: alone(watch, policy, enforced),
        needs_you: needs_you(watch, policy),
        limits: std::iter::once(level_line(watch)).chain(limits(repo, enforced)).collect(),
        protection: protection(watch, repo),
    }
}

/// Whether the gate may merge alone: an Autonomous, running room with valid
/// rules, and this owner is its owner or a maintainer.
fn merges_alone(watch: &Watch, policy: Option<&RoomRulesState>) -> bool {
    watch.level == Level::Autonomous && !watch.paused && watch.maintainer && matches!(policy, Some(RoomRulesState::Read(_)))
}

fn alone(watch: &Watch, policy: Option<&RoomRulesState>, enforced: &Enforced) -> Vec<String> {
    let mut lines = Vec::new();
    let works = matches!(watch.level, Level::Assist | Level::Autonomous) && !watch.paused;
    if watch.push_freely && works {
        lines.push(format("Branch, edit and commit in its own clone of {repo}", &[("repo", watch.repo.clone())]));
        let branches = enforced.protected_branches.join(", ");
        lines.push(format("Push its own branches (never the default branch or {branches}) and open pull requests", &[("branches", branches)]));
    }
    if merges_alone(watch, policy) {
        lines.push(tr("Approve and merge pull requests the merge rule allows").to_owned());
    }
    if lines.is_empty() {
        lines.push(tr("Nothing yet: every step asks you").to_owned());
    }
    lines
}

fn needs_you(watch: &Watch, policy: Option<&RoomRulesState>) -> Vec<String> {
    let mut lines = Vec::new();
    if watch.push_freely && matches!(watch.level, Level::Assist | Level::Autonomous) && !watch.paused {
        lines.push(tr("Build, test and install commands, other websites and anything outside its clone").to_owned());
    } else {
        lines.push(tr("Every push, pull request and command").to_owned());
    }
    lines.push(tr("Deleting or force-pushing branches").to_owned());
    lines.push(match policy {
        _ if watch.level != Level::Autonomous || watch.paused => tr("Every merge (a person merges unless the room is Autonomous)").to_owned(),
        _ if !watch.maintainer => tr("Every merge (only agents of the room owner or maintainers merge)").to_owned(),
        Some(RoomRulesState::Absent) => tr("Every merge: this room has no merge rules — a room admin can set them in Room info → Status").to_owned(),
        Some(RoomRulesState::Unreadable) => tr("Every merge: the room's merge rules have a rule Hagency does not accept").to_owned(),
        _ => tr("Merges the merge rule does not allow").to_owned(),
    });
    lines
}

fn limits(repo: &Repo, enforced: &Enforced) -> Vec<String> {
    let mut lines = vec![
        format("{count} tries per issue, then it is marked failed", &[("count", enforced.max_attempts.to_string())]),
        format("{count} extra tries when a restart or an unanswered card stopped a run", &[("count", enforced.free_retries.to_string())]),
        format("{minutes} minutes per try to open a pull request", &[("minutes", (enforced.handoff_secs / 60).to_string())]),
        merge_rule(repo, enforced),
    ];
    let mut protected = enforced.protected_paths.clone();
    protected.push(enforced.codeowners.clone());
    if let Repo::Read { rules: RoomRulesState::Read(policy), .. } = repo {
        protected.extend(policy.always_human.iter().cloned());
    }
    lines.push(format("Always a person: {paths}", &[("paths", protected.join(", "))]));
    lines
}

/// "Merge rule (room policy): docs/**, up to 50 changed lines, CI not required, 1 approval(s) from another Agent".
fn merge_rule(repo: &Repo, enforced: &Enforced) -> String {
    match repo {
        Repo::Failed(error) => format("Merge rule (room policy): could not be read: {error}", &[("error", error.clone())]),
        Repo::Read { rules: RoomRulesState::Absent, .. } => format(
            "Merge rule (room policy): none — a room admin can set them in Room info → Status (suggested paths: {paths})",
            &[("paths", enforced.suggested_paths.join(", "))],
        ),
        Repo::Read { rules: RoomRulesState::Unreadable, .. } => tr("Merge rule (room policy): not valid, so the gate merges nothing").to_owned(),
        Repo::Read { rules: RoomRulesState::Read(policy), .. } => format("Merge rule (room policy): {scope}, {lines}, {ci}, {approvals} approval(s) from another Agent", &[
            ("scope", scope(policy)),
            ("lines", line_limit(policy)),
            ("ci", tr(if policy.require_ci { "CI required" } else { "CI not required" }).to_owned()),
            ("approvals", policy.required_agent_approvals.max(enforced.min_approvals).to_string()),
        ]),
    }
}

/// Which paths a merge may touch, as the gate's tiers read them.
fn scope(policy: &MergePolicy) -> String {
    match (policy.trusted, policy.trusted_paths.is_empty()) {
        (true, true) => tr("any path").to_owned(),
        (true, false) => policy.trusted_paths.join(", "),
        (false, _) if policy.low_risk_paths.is_empty() => tr("no low-risk paths").to_owned(),
        (false, _) => policy.low_risk_paths.join(", "),
    }
}

fn line_limit(policy: &MergePolicy) -> String {
    let limit = if policy.trusted { policy.trusted_max_lines } else { Some(policy.max_changed_lines) };
    match limit {
        Some(lines) => format("up to {lines} changed lines", &[("lines", lines.to_string())]),
        None => tr("no line limit").to_owned(),
    }
}

/// The one-line branch-protection recommendation, with its state when known.
fn protection(watch: &Watch, repo: &Repo) -> String {
    match repo {
        Repo::Read { branch, protected: Some(true), .. } => format("GitHub branch protection is on for {branch}. Keep it on.", &[("branch", branch.clone())]),
        Repo::Read { branch, protected: Some(false), .. } => format("Recommended: turn on GitHub branch protection for {branch} (it is off).", &[("branch", branch.clone())]),
        _ => format("Recommended: turn on GitHub branch protection for the default branch of {repo}.", &[("repo", watch.repo.clone())]),
    }
}
