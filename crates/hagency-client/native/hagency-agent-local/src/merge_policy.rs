//! Agent auto-merge policy: humans set the policy, agents review and merge
//! routine changes, humans handle the exceptions.
//!
//! The policy lives in the repository at [`POLICY_PATH`] and is always read
//! from the pull request's BASE branch, so a pull request cannot loosen the
//! policy that judges it; the gate reads it at the base COMMIT the pull
//! request was judged against, not the moving branch name. [`evaluate`] is pure: diff stats, paths, CI checks and
//! the Agents that approved in Hagency go in; [`Decision::Merge`] or
//! [`Decision::NeedsHuman`] with a reason comes out. Anything it cannot prove
//! low risk needs a person (an empty diff, a missing policy, pending CI).
//!
//! Separation: an approval only counts when it came from a different Agent
//! than the one that asks to merge the pull request, and at
//! least one such approval is always required, whatever the file says.
//! The per-Agent switch is OFF by default ([`gate`]).
//!
//! Tiers (ADR 0016): the default tier is today's low-risk paths under a line
//! limit. `tier: trusted` is the repository's opt-in for any change
//! (optionally narrowed by `paths:` and `max_lines:`), with CI always
//! required; protected paths and `always_human:` globs still need a person.
//! A maintainer's own rules ([`work`]) are judged too; every policy
//! that applies must pass, so the file can only tighten them.
pub mod gate;
pub mod github;
pub mod work;
pub mod review_loop;

/// Where the policy lives in a repository.
pub const POLICY_PATH: &str = ".hagency/merge-policy.yml";
/// Paths that are never low risk: the policy itself and CI/workflow config.
const PROTECTED_PREFIXES: [&str; 2] = [".hagency/", ".github/"];
/// A file with this name, in any directory, assigns code owners: always a person.
const CODEOWNERS: &str = "CODEOWNERS";
/// The reason a merge waits rather than asks: CI has not finished.
pub const CI_PENDING: &str = "CI checks are still running";
const DEFAULT_MAX_CHANGED_LINES: u64 = 50;
const MIN_AGENT_APPROVALS: u32 = 1;

/// The owner's policy, as read from [`POLICY_PATH`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergePolicy {
    pub low_risk_paths: Vec<String>,
    pub max_changed_lines: u64,
    pub required_agent_approvals: u32,
    pub require_ci: bool,
    /// `tier: trusted`: any change in `trusted_paths` (empty: every path).
    pub trusted: bool,
    pub trusted_paths: Vec<String>,
    /// `max_lines:` of the trusted tier; `None` means no line limit.
    pub trusted_max_lines: Option<u64>,
    /// Globs that always need a person, in either tier.
    pub always_human: Vec<String>,
    /// The owner rules' CI rule: a head with no checks at all passes; any
    /// reported check must still be green.
    pub ci_if_present: bool,
}


/// One changed file of the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// The old path when GitHub reports a rename or copy.
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    /// True when the change has no reviewable lines (binary, mode-only or a
    /// status other than added, modified or removed).
    pub opaque: bool,
}

/// The state of one CI check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Passed,
    Pending,
    Failed,
}

/// What GitHub says about the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrFacts {
    pub open: bool,
    pub files: Vec<ChangedFile>,
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Merge,
    NeedsHuman(String),
}

/// Judge one pull request. `approvers` are the Hagency Agents that approved
/// its current head; `merger` is the Agent asking to merge it.
///
/// TODO(pr-author): the Agent that OPENED the pull request is not known here
/// (every Agent shares one GitHub login), so only the merger is excluded.
/// Record an authenticated author Agent at PR creation and exclude it too.
pub fn evaluate(policy: &MergePolicy, facts: &PrFacts, approvers: &[String], merger: &str) -> Decision {
    if let Some(reason) = file_reason(policy, facts).or_else(|| ci_reason(policy, facts)) {
        return Decision::NeedsHuman(reason);
    }
    let mut others: Vec<&String> = approvers.iter().filter(|a| a.as_str() != merger).collect();
    others.sort();
    others.dedup();
    let required = policy.required_agent_approvals.max(MIN_AGENT_APPROVALS) as usize;
    if others.len() < required {
        return Decision::NeedsHuman(format!("needs {required} approval(s) from another Agent, has {}", others.len()));
    }
    Decision::Merge
}

