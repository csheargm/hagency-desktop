//! Agent auto-merge policy: humans set the policy, agents review and merge
//! routine changes, humans handle the exceptions.
//!
//! The policy lives in the repository at [`POLICY_PATH`] and is always read
//! from the pull request's BASE branch, so a pull request cannot loosen the
//! policy that judges it. [`evaluate`] is pure: diff stats, paths, CI checks and
//! the Agents that approved in Hagency go in; [`Decision::Merge`] or
//! [`Decision::NeedsHuman`] with a reason comes out. Anything it cannot prove
//! low risk needs a person (an empty diff, a missing policy, pending CI).
//!
//! Separation: an approval only counts when it came from a different Agent
//! than the one that authored (and asks to merge) the pull request, and at
//! least one such approval is always required, whatever the file says.
//! The per-Agent switch is OFF by default ([`gate`]).
pub mod gate;

/// Where the policy lives in a repository.
pub const POLICY_PATH: &str = ".hagency/merge-policy.yml";
/// Paths that are never low risk: the policy itself and CI/workflow config.
const PROTECTED_PREFIXES: [&str; 2] = [".hagency/", ".github/"];
const DEFAULT_MAX_CHANGED_LINES: u64 = 50;
const MIN_AGENT_APPROVALS: u32 = 1;

/// The owner's policy, as read from [`POLICY_PATH`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergePolicy {
    pub low_risk_paths: Vec<String>,
    pub max_changed_lines: u64,
    pub required_agent_approvals: u32,
    pub require_ci: bool,
}

/// One changed file of the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
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
/// it; `author` is the Agent that opened it and asks to merge it.
pub fn evaluate(policy: &MergePolicy, facts: &PrFacts, approvers: &[String], author: &str) -> Decision {
    let human = |reason: String| Decision::NeedsHuman(reason);
    if !facts.open {
        return human("the pull request is not open".into());
    }
    if facts.files.is_empty() {
        return human("the pull request changes no files".into());
    }
    if let Some(file) = facts.files.iter().find(|f| PROTECTED_PREFIXES.iter().any(|p| f.path.starts_with(p))) {
        return human(format!("{} is policy or CI configuration", file.path));
    }
    if let Some(file) = facts.files.iter().find(|f| !policy.low_risk_paths.iter().any(|g| glob_match(g, &f.path))) {
        return human(format!("{} is outside the low-risk paths", file.path));
    }
    let lines: u64 = facts.files.iter().map(|f| f.additions.saturating_add(f.deletions)).sum();
    if lines > policy.max_changed_lines {
        return human(format!("{lines} changed lines is over the limit of {}", policy.max_changed_lines));
    }
    if policy.require_ci {
        if facts.checks.contains(&Check::Failed) {
            return human("a CI check failed".into());
        }
        if facts.checks.contains(&Check::Pending) {
            return human("CI checks are still running".into());
        }
    }
    let mut others: Vec<&String> = approvers.iter().filter(|a| a.as_str() != author).collect();
    others.sort();
    others.dedup();
    let required = policy.required_agent_approvals.max(MIN_AGENT_APPROVALS) as usize;
    if others.len() < required {
        return human(format!("needs {required} approval(s) from another Agent, has {}", others.len()));
    }
    Decision::Merge
}

/// Read the small YAML subset the policy uses: `key: value`, `key: [a, b]`
/// and `key:` followed by `- item` lines. Unknown keys are refused, so a typo
/// never silently loosens or drops a rule.
pub fn parse_policy(text: &str) -> Option<MergePolicy> {
    let mut policy = MergePolicy { low_risk_paths: Vec::new(), max_changed_lines: DEFAULT_MAX_CHANGED_LINES, required_agent_approvals: MIN_AGENT_APPROVALS, require_ci: true };
    let mut in_list = false;
    for raw in text.lines() {
        let line = raw.split(" #").next()?.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            if !in_list {
                return None;
            }
            policy.low_risk_paths.push(unquote(item)?);
            continue;
        }
        in_list = false;
        let (key, value) = trimmed.split_once(':')?;
        let value = value.trim();
        match key.trim() {
            "low_risk_paths" if value.is_empty() => in_list = true,
            "low_risk_paths" => policy.low_risk_paths = inline_list(value)?,
            "max_changed_lines" => policy.max_changed_lines = value.parse().ok()?,
            "required_agent_approvals" => policy.required_agent_approvals = value.parse().ok()?,
            "require_ci" => policy.require_ci = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(policy)
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
