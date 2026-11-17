//! The planner (ADR 0017): asked in a room for a feature, an Agent turns the
//! request into an ordered list of GitHub issues with acceptance criteria and
//! posts it with `plan_propose` as a card (an ordinary message). Nothing is
//! filed by the Agent: the Agent's owner reviews or edits the card and clicks
//! "File these issues"; the owner's device then files them with the owner's
//! own GitHub token and reports "Done" once every issue is closed as completed.
use crate::room_tools::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PLAN_PROPOSE_TOOL: &str = "plan_propose";
/// Key of the plan inside the card's message content.
pub const PLAN_KEY: &str = "io.hagency.plan";
pub const PLAN_MAX_ISSUES: usize = 12;
pub const PLAN_TITLE_BYTES: usize = 200;
pub const PLAN_BODY_BYTES: usize = 4 * 1024;
pub const PLAN_CRITERIA: usize = 10;
/// The line every filed issue's body ends with, naming its plan.
pub const PLAN_MARKER: &str = "Part of Hagency plan";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanIssue {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub repo: String,
    pub title: String,
    /// In the order they should be done.
    pub issues: Vec<PlanIssue>,
}

fn line_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.contains(['\n', '\r'])
}
fn repo_ok(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.len() <= 100 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)));
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

impl Plan {
    /// Within the limits a card and an issue body can carry.
    pub fn validate(self) -> Result<Self, Error> {
        let issues_ok = !self.issues.is_empty() && self.issues.len() <= PLAN_MAX_ISSUES && self.issues.iter().all(|i| {
            line_ok(&i.title, PLAN_TITLE_BYTES) && i.body.len() <= PLAN_BODY_BYTES && i.acceptance.len() <= PLAN_CRITERIA
                && i.acceptance.iter().all(|a| line_ok(a, PLAN_TITLE_BYTES))
        });
        if repo_ok(&self.repo) && line_ok(&self.title, PLAN_TITLE_BYTES) && issues_ok { Ok(self) } else { Err(Error::Malformed) }
    }
    /// The GitHub issue body for issue `index` of plan `id`.
    pub fn issue_body(&self, index: usize, id: &str) -> String {
        let issue = &self.issues[index];
        let mut body = issue.body.trim().to_owned();
        if !issue.acceptance.is_empty() {
            body.push_str("\n\n### Acceptance criteria\n");
            for item in &issue.acceptance {
                body.push_str(&format!("- [ ] {}\n", item.trim()));
            }
        }
        body.push_str(&format!("\n---\n{}", self.step_marker(index, id)));
        body.trim_start().to_owned()
    }
    /// The line that marks issue `index` of plan `id` on GitHub, so a retry
    /// can find an issue filed before its number was saved.
    pub fn step_marker(&self, index: usize, id: &str) -> String {
        format!("{PLAN_MARKER} `{id}` ({}), step {} of {}.", self.title.trim(), index + 1, self.issues.len())
    }
}

pub fn parse(arguments: &Value) -> Result<Plan, Error> {
    let plan: Plan = serde_json::from_value(arguments.clone()).map_err(|_| Error::Malformed)?;
    plan.validate()
}

/// The card an Agent posts; `id` is stable per dispatch and plan.
pub fn card_content(id: &str, plan: &Plan, requester: &str, source_event: &str) -> Value {
    let mut lines = vec![format!("Plan for {}: {}", plan.repo, plan.title)];
    for (n, issue) in plan.issues.iter().enumerate() {
        lines.push(format!("{}. {}", n + 1, issue.title));
    }
    lines.push("The Agent's owner can file these as GitHub issues from the card.".into());
    json!({"msgtype": "m.notice", "body": lines.join("\n"),
        PLAN_KEY: {"id": id, "plan": plan, "requester": requester, "source_event": source_event}})
}

/// A card's (id, plan, requester); anything malformed is no card.
pub fn from_content(content: &Value) -> Option<(String, Plan, String)> {
    let p = content.get(PLAN_KEY)?;
    let id = p["id"].as_str().filter(|id| id.starts_with("plan_") && id.len() <= 64)?;
    let plan = serde_json::from_value::<Plan>(p["plan"].clone()).ok()?.validate().ok()?;
    Some((id.into(), plan, p["requester"].as_str().unwrap_or_default().into()))
}

/// The room message once every issue of a plan is closed as completed.
pub fn done_text(numbers: &[u64]) -> String {
    let list: Vec<String> = numbers.iter().map(|n| format!("#{n}")).collect();
    format!("Done: {} merged", list.join(", "))
}

pub(crate) fn definition() -> Value {
    json!({"name": PLAN_PROPOSE_TOOL,
        "description": "When someone in the room asks for a feature or a piece of software, turn the request into a plan: an ordered list of GitHub issues for one repository, each small enough for one pull request, with a clear title, a short body and acceptance criteria. This only posts the plan as a card; it files nothing. Your owner reviews or edits it and files the issues with one click; developer Agents then pick them up, and the room is told when all are merged. Ask a question first if the repository or the goal is unclear. Never put secrets in a plan.",
        "inputSchema": {"type": "object", "properties": {
            "repo": {"type": "string", "description": "owner/name"},
            "title": {"type": "string", "maxLength": PLAN_TITLE_BYTES},
            "issues": {"type": "array", "minItems": 1, "maxItems": PLAN_MAX_ISSUES, "items": {"type": "object", "properties": {
                "title": {"type": "string", "maxLength": PLAN_TITLE_BYTES},
                "body": {"type": "string", "maxLength": PLAN_BODY_BYTES},
                "acceptance": {"type": "array", "maxItems": PLAN_CRITERIA, "items": {"type": "string", "maxLength": PLAN_TITLE_BYTES}}},
                "required": ["title"], "additionalProperties": false}}},
            "required": ["repo", "title", "issues"], "additionalProperties": false}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Value {
        json!({"repo": "acme/sandbox", "title": "Glossary and quick start", "issues": [
            {"title": "Add a Glossary", "body": "Define the terms.", "acceptance": ["README has a Glossary section"]},
            {"title": "Add a Quick start"}]})
    }

    #[test]
    fn a_plan_round_trips_through_its_card() {
        let plan = parse(&args()).unwrap();
        let card = card_content("plan_1", &plan, "@anna:x", "$req");
        assert!(card["body"].as_str().unwrap().contains("2. Add a Quick start"));
        assert_eq!(from_content(&card), Some(("plan_1".into(), plan.clone(), "@anna:x".into())));
        let body = plan.issue_body(0, "plan_1");
        assert!(body.contains("- [ ] README has a Glossary section") && body.contains("step 1 of 2") && body.contains(PLAN_MARKER));
        assert_eq!(done_text(&[4, 5]), "Done: #4, #5 merged");
    }

    #[test]
    fn malformed_plans_are_refused() {
        let mut too_many = args();
        too_many["issues"] = json!(vec![json!({"title": "x"}); PLAN_MAX_ISSUES + 1]);
        let mut bad_title = args();
        bad_title["issues"][0]["title"] = json!("two\nlines");
        for bad in [json!({"repo": "acme", "title": "t", "issues": [{"title": "x"}]}), json!({"repo": "a/b", "title": "t", "issues": []}),
            too_many, bad_title, json!({"repo": "a/b", "title": "t", "issues": [{"title": "x"}], "file": true})] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        assert_eq!(from_content(&json!({PLAN_KEY: {"id": "x", "plan": args()}})), None, "ids are host-made");
    }
}