/// Why `facts` is not a low-risk change under `policy` (open, files inside
/// the low-risk paths, under the line limit, nothing protected, renamed or
/// opaque); `None` when it is. Approvals and CI are not judged here.
pub(crate) fn file_reason(policy: &MergePolicy, facts: &PrFacts) -> Option<String> {
    if !facts.open {
        return Some("the pull request is not open".into());
    }
    if facts.files.is_empty() {
        return Some("the pull request changes no files".into());
    }
    let protected = |path: &str| {
        PROTECTED_PREFIXES.iter().any(|p| path.starts_with(p))
            || path.rsplit('/').next() == Some(CODEOWNERS)
            || policy.always_human.iter().any(|g| glob_match(g, path))
    };
    for file in &facts.files {
        if let Some(old) = &file.previous_path {
            return Some(format!("{} was renamed or copied from {old}", file.path));
        }
        if file.opaque {
            return Some(format!("{} has no reviewable line changes", file.path));
        }
        if protected(&file.path) {
            return Some(format!("{} is policy or CI configuration", file.path));
        }
        if !in_tier_paths(policy, &file.path) {
            return Some(format!("{} is outside the low-risk paths", file.path));
        }
    }
    let lines = facts.files.iter().try_fold(0u64, |sum, f| sum.checked_add(f.additions)?.checked_add(f.deletions));
    let limit = if policy.trusted { policy.trusted_max_lines } else { Some(policy.max_changed_lines) };
    match (lines, limit) {
        (None, _) => Some("the changed line count overflows".into()),
        (Some(lines), Some(limit)) => (lines > limit).then(|| format!("{lines} changed lines is over the limit of {limit}")),
        (Some(_), None) => None,
    }
}

/// The default tier needs a low-risk path; the trusted tier its `paths:` (all when empty).
fn in_tier_paths(policy: &MergePolicy, path: &str) -> bool {
    let globs = if policy.trusted { &policy.trusted_paths } else { &policy.low_risk_paths };
    (policy.trusted && globs.is_empty()) || globs.iter().any(|g| glob_match(g, path))
}

pub(crate) fn ci_reason(policy: &MergePolicy, facts: &PrFacts) -> Option<String> {
    if !policy.require_ci {
        return None;
    }
    if facts.checks.is_empty() && !policy.ci_if_present {
        return Some("no CI checks reported".into());
    }
    if facts.checks.contains(&Check::Failed) {
        return Some("a CI check failed".into());
    }
    facts.checks.contains(&Check::Pending).then(|| CI_PENDING.into())
}

/// Read the small YAML subset the policy uses: `key: value`, `key: [a, b]`
/// and `key:` followed by `- item` lines. Unknown keys are refused, so a typo
/// never silently loosens or drops a rule; trusted-tier keys without
/// `tier: trusted` are refused too, and that tier always requires CI.
pub fn parse_policy(text: &str) -> Option<MergePolicy> {
    let mut policy = MergePolicy {
        low_risk_paths: Vec::new(),
        max_changed_lines: DEFAULT_MAX_CHANGED_LINES,
        required_agent_approvals: MIN_AGENT_APPROVALS,
        require_ci: true,
        trusted: false,
        trusted_paths: Vec::new(),
        trusted_max_lines: None,
        always_human: Vec::new(),
        ci_if_present: false,
    };
    let mut list: Option<&str> = None;
    let mut tier_keys = false;
    for raw in text.lines() {
        let line = raw.split(" #").next()?.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            list_of(&mut policy, list?)?.push(unquote(item)?);
            continue;
        }
        list = None;
        let (key, value) = trimmed.split_once(':')?;
        let (key, value) = (key.trim(), value.trim());
        tier_keys |= matches!(key, "paths" | "max_lines");
        match key {
            "low_risk_paths" | "paths" | "always_human" if value.is_empty() => list = Some(key),
            "low_risk_paths" | "paths" | "always_human" => *list_of(&mut policy, key)? = inline_list(value)?,
            "max_changed_lines" => policy.max_changed_lines = value.parse().ok()?,
            "max_lines" => policy.trusted_max_lines = Some(value.parse().ok()?),
            "required_agent_approvals" => policy.required_agent_approvals = value.parse().ok()?,
            "require_ci" => policy.require_ci = value.parse().ok()?,
            "tier" => policy.trusted = match unquote(value)?.as_str() { "trusted" => true, "default" => false, _ => return None },
            _ => return None,
        }
    }
    if tier_keys && !policy.trusted {
        return None;
    }
    policy.require_ci |= policy.trusted;
    Some(policy)
}

fn list_of<'a>(policy: &'a mut MergePolicy, key: &str) -> Option<&'a mut Vec<String>> {
    match key {
        "low_risk_paths" => Some(&mut policy.low_risk_paths),
        "paths" => Some(&mut policy.trusted_paths),
        "always_human" => Some(&mut policy.always_human),
        _ => None,
    }
}

fn inline_list(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    inner.split(',').map(str::trim).filter(|s| !s.is_empty()).map(unquote).collect()
}

fn unquote(value: &str) -> Option<String> {
    let value = value.trim();
    let inner = [('"', '"'), ('\'', '\'')].iter().find_map(|(a, b)| value.strip_prefix(*a)?.strip_suffix(*b)).unwrap_or(value);
    (!inner.is_empty()).then(|| inner.to_owned())
}

/// `*` matches within one path part, `**` across parts (`**/` may be empty).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', b'/', rest @ ..] => (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && go(rest, &s[i..])),
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| go(rest, &s[i..])),
            [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| go(rest, &s[i..])),
            [c, rest @ ..] => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

#[cfg(test)]
mod tests;
