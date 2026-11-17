//! The review loop's part of the issue-watch owner API (ADR 0015): the
//! desktop poller records admitted remote reviews, posted shared reviews,
//! once-per-head loop steps and the room's trust check, and asks whether a
//! merge would qualify now. Everything is validated here; the ledger and the
//! merge gate stay the authority.
use hagency_agent_local::merge_policy::github::{commit_sha, GhCli};
use hagency_agent_local::merge_policy::gate::{GhPr, Kind};
use hagency_agent_local::merge_policy::review_loop::{merge_readiness, HOLD_STEP};
use hagency_agent_local::merge_policy::{reason_kind, Decision, CI_PENDING};
use hagency_agent_local::{Error, Ledger, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// The loop steps taken once per pull request head.
pub(super) const STEPS: [&str; 2] = ["review", "merge"];
const MAX_STEP_LEN: usize = 96;

fn known_step(step: &str) -> bool {
    STEPS.contains(&step)
        || step.strip_prefix(HOLD_STEP).is_some_and(|rest| !rest.is_empty() && step.len() <= MAX_STEP_LEN && rest.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReviewChange {
    #[serde(default)]
    claim_step: Option<Step>,
    #[serde(default)]
    step_taken: Option<Step>,
    #[serde(default)]
    work_claim: Option<Claim>,
    #[serde(default)]
    readiness: Option<Readiness>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    step: String,
    pr: String,
    head: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Claim {
    repo: String,
    number: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Readiness {
    repo: String,
    number: u64,
}

fn pr_head(step: &Step) -> Result<()> {
    if !step.pr.starts_with("https://github.com/") || step.pr.contains(char::is_whitespace) || commit_sha(&step.head).is_none() {
        return Err(Error::Invalid("pull request must be a github.com URL and head a full commit SHA"));
    }
    if !known_step(&step.step) {
        return Err(Error::Invalid("unknown review-loop step"));
    }
    Ok(())
}

/// Whether `repo` is on the Agent's work list (or its legacy watch).
fn assigned(ledger: &Ledger, agent: &str, repo: &str) -> Result<bool> {
    let legacy = ledger.issue_watch(agent)?.setting.repo;
    Ok(legacy.eq_ignore_ascii_case(repo) || ledger.owner_rules(agent, repo)?.is_some())
}

/// Apply one change for `agent`; the reply's `review` part. `runtime` runs
/// the GitHub reads of a readiness question.
pub(super) fn apply(ledger: &mut Ledger, agent: &str, change: ReviewChange, now: i64, runtime: &tokio::runtime::Handle) -> Result<Value> {
    let mut reply = json!({});
    if let Some(step) = change.step_taken {
        pr_head(&step)?;
        reply["taken"] = ledger.loop_step_taken(agent, &step.step, &step.pr, &step.head)?.into();
    }
    if let Some(step) = change.claim_step {
        pr_head(&step)?;
        reply["claimed"] = ledger.claim_loop_step(agent, &step.step, &step.pr, &step.head)?.into();
    }
    if let Some(claim) = change.work_claim {
        if !assigned(ledger, agent, &claim.repo)? {
            return Err(Error::Invalid("the repository is not on this Agent's work list"));
        }
        reply["claimed"] = ledger.record_work_claim(agent, &claim.repo, claim.number, now)?.into();
    }
    reply["claimsLeft"] = ledger.claims_left_today(agent, now)?.into();
    if let Some(ask) = change.readiness {
        if !assigned(ledger, agent, &ask.repo)? {
            return Err(Error::Invalid("the repository is not on this Agent's work list"));
        }
        let command = GhPr { dir: std::env::temp_dir(), number: ask.number, repo: Some(ask.repo.clone()), head: None, kind: Kind::Merge, body: String::new() };
        // Read as the Agent, the same login its own gh uses (GPT review, Nov 16).
        let token = crate::github_access::TokenStore::current().runtime_environment(agent).unwrap_or_default().agent_github_token();
        let judged = runtime.block_on(merge_readiness(ledger, &command, agent, &GhCli { token }));
        reply["readiness"] = readiness_json(judged.as_ref().map(|(view, decision)| (view.head_sha.as_str(), decision)));
        if let Some((view, _)) = &judged {
            // Unverified: only tells "no review yet" (wait) from "a person must decide".
            reply["readiness"]["approvers"] = ledger.agent_reviewers_count(&view.url, &view.head_sha)?.into();
            // The merger's own "Pull request merges" switch: without it the
            // gate never answers the merge, so the loop says so instead.
            reply["readiness"]["autoMerge"] = ledger.auto_merge(agent)?.into();
        }
    }
    Ok(reply)
}

/// `{decision: merge|wait|human, head, reason, kind}`; unreadable facts ask a
/// person. `kind` (`approvals`, `ci`, `rules`) says what a person's decision is about.
pub(super) fn readiness_json(judged: Option<(&str, &Decision)>) -> Value {
    match judged {
        Some((head, Decision::Merge)) => json!({"decision": "merge", "head": head}),
        Some((head, Decision::NeedsHuman(reason))) if reason == CI_PENDING => json!({"decision": "wait", "head": head, "reason": reason}),
        Some((head, Decision::NeedsHuman(reason))) => json!({"decision": "human", "head": head, "reason": reason, "kind": reason_kind(reason)}),
        None => json!({"decision": "human", "reason": "GitHub or the merge policy could not be read", "kind": "rules"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_changes_parse_strictly() {
        let sha = "a".repeat(40);
        let ok: ReviewChange = serde_json::from_value(json!({"claimStep": {"step": "merge", "pr": "https://github.com/a/b/pull/1", "head": sha}, "workClaim": {"repo": "a/b", "number": 3}, "readiness": {"repo": "a/b", "number": 1}})).unwrap();
        assert!(ok.claim_step.is_some() && ok.work_claim.is_some() && ok.readiness.is_some());
        assert!(serde_json::from_value::<ReviewChange>(json!({"merge": true})).is_err());
        let step = |s: &str, pr: &str, head: &str| Step { step: s.into(), pr: pr.into(), head: head.into() };
        assert!(pr_head(&step("merge", "https://github.com/a/b/pull/1", &sha)).is_ok());
        assert!(pr_head(&step("merge", "http://evil/a", &sha)).is_err());
        assert!(pr_head(&step("merge", "https://github.com/a/b/pull/1", "main")).is_err());
        assert!(pr_head(&step("deploy", "https://github.com/a/b/pull/1", &sha)).is_err());
        assert!(pr_head(&step("hold-approvals-merges-off", "https://github.com/a/b/pull/1", &sha)).is_ok());
        for bad in ["hold-", "hold-Approvals", "hold-a b", "hold-a_b", &format!("hold-{}", "a".repeat(MAX_STEP_LEN))] {
            assert!(pr_head(&step(bad, "https://github.com/a/b/pull/1", &sha)).is_err(), "{bad}");
        }
    }

    #[test]
    fn pending_ci_waits_and_everything_else_unsure_asks_a_person() {
        assert_eq!(readiness_json(Some(("h", &Decision::Merge)))["decision"], "merge");
        assert_eq!(readiness_json(Some(("h", &Decision::NeedsHuman(CI_PENDING.into()))))["decision"], "wait");
        assert_eq!(readiness_json(Some(("h", &Decision::NeedsHuman("code".into()))))["decision"], "human");
        let approvals = readiness_json(Some(("h", &Decision::NeedsHuman("needs 1 approval(s) from another Agent, has 0".into()))));
        assert_eq!((approvals["decision"].as_str(), approvals["kind"].as_str()), (Some("human"), Some("approvals")));
        assert_eq!(readiness_json(None)["kind"], "rules");
        assert_eq!(readiness_json(None)["decision"], "human");
    }
}
