//! What a watch's Safeguards card shows (ADR 0026), as the code enforces it:
//! the engine's own limits and protected names, re-exported so the card never
//! repeats a number, and one read-only GitHub read of the watched
//! repository's default branch (its name and whether GitHub protects it).
//! The merge rules are the room's (ADR 0029), read by the desktop from the
//! room state; no repository file is read.
use super::{FetchError, get_as, pulls_url};
use serde_json::Value;

/// The issue loop's limits (ADR 0020).
pub use hagency_agent_local::issue_track::{HANDOFF_TIMEOUT_SECS, MAX_ATTEMPTS, MAX_FREE_RETRIES};
/// The merge gate's parser and what always needs a person.
pub use hagency_agent_local::merge_policy::{CODEOWNERS, MIN_AGENT_APPROVALS, MergePolicy, PROTECTED_PREFIXES, parse_policy};
/// The room's merge rules: their state type, (de)serialization and the suggested defaults.
pub use hagency_agent_local::merge_policy::room_rules::{
    MAX_POLICY_BYTES, MERGE_RULES_STATE_TYPE, NO_ROOM_RULES, ROOM_RULES_TTL_SECS, RoomRulesSnapshot, SUGGESTED_ALWAYS_HUMAN, SUGGESTED_LOW_RISK_PATHS, content_of, policy_text,
    render_policy, rules_of, suggested_policy,
};
/// The room's autonomy level and Pause (ADR 0030): state types, content and reasons.
pub use hagency_agent_local::room_autonomy;
/// Branches push-freely never pushes, whatever the remote says (ADR 0022).
pub use hagency_agent_local::watch_trust::git::PROTECTED as PROTECTED_BRANCHES;

/// A room's merge rules, as the gate would judge them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomRulesState {
    /// No (accepted) rules event: the gate leaves every merge to a person.
    Absent,
    /// Rules the gate refuses (an unknown key, a bad value): also a person.
    Unreadable,
    Read(MergePolicy),
}

impl RoomRulesState {
    /// The state of a merge-rules event's content (`None`: no event).
    pub fn of(content: Option<&Value>) -> Self {
        match content {
            None => Self::Absent,
            Some(content) => rules_of(content).map_or(Self::Unreadable, Self::Read),
        }
    }
}

/// The default branch of a watched repository and how it is guarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoGuards {
    pub branch: String,
    /// GitHub's own `protected` flag; `None` when it could not be read.
    pub protected: Option<bool>,
}

/// The default branch and its protection flag of `repo`.
/// A protection read that fails is "unknown", never "off".
pub async fn fetch_repo_guards(client: &super::Client, api: &str, repo: &str, token: &str) -> Result<RepoGuards, FetchError> {
    pulls_url(api, repo)?; // refuses anything but owner/name
    let info: Value = parse_json(&get_as(client, &format!("{api}/repos/{repo}"), Some(token)).await?)?;
    let branch = info["default_branch"].as_str().ok_or_else(|| FetchError::Parse("a repository without a default branch".into()))?.to_owned();
    let protected = get_as(client, &format!("{api}/repos/{repo}/branches/{branch}"), Some(token)).await.ok().and_then(|body| protected_flag(&body));
    Ok(RepoGuards { branch, protected })
}

/// The `protected` flag of GitHub's branch reply.
pub fn protected_flag(body: &str) -> Option<bool> {
    parse_json(body).ok()?["protected"].as_bool()
}

fn parse_json(body: &str) -> Result<Value, FetchError> {
    serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn room_rules_are_parsed_by_the_gates_own_parser() {
        let text = "low_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequire_ci: false\n";
        let RoomRulesState::Read(policy) = RoomRulesState::of(Some(&content_of(text))) else { panic!("expected rules") };
        assert_eq!(policy, parse_policy(text).unwrap());
        assert_eq!(RoomRulesState::of(Some(&content_of("auto_merge_everything: true\n"))), RoomRulesState::Unreadable);
        assert_eq!(RoomRulesState::of(Some(&json!({"rules": 1}))), RoomRulesState::Unreadable, "no policy text");
        assert_eq!(RoomRulesState::of(None), RoomRulesState::Absent);
    }

    #[test]
    fn the_protection_flag_is_read_or_unknown() {
        assert_eq!(protected_flag(r#"{"name":"main","protected":true}"#), Some(true));
        assert_eq!(protected_flag(r#"{"name":"main","protected":false}"#), Some(false));
        assert_eq!(protected_flag("not json"), None);
    }
}
