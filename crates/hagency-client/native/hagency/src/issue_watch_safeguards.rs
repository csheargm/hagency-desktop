//! What a watch's Safeguards card shows (ADR 0026), as the code enforces it:
//! the engine's own limits and protected names, re-exported so the card never
//! repeats a number, and one read-only GitHub read of the watched
//! repository's default branch (its name, whether GitHub protects it, and the
//! merge policy file on it, parsed by the gate's own parser).
use super::{FetchError, get_as, pulls_url};
use base64::Engine;
use serde_json::Value;

/// The issue loop's limits (ADR 0020).
pub use hagency_agent_local::issue_track::{HANDOFF_TIMEOUT_SECS, MAX_ATTEMPTS, MAX_FREE_RETRIES};
/// The merge gate's file, its parser and what always needs a person.
pub use hagency_agent_local::merge_policy::{CODEOWNERS, MIN_AGENT_APPROVALS, MergePolicy, POLICY_PATH, PROTECTED_PREFIXES, parse_policy};
/// Branches push-freely never pushes, whatever the remote says (ADR 0022).
pub use hagency_agent_local::watch_trust::git::PROTECTED as PROTECTED_BRANCHES;

/// The merge policy file on the default branch, as the gate would judge it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyFile {
    /// No file: the gate leaves every merge to the owner's card.
    Absent,
    /// A file the gate refuses (an unknown key, a bad value): also the owner's card.
    Unreadable,
    Read(MergePolicy),
}

/// The default branch of a watched repository and how it is guarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoGuards {
    pub branch: String,
    /// GitHub's own `protected` flag; `None` when it could not be read.
    pub protected: Option<bool>,
    pub policy: PolicyFile,
}

/// The default branch, its protection flag and the policy file of `repo`.
/// A protection read that fails is "unknown", never "off".
pub async fn fetch_repo_guards(client: &super::Client, api: &str, repo: &str, token: &str) -> Result<RepoGuards, FetchError> {
    pulls_url(api, repo)?; // refuses anything but owner/name
    let info: Value = parse_json(&get_as(client, &format!("{api}/repos/{repo}"), Some(token)).await?)?;
    let branch = info["default_branch"].as_str().ok_or_else(|| FetchError::Parse("a repository without a default branch".into()))?.to_owned();
    let policy = match get_as(client, &format!("{api}/repos/{repo}/contents/{POLICY_PATH}"), Some(token)).await {
        Ok(body) => policy_from_contents(&body)?,
        Err(FetchError::Status { status: 404, .. }) => PolicyFile::Absent,
        Err(error) => return Err(error),
    };
    let protected = get_as(client, &format!("{api}/repos/{repo}/branches/{branch}"), Some(token)).await.ok().and_then(|body| protected_flag(&body));
    Ok(RepoGuards { branch, protected, policy })
}

/// GitHub's contents reply (base64 `content`) parsed by the gate's parser.
pub fn policy_from_contents(body: &str) -> Result<PolicyFile, FetchError> {
    let reply = parse_json(body)?;
    let encoded: String = reply["content"].as_str().ok_or_else(|| FetchError::Parse("a file without content".into()))?.split_whitespace().collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|e| FetchError::Parse(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(parse_policy(&text).map_or(PolicyFile::Unreadable, PolicyFile::Read))
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

    fn contents(text: &str) -> String {
        serde_json::json!({"content": base64::engine::general_purpose::STANDARD.encode(text)}).to_string()
    }

    #[test]
    fn the_policy_is_parsed_by_the_gates_own_parser() {
        let file = "low_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequire_ci: false\n";
        let PolicyFile::Read(policy) = policy_from_contents(&contents(file)).unwrap() else { panic!("expected a policy") };
        assert_eq!(policy, parse_policy(file).unwrap());
        assert!(!policy.require_ci);
    }

    #[test]
    fn a_policy_the_gate_refuses_reads_as_unreadable() {
        assert_eq!(policy_from_contents(&contents("auto_merge_everything: true\n")).unwrap(), PolicyFile::Unreadable);
        assert!(policy_from_contents("{}").is_err(), "no content is a read failure, not an absent file");
    }

    #[test]
    fn github_base64_with_line_breaks_decodes() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("require_ci: true\n");
        let (head, tail) = encoded.split_at(8);
        let body = serde_json::json!({"content": format!("{head}\n{tail}\n")}).to_string();
        assert!(matches!(policy_from_contents(&body).unwrap(), PolicyFile::Read(p) if p.require_ci));
    }

    #[test]
    fn the_protection_flag_is_read_or_unknown() {
        assert_eq!(protected_flag(r#"{"name":"main","protected":true}"#), Some(true));
        assert_eq!(protected_flag(r#"{"name":"main","protected":false}"#), Some(false));
        assert_eq!(protected_flag("not json"), None);
    }
}
