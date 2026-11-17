//! The GitHub issue watch of one owned Agent, kept in the same profile ledger
//! as the Agent policy, so the owner check and ledger opening match it.
//! GET returns `{issueWatch, issueSeen, work, plans, track, ...}` (`issueSeen`: issue numbers already
//! handed off for the watched repository). POST applies `{issueWatch}` and/or
//! `{issueSeen: {repo, number}}` in one transaction, then answers like GET.
//! Writes need this device to be the Agent's execution device.
use super::*;
use hagency_agent_local::issue_watch::WatchSetting;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    /// The watch itself (`off`, `labelled`, `trusted` or `all`).
    #[serde(default)]
    issue_watch: Option<WatchSetting>,
    /// One issue the desktop poller handed to the Agent (after the send succeeded).
    #[serde(default)]
    issue_seen: Option<SeenIssue>,
    /// The Agent's whole work list (repositories and roles), replacing the old one.
    #[serde(default)]
    work: Option<Vec<hagency_agent_local::merge_policy::work::OwnerRules>>,
    /// The review loop's records and questions (`review_loop`).
    #[serde(default)]
    review: Option<super::review_loop::ReviewChange>,
    /// A planner card the owner filed, or one whose "Done" was posted.
    #[serde(default)]
    plan: Option<PlanChange>,
    /// The issue loop's follow-through record of one issue (ADR 0018).
    #[serde(default)]
    track: Option<hagency_agent_local::issue_track::TrackChange>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanChange {
    /// Reserve the plan (all issue numbers 0) before anything is filed.
    #[serde(default)]
    filed: Option<hagency_agent_local::plans::PlanRecord>,
    /// One step's issue number, right after GitHub created it.
    #[serde(default)]
    issue: Option<PlanIssue>,
    #[serde(default)]
    done: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanIssue {
    id: String,
    index: usize,
    number: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeenIssue {
    repo: String,
    number: i64,
}

pub(super) async fn handle(req: &mut Request, depot: &Depot, agent: String) -> Result<Value, OwnerError> {
    if req.uri().query().is_some() {
        return Err(error(400, "invalid_arguments"));
    }
    let change = match req.method().clone() {
        Method::GET => None,
        Method::POST => {
            require_execution_device(req, depot, &agent).await?;
            Some(parse::<Change>(req).await?)
        }
        _ => return Err(error(405, "unsupported_operation")),
    };
    let reply = api(req, depot, OwnerOperation::Agents).await?;
    let agents: Vec<ServerAgent> = serde_json::from_value(reply.value["agents"].clone())
        .map_err(|_| error(502, "invalid_server_response"))?;
    if !agents.iter().any(|record| record.id == agent) {
        return Err(error(403, "owner_scope_required"));
    }
    let root = console(depot)
        .map_err(|_| error(503, "local_state_unavailable"))?
        .0
        .server_login
        .state_directory()
        .ok_or_else(|| error(503, "local_state_unavailable"))?;
    recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || apply(&root, &reply, &agent, change, &runtime))
        .await
        .map_err(|_| error(503, "local_state_unavailable"))?
}

fn apply(root: &Path, reply: &OwnerReply, agent: &str, change: Option<Change>, runtime: &tokio::runtime::Handle) -> Result<Value, OwnerError> {
    let path = ledger_path(root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(503, "local_state_unavailable"))?;
    let identity = profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(403, "owner_scope_required"))?;
    let mut ledger = Ledger::open_scoped(&path, &reply.owner, &identity).map_err(local_error)?;
    let mut review = Value::Null;
    // The reply names the plan a change touched, done or not (`plan`).
    let plan_query = change.as_ref().and_then(|c| c.plan.as_ref()).and_then(|p| p.filed.as_ref().map(|f| f.id.clone()).or_else(|| p.issue.as_ref().map(|i| i.id.clone())));
    if let Some(mut change) = change {
        let loop_change = change.review.take();
        // All parts of one POST commit together or not at all.
        ledger
            .atomically(|ledger| apply_change(ledger, &reply.owner, agent, change))
            .map_err(local_error)?;
        if let Some(loop_change) = loop_change {
            // Not in the transaction: a readiness question waits on GitHub.
            review = super::review_loop::apply(&mut ledger, agent, loop_change, now(), runtime).map_err(local_error)?;
        }
    }
    let watch = ledger.issue_watch(agent).map_err(local_error)?;
    let mut seen: Vec<i64> = ledger
        .seen_issues(agent, &watch.setting.repo)
        .map_err(local_error)?
        .into_iter()
        .collect();
    seen.sort_unstable();
    let work = ledger.all_owner_rules(agent).map_err(local_error)?;
    let mut work_seen = serde_json::Map::new();
    for entry in &work {
        let mut numbers: Vec<i64> = ledger.seen_issues(agent, &entry.repo).map_err(local_error)?.into_iter().collect();
        numbers.sort_unstable();
        work_seen.insert(entry.repo.clone(), numbers.into());
    }
    let plans = ledger.open_plans(agent).map_err(local_error)?;
    let track = ledger.issue_tracks(agent).map_err(local_error)?;
    let plan = match &plan_query { Some(id) => ledger.plan(agent, id).map_err(local_error)?.map(|(p, done)| json!({"plan": p, "done": done})).unwrap_or(Value::Null), None => Value::Null };
    Ok(json!({"plan": plan, "agentId": agent, "issueWatch": watch, "issueSeen": seen, "work": work, "workSeen": work_seen, "review": review, "plans": plans, "track": track}))
}

