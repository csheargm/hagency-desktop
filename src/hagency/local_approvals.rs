//! Tool requests of an Agent running in this app (Claude Code, Codex, Octos on
//! this device) that ask its owner. They wait in the in-process runtime's queue
//! (`owned-agents/{id}/runtime/approvals`), not in the management server's
//! shared-execution list, so until now only the Agent's Service page showed
//! them, and only while it was open: an owner in the room saw no card and the
//! request timed out. This module reads that queue and shapes each request as
//! a `pending_approvals` row, so the room's permission notice and the app-wide
//! poll show it with the same Approve / Deny buttons; a decision goes back to
//! the waiting Agent through `runtime/approvals/{id}/decision`.
use ::hagency::native_owner::{Command, LocalAction};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

/// Marks a row that came from this app's own Agent runtime.
pub(crate) const SOURCE: &str = "localRuntime";
/// Added to an answer: this app's Agents whose waiting requests could not be
/// read (`[{agent, code}]`), so the inbox says requests may be missing.
pub(crate) const UNREADABLE: &str = "unreadableAgents";
/// Added to an answer whose management part failed while this app's own
/// Agents were read: the failure code, so it is not mistaken for "none".
pub(crate) const MANAGEMENT_ERROR: &str = "managementError";
/// The decision that approves and remembers exactly this request.
pub const ALWAYS: &str = "approve_always";
/// At most this many Agents are read per poll (the listing is bounded too).
pub(crate) const MAX_AGENTS: usize = 128;

/// A runtime `ApprovalView` (`proposalId`, `argsDigest`, `proposal`) as a
/// `pending_approvals` row. None when it lacks what a card needs.
pub fn to_row(view: &Value, agent_name: &str) -> Option<Value> {
    let p = &view["proposal"];
    let scope = &p["scope"];
    let (agent, room) = (scope["agent"].as_str()?, scope["room"].as_str()?);
    let (proposal, digest) = (view["proposalId"].as_str()?, view["argsDigest"].as_str()?);
    let tool = p["tool"].as_str()?;
    let (method, tool) = match tool.split_once('.') {
        Some(("claude", t)) => ("claude/can_use_tool", t),
        Some(("octos", t)) => ("octos/can_use_tool", t),
        _ => ("claude/can_use_tool", tool),
    };
    Some(json!({
        "approval": {
            "id": format!("local:{agent}:{proposal}"),
            "agentName": agent_name, "roomId": room, "eventId": "", "threadRoot": scope["thread"],
            "request": {"method": method, "params": {"toolName": tool, "input": p["arguments"],
                "cwd": p["canonical_directory"]}, "sessionAllowed": false},
            "expiresAtMs": p["expires"].as_u64()?.saturating_mul(1000),
        },
        "digest": digest, "decision": null, "canDecide": true,
        // "Always allow this": only an exact command or Claude tool call.
        "alwaysAllowed": ::hagency::console::can_remember(p),
        "networkStep": ::hagency::console::is_network_step(p),
        SOURCE: {"agent": agent, "proposalId": proposal},
    }))
}

/// The runtime decision for a row built by `to_row`, or None for any other row.
pub fn decision_command(row: &Value, decision: &str) -> Option<Command> {
    let source = &row[SOURCE];
    let agent = source["agent"].as_str()?;
    let proposal = source["proposalId"].as_str()?;
    let digest = row["digest"].as_str()?;
    Some(Command::Local {
        agent_id: agent.into(),
        action: LocalAction::Decision { approval_id: proposal.into() },
        input: Some(json!({"argsDigest": digest, "approved": decision != "deny", "remember": decision == ALWAYS})),
        binding_id: None,
        requester: None,
    })
}

/// Posted when this app's own Agent runtime queued, decided or dropped a
/// request, so cards refresh within a second instead of at the next poll.
#[derive(Debug, Clone, Copy)]
pub struct LocalApprovalsChanged;

static WATCHING: AtomicBool = AtomicBool::new(false);
/// Clears `WATCHING` however the task ends (signal gone, or the account
/// session's tasks aborted on sign-out), so the next tick starts it again.
struct Watching;
impl Drop for Watching {
    fn drop(&mut self) {
        WATCHING.store(false, Ordering::SeqCst);
    }
}

/// Forward the in-process runtime's change signal to the UI thread. Called on
/// every approvals tick; starts the forwarder when none is running.
pub fn ensure_watching() {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut changed = ::hagency::console::approvals_changed();
    crate::sliding_sync::spawn_async_task(async move {
        let _watching = Watching;
        // `Err` means the process-wide sender is gone (shutdown): stop quietly.
        while changed.changed().await.is_ok() {
            makepad_widgets::Cx::post_action(LocalApprovalsChanged);
        }
    });
}

