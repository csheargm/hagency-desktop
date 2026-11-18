//! One reading of the requests waiting for this owner: the management list
//! (the server's shared-execution approvals) and this app's own Agents'
//! queues (`local_approvals`), each with its own time bound, read at once.
//!
//! Why (Chris's test, Nov 17): the server's agent service stalled for about
//! 30 s on a dead database connection, and the whole reading hit its single
//! 20 s limit ("approvals_read_timeout"). Both halves waited on that server:
//! even this device's own queue began with the owner API's Agent list. A
//! request this device's own runtime was waiting on would have been hidden.
//! Now a slow server costs only the management rows; the own queue falls back
//! to the last Agent list that answered.
use ::hagency::native_owner::{Command, ManagementCommand};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// The server's part. Both halves are shown together, so this also bounds
/// how late this device's own cards can show; a healthy server answers in
/// well under a second, and the next poll follows within 10 s (GPT review).
const MANAGEMENT_READ: Duration = Duration::from_secs(8);
/// This app's own queues are read in-process: the Agent list (`AGENT_LIST_WAIT`)
/// plus each Agent's queue (`QUEUE_WAIT`), at most this long in all.
const LOCAL_READ: Duration = Duration::from_secs(10);
/// One Agent's queue: a slow one is named unreadable, the others still show.
pub(crate) const QUEUE_WAIT: Duration = Duration::from_secs(2);
/// Named in `unreadableAgents` when the queues were read from the last
/// Agent list because the owner API did not answer: an Agent started since
/// may be missing, and the inbox says so (GPT review).
pub(crate) const STALE_AGENT_LIST: &str = "agent_list_unavailable";
/// How long the owner API's Agent list may take before the last one is used.
const AGENT_LIST_WAIT: Duration = Duration::from_secs(5);

/// This device's active Agents (id, display name) from the last listing that
/// answered, per signed-in identity.
static LISTED: LazyLock<Mutex<(String, Vec<(String, String)>)>> = LazyLock::new(Default::default);

/// Every request this account may read (in `room`, if given), management
/// rows first, then this app's own Agents' rows.
pub async fn read_all(service: &crate::hagency::backend::Service, room: Option<&str>) -> Result<Value, String> {
    // Boxed: both halves inline made every spawning caller's future (and
    // the frame that moves it onto the runtime) overflow a test thread's
    // stack (matrix_links native_root_delivery test, Nov 17).
    let management = Box::pin(service.shared_permissions(ManagementCommand::SharedApprovals { room_id: room.map(str::to_owned) }));
    let local = Box::pin(super::local_approvals::read(service));
    Box::pin(bounded(management, local, room, MANAGEMENT_READ, LOCAL_READ)).await
}

/// Reads both halves at once, each within its own limit.
async fn bounded(
    management: impl Future<Output = Result<Value, String>>,
    local: impl Future<Output = Result<(Vec<Value>, Vec<Value>), String>>,
    room: Option<&str>,
    management_limit: Duration,
    local_limit: Duration,
) -> Result<Value, String> {
    let timeout = super::pending_approvals::READ_TIMEOUT;
    let (answer, local) = tokio::join!(
        async { tokio::time::timeout(management_limit, management).await.unwrap_or_else(|_| Err(timeout.into())) },
        async { tokio::time::timeout(local_limit, local).await.unwrap_or_else(|_| Err(timeout.into())) },
    );
    merge(room, answer, local)
}

/// Append this app's own Agents' waiting requests (in `room`, if given) to a
/// management answer. A failed listing leaves the answer as it was: the
/// management rows must still show, and the next poll reads the queue again.
fn merge(room: Option<&str>, answer: Result<Value, String>, local: Result<(Vec<Value>, Vec<Value>), String>) -> Result<Value, String> {
    use super::local_approvals::{MANAGEMENT_ERROR, UNREADABLE};
    let local_read = local.is_ok();
    let (rows, unreadable) = match local {
        Ok(read) => read,
        Err(code) => {
            // The inbox says so too (`UNREADABLE`); the poll repeats within seconds.
            makepad_widgets::log!("Local Agent approvals unreadable: {code}");
            (Vec::new(), vec![json!({"agent": "", "agentId": "", "code": code})])
        }
    };
    let rows: Vec<Value> = rows.into_iter().filter(|r| room.is_none_or(|room| r["approval"]["roomId"] == room)).collect();
    let mut value = match answer {
        Ok(value) => value,
        // Neither half answered.
        Err(code) if !local_read => return Err(code),
        // Management is unreachable but this app's own queues were read (even
        // empty: that empty answer is news the store must reconcile).
        Err(code) => json!({"approvals": [], MANAGEMENT_ERROR: code}),
    };
    if let Some(list) = value["approvals"].as_array_mut() {
        list.extend(rows);
    } else {
        value["approvals"] = Value::Array(rows);
    }
    value[UNREADABLE] = Value::Array(unreadable);
    Ok(value)
}

