//! Recent requests for one owned Agent (the panel's Activity tab), read from
//! this device's profile ledger. Read-only; same owner check and ledger
//! opening as remembered approvals.
use super::*;

/// How many recent requests the Activity tab shows.
const RECENT_LIMIT: usize = 20;

/// GET returns `{agentId, requests: [Activity…]}`, newest first.
pub(super) async fn handle(req: &mut Request, depot: &Depot, agent: String) -> Result<Value, OwnerError> {
    if req.uri().query().is_some() {
        return Err(error(400, "invalid_arguments"));
    }
    if req.method() != Method::GET {
        return Err(error(405, "unsupported_operation"));
    }
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
    tokio::task::spawn_blocking(move || read(&root, &reply, &agent))
        .await
        .map_err(|_| error(503, "local_state_unavailable"))?
}

fn read(root: &Path, reply: &OwnerReply, agent: &str) -> Result<Value, OwnerError> {
    let path = ledger_path(root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(503, "local_state_unavailable"))?;
    let identity = profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
        .map_err(|_| error(403, "owner_scope_required"))?;
    let ledger = Ledger::open_scoped(&path, &reply.owner, &identity).map_err(local_error)?;
    let requests = ledger.recent_activity(&reply.owner, agent, RECENT_LIMIT).map_err(local_error)?;
    Ok(json!({"agentId": agent, "requests": requests}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hagency_agent_local::inbox::{Dispatch, Limits};

    const ORIGIN: &str = "https://server.example/";
    const ISSUER: &str = "https://server.example/auth/";
    const OWNER: &str = "@alice:example";

    fn reply(subject: &str) -> OwnerReply {
        OwnerReply { owner: OWNER.into(), origin: ORIGIN.into(), issuer: ISSUER.into(), subject: subject.into(), value: Value::Null }
    }
    fn dispatch() -> Dispatch {
        Dispatch {
            id: "dispatch-a".into(), binding_id: "binding".into(), agent_id: "agent".into(),
            event_id: "$event:example".into(), room_id: "!room:example".into(), requester_mxid: "@bob:example".into(),
            thread_root: "$root:example".into(), body: "private question".into(), state: "offered".into(),
            binding_generation: 1, dispatch_epoch: Some(1), dispatch_device_id: Some("device".into()),
            execution_id: None, outcome: None,
        }
    }

    #[test]
    fn activity_route_reads_this_profile_ledger_without_request_text() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().join("state");
        let path = ledger_path(&root, ORIGIN, ISSUER, "subject-a", OWNER).unwrap();
        let identity = profile_identity(ORIGIN, ISSUER, "subject-a", OWNER).unwrap();
        let mut ledger = Ledger::open_scoped(&path, OWNER, &identity).unwrap();
        ledger.register_binding(OWNER, &dispatch().scope()).unwrap();
        ledger.receive_dispatch(OWNER, &dispatch(), Limits::default(), 1_700_000_000).unwrap();
        drop(ledger);
        let value = read(&root, &reply("subject-a"), "agent").unwrap();
        assert_eq!(value["agentId"], "agent");
        let request = &value["requests"][0];
        assert_eq!((request["dispatchId"].as_str(), request["outcome"].as_str()), (Some("dispatch-a"), Some("running")));
        assert_eq!((request["requester"].as_str(), request["receivedAt"].as_i64()), (Some("@bob:example"), Some(1_700_000_000)));
        assert!(!value.to_string().contains("private question"));
        assert_eq!(read(&root, &reply("subject-a"), "other").unwrap()["requests"], json!([]));
    }
}
