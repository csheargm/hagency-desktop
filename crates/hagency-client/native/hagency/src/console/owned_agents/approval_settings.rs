//! The "Always allow" rules and approval mode of one owned Agent (ported from
//! the original demo's `owned_agents/remembered.rs`), kept in the same profile
//! ledger as the Agent policy, so the owner check and ledger opening match it.
//! GET returns `{mode, rules}`; POST applies `{forget}` (revoke one rule)
//! and/or `{mode}` (`ask`, `remember` or `outside`) and/or `{autoMerge}` (the
//! opt-in agent auto-merge switch, off by default) and/or `{roomShare: {room,
//! answer, review, fix}}` (what the Agent does for members of one group room,
//! ADR 0024; GET lists the saved rooms as `roomShares`) in one transaction, then
//! answers like GET. Writes need this device to be the Agent's execution device.
//! Rules are only ever created by the owner's "Always allow this" on a card.
use super::*;
use hagency_agent_local::remembered::RememberedRequest;
use hagency_agent_local::room_share::SavedRoomShare;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    #[serde(default)]
    forget: Option<RememberedRequest>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    auto_merge: Option<bool>,
    #[serde(default)]
    room_share: Option<SavedRoomShare>,
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
        // Both parts of one POST commit together or not at all.
        ledger
            .atomically(|ledger| apply_change(ledger, &reply.owner, agent, change))
            .map_err(local_error)?;
    }
    let rules = ledger.remembered_requests(&reply.owner, agent).map_err(local_error)?;
    let mode = ledger.approval_mode(agent).map_err(local_error)?;
    let auto_merge = ledger.auto_merge(agent).map_err(local_error)?;
    let room_shares = ledger.room_shares(agent).map_err(local_error)?;
    let rules: Vec<Value> = rules.iter().map(|rule| json!({"tool": rule.tool, "directory": rule.directory, "request": rule.request, "created": rule.created, "room": rule.room, "label": label(rule)})).collect();
    Ok(json!({"agentId": agent, "mode": mode, "autoMerge": auto_merge, "roomShares": room_shares, "rules": rules}))
}

/// What the owner's panel names a rule by: the exact command, else the tool
/// and the file or pattern it was allowed on.
fn label(rule: &RememberedRequest) -> String {
    let request: Value = serde_json::from_str(&rule.request).unwrap_or_default();
    if let Some(command) = request["command"].as_str() {
        return command.to_owned();
    }
    // Codex argv (kept exactly, wrapper included): its words, space-separated.
    if let Some(words) = request["command"].as_array() {
        return words.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ");
    }
    let tool = rule.tool.rsplit('.').next().unwrap_or(&rule.tool);
    match ["file_path", "path", "pattern", "url"].iter().find_map(|k| request[*k].as_str()) {
        Some(target) => format!("{tool} {target}"),
        None => tool.to_owned(),
    }
}

fn apply_change(ledger: &mut Ledger, owner: &str, agent: &str, change: Change) -> hagency_agent_local::Result<()> {
    if let Some(rule) = change.forget {
        ledger.forget_request(owner, agent, &rule)?;
    }
    if let Some(mode) = change.mode {
        ledger.set_approval_mode(owner, agent, &mode)?;
    }
    if let Some(enabled) = change.auto_merge {
        ledger.set_auto_merge(owner, agent, enabled)?;
    }
    if let Some(saved) = change.room_share {
        ledger.set_room_share(owner, agent, &saved.room, saved.share())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_a_forget_and_or_a_mode_and_nothing_else() {
        let change: Change = serde_json::from_value(json!({"mode": "remember"})).unwrap();
        assert_eq!(change.mode.as_deref(), Some("remember"));
        let change: Change = serde_json::from_value(json!({"autoMerge": true})).unwrap();
        assert_eq!(change.auto_merge, Some(true));
        let change: Change = serde_json::from_value(json!({"roomShare": {"room": "!dev:s", "answer": true, "review": false, "fix": false}})).unwrap();
        assert!(!change.room_share.unwrap().review);
        // Every choice is explicit; nothing else (a merge rule) can ride along.
        assert!(serde_json::from_value::<Change>(json!({"roomShare": {"room": "!dev:s", "answer": true}})).is_err());
        assert!(serde_json::from_value::<Change>(json!({"roomShare": {"room": "!dev:s", "answer": true, "review": true, "fix": true, "merge": true}})).is_err());
        let change: Change = serde_json::from_value(json!({"forget": {"tool": "claude.Bash", "directory": "/w", "request": "{\"command\":\"cargo test\"}"}})).unwrap();
        assert_eq!(change.forget.unwrap().request, r#"{"command":"cargo test"}"#);
        // A rule is never created here: only the owner's card decision does that.
        assert!(serde_json::from_value::<Change>(json!({"remember": {"tool": "claude.Bash"}})).is_err());
        assert!(serde_json::from_value::<Change>(json!({"forget": {"tool": "claude.Bash", "prefix": "git"}})).is_err());
    }

    #[test]
    fn a_rule_is_named_by_its_exact_command_or_its_target() {
        let rule = |tool: &str, request: &str| RememberedRequest { tool: tool.into(), directory: "/w".into(), request: request.into(), created: 0, room: String::new() };
        assert_eq!(label(&rule("claude.Bash", r#"{"command":"cargo test"}"#)), "cargo test");
        assert_eq!(label(&rule("claude.Edit", r#"{"file_path":"/w/a.rs","new_string":"b"}"#)), "Edit /w/a.rs");
        assert_eq!(label(&rule("codex.command", r#"{"command":["bash","-lc","cargo test"],"cwd":"/w"}"#)), "bash -lc cargo test");
        assert_eq!(label(&rule("claude.WebSearch", r#"{"query":"x"}"#)), "WebSearch");
    }
}