/// This device's active Agents: the owner API's list, or, while it is slow or
/// failing, the last list it gave this identity (`true`: the fallback).
pub(crate) async fn own_agents(service: &crate::hagency::backend::Service) -> Result<(Vec<(String, String)>, bool), String> {
    let listed = tokio::time::timeout(AGENT_LIST_WAIT, service.execute(Command::Agents)).await;
    let listed = listed.unwrap_or_else(|_| Err(super::pending_approvals::READ_TIMEOUT.into()));
    remembered(&service.identity(), listed.map(|value| active_agents(&value)))
}

/// Keep a listing that answered; fall back to the kept one when it did not.
fn remembered(identity: &str, listed: Result<Vec<(String, String)>, String>) -> Result<(Vec<(String, String)>, bool), String> {
    let mut kept = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    match listed {
        Ok(agents) => {
            *kept = (identity.to_owned(), agents.clone());
            Ok((agents, false))
        }
        Err(code) if kept.0 == identity && !kept.1.is_empty() => {
            makepad_widgets::log!("Agent list unavailable ({code}); reading the last known Agents' approvals");
            Ok((kept.1.clone(), true))
        }
        Err(code) => Err(code),
    }
}

/// The active Agents of an owner API listing, at most `MAX_AGENTS`.
fn active_agents(listed: &Value) -> Vec<(String, String)> {
    listed["agents"].as_array().into_iter().flatten()
        .filter(|agent| agent["state"] == "active")
        .filter_map(|agent| {
            let id = agent["id"].as_str()?;
            let name = agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent");
            Some((id.to_owned(), name.to_owned()))
        })
        .take(super::local_approvals::MAX_AGENTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::local_approvals::MANAGEMENT_ERROR;

    fn row(room: &str) -> Value {
        json!({"approval": {"id": "local:ag:p1", "roomId": room}})
    }

    /// The Nov 17 stall: the server never answers, but this device's own
    /// Agent is waiting; its card still shows, and the failure is named.
    #[tokio::test]
    async fn a_stalled_server_never_hides_the_own_agents_request() {
        let started = std::time::Instant::now();
        let never = std::future::pending::<Result<Value, String>>();
        let local = async { Ok((vec![row("!r:x")], Vec::new())) };
        let short = Duration::from_millis(50);
        let value = bounded(never, local, None, short, Duration::from_secs(5)).await.unwrap();
        assert_eq!(value["approvals"].as_array().unwrap().len(), 1);
        assert_eq!(value[MANAGEMENT_ERROR], super::super::pending_approvals::READ_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(2), "bounded by the management limit, not more");
    }

    #[tokio::test]
    async fn both_halves_stalled_is_a_named_failure_and_a_room_reading_keeps_its_room() {
        let never = std::future::pending::<Result<Value, String>>();
        let stuck = std::future::pending::<Result<(Vec<Value>, Vec<Value>), String>>();
        let short = Duration::from_millis(20);
        assert_eq!(bounded(never, stuck, None, short, short).await.unwrap_err(), super::super::pending_approvals::READ_TIMEOUT);
        // Management down, own queues read and empty: still an answer to reconcile.
        let never = std::future::pending::<Result<Value, String>>();
        let empty = async { Ok((Vec::new(), Vec::new())) };
        let value = bounded(never, empty, None, short, short).await.unwrap();
        assert!(value["approvals"].as_array().unwrap().is_empty() && value[MANAGEMENT_ERROR].is_string());
        let answer = async { Ok(json!({"approvals": [{"approval": {"id": "sap"}}]})) };
        let local = async { Ok((vec![row("!r:x"), row("!other:x")], Vec::new())) };
        let value = bounded(answer, local, Some("!r:x"), short, short).await.unwrap();
        assert_eq!(value["approvals"].as_array().unwrap().len(), 2, "management row + this room's own row");
    }

    #[test]
    fn the_last_agent_list_stands_in_while_the_owner_api_is_down() {
        let listed = json!({"agents": [{"id": "a1", "state": "active", "displayName": "bird-claude"},
            {"id": "a2", "state": "stopped"}, {"id": "a3", "state": "active"}]});
        let agents = active_agents(&listed);
        assert_eq!(agents, vec![("a1".into(), "bird-claude".into()), ("a3".into(), "Agent".into())]);
        assert_eq!(remembered("me", Ok(agents.clone())).unwrap(), (agents.clone(), false));
        assert_eq!(remembered("me", Err("approvals_read_timeout".into())).unwrap(), (agents, true), "named as the fallback");
        assert!(remembered("someone else", Err("down".into())).is_err(), "never another identity's list");
    }
}