fn apply_change(ledger: &mut Ledger, owner: &str, agent: &str, change: Change) -> hagency_agent_local::Result<()> {
    if let Some(setting) = change.issue_watch {
        ledger.set_issue_watch(owner, agent, setting, now())?;
    }
    if let Some(work) = change.work {
        ledger.set_work(owner, agent, work)?;
    }
    if let Some(seen) = change.issue_seen {
        ledger.mark_issue_seen(agent, &seen.repo, seen.number)?;
    }
    if let Some(track) = change.track {
        ledger.track_issue(agent, &track, now())?;
    }
    if let Some(plan) = change.plan {
        if let Some(filed) = plan.filed {
            ledger.record_plan(owner, agent, filed)?;
        }
        if let Some(step) = plan.issue {
            ledger.set_plan_issue(owner, agent, &step.id, step.index, step.number)?;
        }
        if let Some(id) = plan.done {
            ledger.finish_plan(owner, agent, &id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hagency_agent_local::issue_watch::WatchMode;

    #[test]
    fn issue_watch_changes_parse_and_bad_ones_do_not() {
        for (wire, mode) in [("off", WatchMode::Off), ("labelled", WatchMode::Labelled), ("trusted", WatchMode::Trusted), ("all", WatchMode::All)] {
            let change: Change = serde_json::from_value(json!({"issueWatch": {"mode": wire, "label": "agent-fix", "repo": "acme/app", "roomId": "!r:x"}})).unwrap();
            let setting = change.issue_watch.unwrap();
            assert_eq!((setting.mode, setting.repo.as_str(), setting.room_id.as_str()), (mode, "acme/app", "!r:x"));
        }
        let off: Change = serde_json::from_value(json!({"issueWatch": {"mode": "off"}})).unwrap();
        assert_eq!(off.issue_watch.unwrap().mode, WatchMode::Off);
        assert!(serde_json::from_value::<Change>(json!({"issueWatch": {"mode": "sometimes"}})).is_err());
        assert!(serde_json::from_value::<Change>(json!({"issueWatch": {"mode": "all", "merge": true}})).is_err(), "unknown fields are refused");
        assert!(serde_json::from_value::<Change>(json!({"mode": "ask"})).is_err(), "unknown parts are refused");
    }

    #[test]
    fn issue_seen_changes_parse_and_bad_ones_do_not() {
        let change: Change = serde_json::from_value(json!({"issueSeen": {"repo": "acme/app", "number": 7}})).unwrap();
        let seen = change.issue_seen.unwrap();
        assert_eq!((seen.repo.as_str(), seen.number), ("acme/app", 7));
        assert!(serde_json::from_value::<Change>(json!({"issueSeen": {"repo": "acme/app"}})).is_err());
        assert!(serde_json::from_value::<Change>(json!({"issueSeen": {"repo": "acme/app", "number": 7, "close": true}})).is_err());
    }

    #[test]
    fn track_changes_ride_with_the_seen_record() {
        let change: Change = serde_json::from_value(json!({"issueSeen": {"repo": "a/b", "number": 3}, "track": {"repo": "a/b", "number": 3, "handed": 1}})).unwrap();
        assert_eq!(change.track.unwrap().handed, Some(1));
        assert!(serde_json::from_value::<Change>(json!({"track": {"repo": "a/b", "number": 3, "merge": true}})).is_err());
    }

    #[test]
    fn plan_changes_parse_and_unknown_fields_are_refused() {
        let change: Change = serde_json::from_value(json!({"plan": {"filed": {"id": "plan_1", "repo": "a/b", "roomId": "!r:x", "issues": [4]}}})).unwrap();
        assert_eq!(change.plan.unwrap().filed.unwrap().issues, [4]);
        assert!(serde_json::from_value::<Change>(json!({"plan": {"done": "plan_1", "merge": true}})).is_err());
    }
}