/// The waiting requests of this app's Agents, and the Agents whose queue
/// could not be read (`{agent, code}`), which are logged, never skipped silently.
pub(crate) async fn read(service: &crate::hagency::backend::Service) -> Result<(Vec<Value>, Vec<Value>), String> {
    let (mut rows, mut unreadable) = (Vec::new(), Vec::new());
    for (id, name) in super::approvals_read::own_agents(service).await? {
        let local = Command::Local { agent_id: id.clone(), action: LocalAction::Approvals, input: None, binding_id: None, requester: None };
        // An Agent that does not run on this device answers an empty queue.
        let answer = match service.execute(local).await {
            Ok(answer) => answer,
            Err(code) => {
                makepad_widgets::log!("Approvals of Agent {id} unreadable: {code}");
                unreadable.push(json!({"agent": name, "code": code}));
                continue;
            }
        };
        rows.extend(answer["approvals"].as_array().into_iter().flatten().filter_map(|v| to_row(v, &name)));
    }
    Ok((rows, unreadable))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real canonical folder: "Always allow" needs one (no symlinks).
    fn folder() -> String {
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap().display().to_string()
    }

    fn view() -> Value {
        json!({"proposalId":"p1","argsDigest":"a".repeat(64),"proposal":{
            "scope":{"agent":"ag","binding":"b","room":"!r:x","requester":"@o:x","thread":""},
            "dispatch":"d","tool":"claude.Bash","arguments":{"command":"gh issue view 6"},
            "canonical_directory":folder(),"risk":"high","policy_revision":[1,0,0],"expires":2_000_000_000u64}})
    }

    /// The owner's flow: the Agent asks for `gh issue view`; the room card
    /// says so and offers Approve / Deny, and the click reaches the runtime.
    #[test]
    fn a_local_agent_request_becomes_a_room_card_and_its_decision_reaches_the_runtime() {
        let row = to_row(&view(), "demoup-claude").unwrap();
        assert_eq!(row["approval"]["roomId"], "!r:x");
        assert_eq!(row["canDecide"], true);
        let (title, summary) = crate::hagency::pending_approvals::describe(&row);
        assert!(title.contains("demoup-claude wants to use Bash"), "{title}");
        assert!(summary.contains("gh issue view 6"), "{summary}");
        let Some(Command::Local { agent_id, action: LocalAction::Decision { approval_id }, input, .. }) = decision_command(&row, "approve_once") else { panic!("not a runtime decision") };
        assert_eq!((agent_id.as_str(), approval_id.as_str()), ("ag", "p1"));
        assert_eq!(input, Some(json!({"argsDigest": "a".repeat(64), "approved": true, "remember": false})));
        assert_eq!(row["alwaysAllowed"], true, "an exact shell command can be remembered");
        let Some(Command::Local { input, .. }) = decision_command(&row, ALWAYS) else { panic!() };
        assert_eq!(input, Some(json!({"argsDigest": "a".repeat(64), "approved": true, "remember": true})));
        let Some(Command::Local { input, .. }) = decision_command(&row, "deny") else { panic!() };
        assert_eq!(input.unwrap()["approved"], false);
        // A management row is not routed to the runtime.
        assert!(decision_command(&json!({"approval":{"id":"sap"},"digest":"d"}), "deny").is_none());
    }

    /// A Codex permission expansion is approved once; the card offers no
    /// "Always allow this" for it.
    #[test]
    fn an_expansion_offers_no_always_allow() {
        let mut v = view();
        v["proposal"]["tool"] = json!("codex.permissions");
        v["proposal"]["risk"] = json!("permission_expansion");
        assert_eq!(to_row(&v, "x").unwrap()["alwaysAllowed"], false);
    }

    /// A push is never remembered: no "Always allow this", and the card says why.
    #[test]
    fn a_push_offers_no_always_allow_and_says_why() {
        let mut v = view();
        v["proposal"]["arguments"]["command"] = json!("git push origin fix-6");
        let row = to_row(&v, "x").unwrap();
        assert_eq!((row["alwaysAllowed"].clone(), row["networkStep"].clone()), (json!(false), json!(true)));
        assert_eq!(to_row(&view(), "x").unwrap()["networkStep"], false);
    }

    #[test]
    fn a_view_missing_its_room_or_digest_is_not_shown() {
        let mut v = view();
        v["proposal"]["scope"]["room"] = Value::Null;
        assert!(to_row(&v, "x").is_none());
        let mut v = view();
        v["argsDigest"] = Value::Null;
        assert!(to_row(&v, "x").is_none());
    }
}
