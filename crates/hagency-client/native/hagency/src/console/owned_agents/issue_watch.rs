//! The GitHub issue watch of one owned Agent, kept in the same profile ledger
//! as the Agent policy, so the owner check and ledger opening match it.
//! GET returns `{issueWatch, issueSeen}` (`issueSeen`: issue numbers already
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
    tokio::task::spawn_blocking(move || apply(&root, &reply, &agent, change))
        .await
        .map_err(|_| error(503, "local_state_unavailable"))?
}

fn apply(root: &Path, reply: &OwnerReply, agent: &str, change: Option<Change>) -> Result<Value, OwnerError> {
    let path = ledger_path(root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(503, "local_state_unavailable"))?;
    let identity = profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(403, "owner_scope_required"))?;
    let mut ledger = Ledger::open_scoped(&path, &reply.owner, &identity).map_err(local_error)?;
    if let Some(change) = change {
        // All parts of one POST commit together or not at all.
        ledger
            .atomically(|ledger| apply_change(ledger, &reply.owner, agent, change))
            .map_err(local_error)?;
    }
    let watch = ledger.issue_watch(agent).map_err(local_error)?;
    let mut seen: Vec<i64> = ledger
        .seen_issues(agent, &watch.setting.repo)
        .map_err(local_error)?
        .into_iter()
        .collect();
    seen.sort_unstable();
    Ok(json!({"agentId": agent, "issueWatch": watch, "issueSeen": seen}))
}

fn apply_change(ledger: &mut Ledger, owner: &str, agent: &str, change: Change) -> hagency_agent_local::Result<()> {
    if let Some(setting) = change.issue_watch {
        ledger.set_issue_watch(owner, agent, setting, now())?;
    }
    if let Some(seen) = change.issue_seen {
        ledger.mark_issue_seen(agent, &seen.repo, seen.number)?;
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
}
