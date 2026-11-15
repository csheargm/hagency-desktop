//! App-wide awareness of pending shared-execution permission requests.
//!
//! The executor gives the owner about two minutes to answer a Codex permission
//! request. Until now the request was visible only inside the open room or the
//! owner console page, so an owner elsewhere in the app simply timed out. This
//! module polls the authenticated management list (never chat text) on a fixed
//! cadence, announces new actionable requests, and lets the rooms list, the
//! Agent tab and the message timeline show them in place.
//!
//! It also keeps this client's own decisions. Both cards that show a request
//! in a room (on the message that raised it, and the notice at the end of the
//! timeline) read them here, so a click on either one shows the same "sending"
//! and "approved" state on both and neither offers that request's buttons again.
//!
//! Nothing here grants authority: every decision still goes through
//! `hagency.shared_approvals.decide`, where the management server re-checks the
//! deciding account, the request digest and expiry.
//!
//! The owner's own local Agents ask on this computer instead. The daemon that
//! runs them lists their requests (never chat text) and this module reads that
//! list on the same cadence, as rows of the same shape marked `"source":
//! "local"`. Their decisions go to the Agent's own decision route, where the
//! daemon re-checks the owner, the exact request and its expiry. Nothing is
//! posted to the room: only the owner's app shows them.
// TODO(R4): over budget; split describe()/tool_target() wording out from the poll store.
use super::approval_text::{readable_command, relative_to_workspace};
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use ::hagency::native_owner::ManagementCommand;
use makepad_widgets::*;
use matrix_sdk::ruma::RoomId;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// Seconds between management polls. Requests expire after at most 120 s, so
/// this leaves the owner well over a minute even when nothing else refreshes.
pub const POLL_SECONDS: f64 = 10.0;

/// How long this client remembers a decision it made. Requests expire within
/// a few minutes, so this outlives every request a decision could apply to.
const KEEP_DECISIONS_MS: u64 = 10 * 60 * 1000;

/// A request the owner (or a delegated requester) may still act on.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingApproval {
    /// The management row: `approval`, `digest`, `decision`, `canDecide`,
    /// and `encryptedPending` once the Olm details have or have not arrived.
    pub row: Value,
    pub local: LocalDecision,
}
impl PendingApproval {
    pub fn id(&self) -> &str {
        self.row["approval"]["id"].as_str().unwrap_or_default()
    }
    pub fn digest(&self) -> &str {
        self.row["digest"].as_str().unwrap_or_default()
    }
    pub fn room_id(&self) -> &str {
        self.row["approval"]["roomId"].as_str().unwrap_or_default()
    }
    pub fn event_id(&self) -> &str {
        self.row["approval"]["eventId"].as_str().unwrap_or_default()
    }
    pub fn thread_root(&self) -> &str {
        self.row["approval"]["threadRoot"].as_str().unwrap_or_default()
    }
    pub fn agent_name(&self) -> &str {
        self.row["approval"]["agentName"].as_str().unwrap_or("Agent")
    }
    pub fn can_decide(&self) -> bool {
        self.row["canDecide"] == true && self.row["encryptedPending"] != true
    }
    pub fn session_allowed(&self) -> bool {
        self.row["approval"]["request"]["sessionAllowed"] == true
    }
    pub fn expires_at_ms(&self) -> Option<u64> {
        self.row["approval"]["expiresAtMs"].as_u64()
    }
    pub fn owner_mxid(&self) -> &str {
        self.row["approval"]["ownerMxid"].as_str().unwrap_or_default()
    }
    /// A request of the owner's own local Agent, decided on this computer.
    pub fn is_local(&self) -> bool {
        is_local(&self.row)
    }
    /// Whether the owner can answer it now.
    fn actionable(&self, now: u64) -> bool {
        self.can_decide() && self.local == LocalDecision::None && self.expires_at_ms().is_none_or(|t| t > now)
    }
}

/// The request `method` of a local Agent's row.
const LOCAL_METHOD: &str = "hagency/local_tool";
fn is_local(row: &Value) -> bool {
    row["source"] == "local"
}
/// The id of a local Agent's request in this store, never a management id.
pub fn local_id(proposal_id: &str) -> String {
    format!("local:{proposal_id}")
}

/// What this client has done with a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalDecision {
    None,
    Sending(String),
    Sent(String),
    Failed(String, String),
}

#[derive(Default)]
struct State {
    identity: String,
    rows: Vec<PendingApproval>,
    announced: HashSet<String>,
    /// This client's decisions by request id, with the time each was made.
    /// Kept apart from `rows`: a poll answer read before a click must not
    /// offer the buttons of a request that was already answered.
    decisions: HashMap<String, (LocalDecision, u64)>,
    /// The owner's local Agents' names by Agent id, read from the owner's
    /// Agent list once an unknown Agent asks. Empty for an Agent not listed.
    agent_names: HashMap<String, String>,
}
impl State {
    fn for_identity(&mut self, identity: &str) {
        if self.identity != identity {
            *self = State { identity: identity.to_owned(), ..State::default() };
        }
    }
    fn local(&self, id: &str) -> LocalDecision {
        self.decisions.get(id).map(|(decision, _)| decision.clone()).unwrap_or(LocalDecision::None)
    }
    fn pending(&self, row: Value) -> PendingApproval {
        let local = self.local(row["approval"]["id"].as_str().unwrap_or_default());
        PendingApproval { row, local }
    }
    fn record(&mut self, id: &str, local: LocalDecision, now: u64) {
        for p in self.rows.iter_mut().filter(|p| p.id() == id) {
            p.local = local.clone();
        }
        self.decisions.insert(id.to_owned(), (local, now));
    }
    fn forget_old_decisions(&mut self, now: u64) {
        self.decisions.retain(|_, (_, at)| now.saturating_sub(*at) < KEEP_DECISIONS_MS);
    }
    /// The actionable requests not announced yet, now marked announced.
    fn take_announcements(&mut self, now: u64) -> Vec<PendingApproval> {
        let announce: Vec<PendingApproval> = self
            .rows
            .iter()
            .filter(|p| p.actionable(now) && !self.announced.contains(p.id()))
            .cloned()
            .collect();
        for p in &announce {
            self.announced.insert(p.id().to_owned());
        }
        announce
    }
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

/// Says each request once: a pop-up, and a system notification where the
/// platform has them.
fn announce(cx: &mut Cx, requests: Vec<PendingApproval>) {
    for p in requests {
        let (title, summary) = describe(&p.row);
        enqueue_popup_notification(
            format!("{title}\n{summary}\n{}", crate::i18n::tr("Open the room or the Agent page to decide.")),
            PopupKind::Info,
            Some(20.0),
        );
        // A no-op on backends without system notifications; iOS shows it.
        crate::jit_compat::show_notification(cx, &title, &summary);
    }
}

/// Posted from the poll task with the raw management answer.
#[derive(Debug)]
pub struct PendingApprovalsUpdate {
    pub identity: String,
    pub result: Result<Value, String>,
}
/// Posted by the app after a poll changed the pending set, so open timelines
/// re-populate the messages that carry a request.
#[derive(Debug, Clone, Copy)]
pub struct PendingApprovalsChanged;
/// Posted from a decide task once the server answered.
#[derive(Debug)]
pub struct PendingApprovalDecided {
    pub id: String,
    pub decision: String,
    pub result: Result<(), String>,
}
/// Posted from the poll task with this computer's local requests, already
/// as rows (`None`: no owner runs here), and the Agent names it knows.
#[derive(Debug)]
pub struct LocalApprovalsUpdate {
    pub identity: String,
    pub result: Result<(Option<Vec<Value>>, HashMap<String, String>), String>,
}

/// One read of the local list at a time: a busy owner answers late, and the
/// next round asks again only after this one is answered.
static LOCAL_READING: AtomicBool = AtomicBool::new(false);
struct LocalReading;
impl Drop for LocalReading {
    fn drop(&mut self) {
        LOCAL_READING.store(false, Ordering::Release);
    }
}

/// Ask the management server for every request this account may read, and
/// the owner running here for its local Agents' requests. Silently does
/// nothing without a signed-in Hagency identity.
pub fn poll() {
    let Ok(service) = crate::hagency::backend::Service::for_current() else { return };
    let identity = service.identity();
    if !LOCAL_READING.swap(true, Ordering::AcqRel) {
        let reading = LocalReading;
        let names = STATE
            .lock()
            .ok()
            .filter(|s| s.identity == identity)
            .map(|s| s.agent_names.clone())
            .unwrap_or_default();
        let (service, identity) = (service.clone(), identity.clone());
        crate::sliding_sync::spawn_async_task(async move {
            let result = read_local(&service, names).await;
            drop(reading);
            Cx::post_action(LocalApprovalsUpdate { identity, result });
        });
    }
    crate::sliding_sync::spawn_async_task(async move {
        let result = service
            .shared_permissions(ManagementCommand::SharedApprovals { room_id: None })
            .await;
        Cx::post_action(PendingApprovalsUpdate { identity, result });
    });
}

/// The local requests as rows, with each Agent's name. An Agent not seen
/// before is looked up in the owner's Agent list, once.
async fn read_local(
    service: &crate::hagency::backend::Service,
    mut names: HashMap<String, String>,
) -> Result<(Option<Vec<Value>>, HashMap<String, String>), String> {
    let Some(value) = service.local_approvals().await? else { return Ok((None, names)) };
    let views = value["approvals"].as_array().cloned().unwrap_or_default();
    let unknown: Vec<&str> = views
        .iter()
        .filter_map(|v| v["proposal"]["scope"]["agent"].as_str())
        .filter(|agent| !names.contains_key(*agent))
        .collect();
    if !unknown.is_empty()
        && let Ok(roster) = service.execute(crate::hagency::model::Command::Agents).await
    {
        for agent in roster["agents"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (agent["id"].as_str(), agent["displayName"].as_str()) {
                names.insert(id.to_owned(), name.to_owned());
            }
        }
        for agent in unknown {
            names.entry(agent.to_owned()).or_default();
        }
    }
    let owner = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    Ok((Some(views.iter().filter_map(|v| local_row(v, &names, &owner)).collect()), names))
}

/// A local Agent's request in the shape the cards and lists read. Only the
/// owner reads this list, so it can be decided here; the daemon checks the
/// owner again when the decision arrives. Malformed entries are left out.
fn local_row(view: &Value, names: &HashMap<String, String>, owner: &str) -> Option<Value> {
    let p = &view["proposal"];
    let id = view["proposalId"].as_str().filter(|id| !id.is_empty())?;
    let digest = view["argsDigest"]
        .as_str()
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let agent = p["scope"]["agent"].as_str().filter(|a| !a.is_empty())?;
    let room = p["scope"]["room"].as_str().filter(|r| r.starts_with('!'))?;
    let expires = p["expires"].as_i64().filter(|t| *t > 0)?;
    let name = names.get(agent).map(String::as_str).filter(|n| !n.is_empty()).unwrap_or("Agent");
    // Event ids only: a main-timeline message may carry its room as its thread.
    let event = |key: &str| view[key].as_str().filter(|e| e.starts_with('$')).unwrap_or_default().to_owned();
    Some(json!({
        "source": "local",
        "digest": digest,
        "canDecide": true,
        "decision": null,
        "approval": {
            "id": local_id(id),
            "proposalId": id,
            "agentId": agent,
            "agentName": name,
            "roomId": room,
            "eventId": event("eventId"),
            "threadRoot": event("threadRoot"),
            "requesterMxid": p["scope"]["requester"],
            "ownerMxid": owner,
            "expiresAtMs": (expires as u64).saturating_mul(1000),
            "request": {
                "method": LOCAL_METHOD,
                "params": {"tool": p["tool"], "input": p["arguments"], "cwd": p["canonical_directory"], "risk": p["risk"]},
            },
        },
    }))
}

/// Record a read of the local requests. Returns true when the pending set
/// changed. New actionable requests are announced once.
pub fn apply_local(cx: &mut Cx, update: &LocalApprovalsUpdate) -> bool {
    let Ok((rows, names)) = &update.result else { return false };
    let now = now_ms();
    let (changed, new) = {
        let Ok(mut state) = STATE.lock() else { return false };
        state.for_identity(&update.identity);
        state.forget_old_decisions(now);
        state.agent_names = names.clone();
        let before = shape(state.rows.iter().filter(|p| p.is_local()));
        let fresh: Vec<PendingApproval> = rows
            .iter()
            .flatten()
            .filter(|row| row["approval"]["expiresAtMs"].as_u64().is_some_and(|t| t > now))
            .map(|row| state.pending(row.clone()))
            .collect();
        let after = shape(fresh.iter());
        state.rows.retain(|p| !p.is_local());
        state.rows.extend(fresh);
        (before != after, state.take_announcements(now))
    };
    announce(cx, new);
    changed
}

/// Whether the request `id` is new to this client's reminders, now marked
/// announced. The Agent page asks before its own notice, so one request is
/// never announced twice.
pub fn first_notice(identity: &str, id: &str) -> bool {
    let Ok(mut state) = STATE.lock() else { return true };
    // Another account's record is not this page's to replace.
    if !state.identity.is_empty() && state.identity != identity {
        return true;
    }
    state.for_identity(identity);
    state.announced.insert(id.to_owned())
}

/// The requests in a management answer that still wait for a decision.
fn open_rows(value: &Value, now: u64) -> Vec<Value> {
    value["approvals"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["decision"].is_null())
                .filter(|row| row["approval"]["expiresAtMs"].as_u64().is_none_or(|t| t > now))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// What the cards show of a set of requests: which ones, and whether each can
/// be decided yet (the encrypted details may still be on their way).
fn shape<'a>(rows: impl Iterator<Item = &'a PendingApproval>) -> Vec<(String, bool)> {
    rows.map(|p| (p.id().to_owned(), p.can_decide())).collect()
}

/// Record a poll answer. Returns true when the pending set changed, so callers
/// can redraw lists and timelines. New actionable requests are announced once.
pub fn apply(cx: &mut Cx, update: &PendingApprovalsUpdate) -> bool {
    let Ok(value) = &update.result else { return false };
    let now = now_ms();
    let fresh = open_rows(value, now);
    let (changed, new) = {
        let Ok(mut state) = STATE.lock() else { return false };
        state.for_identity(&update.identity);
        state.forget_old_decisions(now);
        let before = shape(state.rows.iter().filter(|p| !p.is_local()));
        let rows: Vec<PendingApproval> = fresh.into_iter().map(|row| state.pending(row)).collect();
        let after = shape(rows.iter());
        // The local Agents' requests come from their own reads.
        state.rows.retain(PendingApproval::is_local);
        state.rows.extend(rows);
        (before != after, state.take_announcements(now))
    };
    announce(cx, new);
    changed
}

/// Record the open room's own reading (its permission notice reads every two
/// seconds), so the cards on its messages are as current as the notice.
/// Requests of a room on screen are not announced again later. Returns true
/// when the room's pending set changed.
pub fn merge_room(identity: &str, room: &str, value: &Value, on_screen: bool) -> bool {
    let now = now_ms();
    let fresh: Vec<Value> = open_rows(value, now).into_iter().filter(|row| row["approval"]["roomId"] == room).collect();
    let Ok(mut state) = STATE.lock() else { return false };
    state.for_identity(identity);
    state.forget_old_decisions(now);
    let before = shape(state.rows.iter().filter(|p| p.room_id() == room && !p.is_local()));
    let fresh: Vec<PendingApproval> = fresh.into_iter().map(|row| state.pending(row)).collect();
    let after = shape(fresh.iter());
    if on_screen {
        for p in &fresh {
            state.announced.insert(p.id().to_owned());
        }
    }
    state.rows.retain(|p| p.room_id() != room || p.is_local());
    state.rows.extend(fresh);
    before != after
}

/// Record the outcome of a decision this client sent.
pub fn apply_decided(update: &PendingApprovalDecided) {
    if let Ok(mut state) = STATE.lock() {
        let local = match &update.result {
            Ok(()) => LocalDecision::Sent(update.decision.clone()),
            Err(error) => LocalDecision::Failed(update.decision.clone(), error.clone()),
        };
        state.record(&update.id, local, now_ms());
    }
    if let Err(error) = &update.result {
        enqueue_popup_notification(
            format!("{} {}", crate::i18n::tr("Could not send the decision:"), decide_error(error)),
            PopupKind::Error,
            Some(8.0),
        );
    }
}

/// Send `decision` for the management `row`. Every card showing the request
/// says "sending" until the server answers, then that it was decided; the
/// next poll drops the row once the executor used it. Returns false, and
/// sends nothing, when this client cannot decide the request now.
pub fn decide(row: &Value, decision: &str) -> bool {
    let id = row["approval"]["id"].as_str().unwrap_or_default().to_owned();
    let digest = row["digest"].as_str().unwrap_or_default().to_owned();
    if id.is_empty() || digest.is_empty() || row["canDecide"] != true || row["encryptedPending"] == true || !row["decision"].is_null() {
        return false;
    }
    // A local Agent's request is approved once or refused, nothing wider.
    let local = is_local(row).then(|| {
        (row["approval"]["agentId"].as_str().unwrap_or_default().to_owned(),
         row["approval"]["proposalId"].as_str().unwrap_or_default().to_owned())
    });
    if local.as_ref().is_some_and(|(agent, proposal)| agent.is_empty() || proposal.is_empty() || !matches!(decision, "approve_once" | "deny")) {
        return false;
    }
    {
        let Ok(mut state) = STATE.lock() else { return false };
        if !matches!(state.local(&id), LocalDecision::None | LocalDecision::Failed(..)) {
            return false;
        }
        if !state.rows.iter().any(|p| p.id() == id) {
            state.rows.push(PendingApproval { row: row.clone(), local: LocalDecision::None });
        }
        state.record(&id, LocalDecision::Sending(decision.to_owned()), now_ms());
    }
    let decision = decision.to_owned();
    let service = crate::hagency::backend::Service::for_current();
    crate::sliding_sync::spawn_async_task(async move {
        use crate::hagency::model::{Command, LocalAction};
        let result = match (service, local) {
            // The same route as the Agent page: the daemon checks the owner,
            // the exact request and its expiry.
            (Ok(service), Some((agent_id, approval_id))) => service
                .execute(Command::Local {
                    agent_id,
                    action: LocalAction::Decision { approval_id },
                    input: Some(json!({"argsDigest": digest, "approved": decision == "approve_once"})),
                    binding_id: None,
                    requester: None,
                })
                .await
                .map(|_| ()),
            (Ok(service), None) => service
                .shared_permissions(ManagementCommand::SharedApprovalDecide {
                    id: id.clone(),
                    digest,
                    decision: decision.clone(),
                })
                .await
                .map(|_| ()),
            (Err(error), _) => Err(error),
        };
        Cx::post_action(PendingApprovalDecided { id, decision, result });
    });
    true
}

/// What this client did with the request `id`.
pub fn local_decision(id: &str) -> LocalDecision {
    STATE.lock().map_or(LocalDecision::None, |s| s.local(id))
}

/// The button text (a catalog key) of a decision.
pub fn decision_label(decision: &str) -> &'static str {
    match decision {
        "approve_session" => "Always approve for this session",
        "deny" => "Deny",
        _ => "Approve once",
    }
}
/// A decision in the past tense (a catalog key), for "previous request" lines.
pub fn decided_label(decision: &str) -> &'static str {
    match decision {
        "approve_session" => "Approved for this session",
        "deny" => "Denied",
        _ => "Approved once",
    }
}
/// What a card says (a catalog key) once the server recorded the decision.
pub fn sent_status(decision: &str) -> &'static str {
    match decision {
        "approve_session" => "Approved for this session. Waiting for the Agent to continue.",
        "deny" => "Denied. The Agent will not run this call.",
        _ => "Approved once. Waiting for the Agent to continue.",
    }
}

/// Actionable requests in `room`, for list badges.
pub fn count_for_room(room: &RoomId) -> u64 {
    let now = now_ms();
    STATE.lock().map_or(0, |s| {
        s.rows.iter().filter(|p| p.room_id() == room.as_str() && p.actionable(now)).count() as u64
    })
}
/// Actionable requests anywhere, for the Agent tab.
pub fn total() -> u64 {
    let now = now_ms();
    STATE.lock().map_or(0, |s| s.rows.iter().filter(|p| p.actionable(now)).count() as u64)
}
/// The request raised by the message `event_id` in `room`, if any. In the
/// room timeline a thread's root message also carries its thread's requests,
/// because the triggering reply is not shown there.
pub fn for_event(room: &RoomId, event_id: &str, include_thread_root: bool) -> Option<PendingApproval> {
    let state = STATE.lock().ok()?;
    state
        .rows
        .iter()
        .filter(|p| p.room_id() == room.as_str())
        .find(|p| p.event_id() == event_id || (include_thread_root && p.thread_root() == event_id && p.thread_root() != p.event_id()))
        .cloned()
}
/// Every pending request in `room`, newest first.
pub fn for_room(room: &RoomId) -> Vec<PendingApproval> {
    STATE.lock().map_or(Vec::new(), |s| s.rows.iter().filter(|p| p.room_id() == room.as_str()).cloned().collect())
}
pub fn forget_identity() {
    if let Ok(mut state) = STATE.lock() {
        *state = State::default();
    }
}

/// A title and a short summary of what the Agent asked for: the tool and the
/// command, file or address it would act on, in the UI language.
pub fn describe(row: &Value) -> (String, String) {
    use crate::i18n::{format, tr};
    let a = &row["approval"];
    let agent = || ("agent", a["agentName"].as_str().unwrap_or("Agent").to_owned());
    if row["encryptedPending"] == true {
        return (
            format("{agent}'s request is on its way", &[agent()]),
            tr("Waiting for the encrypted details from the Agent's device.").to_owned(),
        );
    }
    let request = &a["request"];
    let params = &request["params"];
    let reason = params["reason"].as_str().or_else(|| request["reason"].as_str()).unwrap_or("");
    let mut lines = Vec::new();
    let title = match request["method"].as_str() {
        Some("item/commandExecution/requestApproval") => {
            let cwd = params["cwd"].as_str();
            lines.push(readable_command(&command_words(&params["command"]), cwd));
            if let Some(cwd) = cwd {
                lines.push(format("In {path}", &[("path", relative_to_workspace(cwd))]));
            }
            format("{agent} wants to run a command", &[agent()])
        }
        Some("item/fileChange/requestApproval") => {
            if let Some(root) = params["grantRoot"].as_str() {
                lines.push(relative_to_workspace(root));
            }
            format("{agent} wants to change files", &[agent()])
        }
        Some(method @ ("claude/can_use_tool" | "octos/can_use_tool")) => {
            let tool = params["toolName"].as_str().unwrap_or("tool");
            lines.push(tool_target(method, tool, &params["input"], params["cwd"].as_str()));
            format("{agent} wants to use {tool}", &[agent(), ("tool", tool.to_owned())])
        }
        Some("item/permissions/requestApproval") => {
            let p = &params["permissions"];
            if p.get("network").is_some() {
                lines.push(tr("Network access").to_owned());
            }
            if p.get("fileSystem").is_some() {
                lines.push(format("File system: {value}", &[("value", p["fileSystem"].to_string())]));
            }
            format("{agent} asks for permissions", &[agent()])
        }
        Some(LOCAL_METHOD) => {
            let input = &params["input"];
            // The owner's rule asks before each request reaches the model.
            let title = if params["risk"] == "model_request" {
                if let Some(preview) = input["inputPreview"].as_str() {
                    lines.push(format!("{}: {}", tr("Request preview"), short_text(preview)));
                }
                format("{agent} asks for a permission", &[agent()])
            } else {
                let tool = params["tool"].as_str().unwrap_or("tool");
                lines.push(tool_target(LOCAL_METHOD, tool, input));
                if let Some(cwd) = params["cwd"].as_str().filter(|cwd| !cwd.is_empty()) {
                    lines.push(format("In {path}", &[("path", cwd.to_owned())]));
                }
                format("{agent} wants to use {tool}", &[agent(), ("tool", tool.to_owned())])
            };
            // When someone else's message made the Agent ask, say who.
            if let Some(requester) = a["requesterMxid"].as_str().filter(|r| !r.is_empty() && a["ownerMxid"] != *r) {
                lines.push(format("Requested by {requester}", &[("requester", requester.to_owned())]));
            }
            title
        }
        _ => format("{agent} asks for a permission", &[agent()]),
    };
    if !reason.is_empty() {
        lines.push(format("Reason: {reason}", &[("reason", reason.to_owned())]));
    }
    (title, lines.join("\n"))
}
/// At most a few lines of a long text, cut at a character boundary.
fn short_text(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > 240 { format!("{}…", text.chars().take(240).collect::<String>()) } else { text.to_owned() }
}
/// What a Claude or Octos tool call would act on. Claude Code's tools name
/// their target in a few well-known input fields; anything else is shown as
/// compact JSON.
/// The full target is shown (commands one step per line, workspace paths as
/// `~workspace/…`); the card scrolls rather than cutting it.
fn tool_target(method: &str, tool: &str, input: &Value, cwd: Option<&str>) -> String {
    if method == "octos/can_use_tool" && tool == "shell" {
        let command = input["command"].as_str()
            .or_else(|| input["cmd"].as_str())
            .or_else(|| input["body"].as_str()).unwrap_or("");
        return readable_command(command, cwd);
    }
    if let Some(command) = input["command"].as_str().filter(|text| !text.trim().is_empty()) {
        return readable_command(command, cwd);
    }
    ["file_path", "notebook_path", "path", "pattern", "url", "query"]
        .iter()
        .find_map(|key| input[*key].as_str().filter(|text| !text.trim().is_empty()))
        .map(relative_to_workspace)
        .unwrap_or_else(|| input.to_string())
}
fn command_words(command: &Value) -> String {
    match command {
        Value::Array(parts) => parts.iter().map(|p| p.as_str().map(str::to_owned).unwrap_or_else(|| p.to_string())).collect::<Vec<_>>().join(" "),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
fn decide_error(code: &str) -> &'static str {
    match code {
        "approval_request_changed" | "permission_request_changed" => "the request changed; reopen it / 请求已变化，请重新打开",
        "approval_already_decided" => "someone already decided / 已有人决定",
        "approval_expired" => "the request expired / 请求已过期",
        "resource_owner_required" => "only the resource owner can approve this / 只有资源所有者能批准",
        "encrypted_approval_unavailable" => "the encrypted details have not arrived / 加密详情尚未到达",
        // A local Agent's request that was answered, expired or ended.
        "invalid_tool_approval" => "the request is no longer waiting / 该请求已不再等待",
        _ => "please retry / 请重试",
    }
}
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Sets this client's decision for `id` without sending anything.
#[cfg(test)]
pub(crate) fn record_for_test(id: &str, local: LocalDecision) {
    STATE.lock().unwrap().record(id, local, now_ms());
}
/// The store is one per process: tests that use it take turns.
#[cfg(test)]
pub(crate) static TEST_TURN: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, room: &str, event: &str, can: bool) -> Value {
        json!({"approval":{"id":id,"agentName":"小黄瓜","roomId":room,"eventId":event,"threadRoot":"$root",
            "request":{"method":"item/commandExecution/requestApproval","params":{"command":["git","push"],"cwd":"/w","reason":"deploy"},"sessionAllowed":true},
            "expiresAtMs": now_ms() + 100_000},
            "digest":"d","decision":null,"canDecide":can})
    }

    #[test]
    fn describe_puts_the_command_and_reason_in_words() {
        let (title, summary) = describe(&row("sap_1", "!r:x", "$e", true));
        assert!(title.starts_with("小黄瓜 wants to run a command"));
        assert!(summary.contains("git push") && summary.contains("In /w") && summary.contains("Reason: deploy"));
        let mut pending = row("sap_1", "!r:x", "$e", true);
        pending["encryptedPending"] = json!(true);
        assert!(describe(&pending).0.contains("on its way"));
        pending["encryptedPending"] = json!(false);
        // A long tool input is shown whole, never cut at a length.
        let long = "x".repeat(400);
        pending["approval"]["request"] = json!({"method":"claude/can_use_tool","params":{"toolName":"Mcp","input":{"data":long}}});
        assert!(describe(&pending).1.contains(&long));
        // The Agent's workspace path reads as ~workspace/….
        let ws = format!("/u/owner_{}/room-workspaces/binding_{}", "a".repeat(64), "b".repeat(64));
        pending["approval"]["request"] = json!({"method":"item/commandExecution/requestApproval","params":{"command":"git push","cwd":format!("{ws}/repo")}});
        let summary = describe(&pending).1;
        assert!(summary.contains("In ~workspace/repo") && !summary.contains("owner_"), "{summary}");
        pending["approval"]["request"] = json!({"method":"octos/can_use_tool",
            "params":{"toolName":"shell","input":{"cmd":"printf PTY_READY; read answer","tty":true}}});
        assert!(describe(&pending).1.contains("printf PTY_READY;\nread answer"));
        // Claude Code's tools: the command, file or pattern, not raw JSON.
        for (tool, input, shown) in [
            ("Bash", json!({"command":"ls -la","description":"List files"}), "ls -la"),
            ("Write", json!({"file_path":"/w/notes.md","content":"secret text"}), "/w/notes.md"),
            ("Glob", json!({"pattern":"**/*.rs"}), "**/*.rs"),
            ("Unknown", json!({"n":1}), "{\"n\":1}"),
            ("Bash", json!({"command":"cargo test && git push && gh pr create --fill"}), "cargo test &&\ngit push &&\ngh pr create --fill"),
        ] {
            pending["approval"]["request"] = json!({"method":"claude/can_use_tool","params":{"toolName":tool,"input":input}});
            let (title, summary) = describe(&pending);
            assert_eq!(title, format!("小黄瓜 wants to use {tool}"));
            assert_eq!(summary, shown);
        }
    }

    #[test]
    fn pending_rows_are_indexed_by_room_event_and_thread_root() {
        let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget_identity();
        let mut state = STATE.lock().unwrap();
        state.identity = "me".into();
        state.rows = vec![
            PendingApproval { row: row("sap_a", "!a:x", "$reply", true), local: LocalDecision::None },
            PendingApproval { row: row("sap_b", "!b:x", "$other", false), local: LocalDecision::None },
        ];
        drop(state);
        let a = RoomId::parse("!a:x").unwrap();
        let b = RoomId::parse("!b:x").unwrap();
        assert_eq!((count_for_room(&a), count_for_room(&b), total()), (1, 0, 1));
        assert_eq!(for_event(&a, "$reply", false).map(|p| p.id().to_owned()).as_deref(), Some("sap_a"));
        assert!(for_event(&a, "$root", false).is_none(), "a thread timeline only matches the triggering reply");
        assert_eq!(for_event(&a, "$root", true).map(|p| p.id().to_owned()).as_deref(), Some("sap_a"));
        assert!(for_event(&b, "$reply", true).is_none());
        apply_decided(&PendingApprovalDecided { id: "sap_a".into(), decision: "deny".into(), result: Ok(()) });
        assert_eq!((count_for_room(&a), total()), (0, 0), "a sent decision no longer counts as pending");
        assert_eq!(for_event(&a, "$reply", false).unwrap().local, LocalDecision::Sent("deny".into()));
        forget_identity();
    }

    #[test]
    fn a_decision_survives_an_older_poll_answer_and_the_open_room_keeps_cards_current() {
        let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget_identity();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let answer = |rows: Vec<Value>| PendingApprovalsUpdate { identity: "me".into(), result: Ok(json!({"approvals": rows})) };
        // The open room's notice read a new request before the app-wide poll did.
        let room_rows = json!({"approvals":[row("sap_new", "!a:x", "$reply", true), row("sap_elsewhere", "!b:x", "$x", true)]});
        assert!(merge_room("me", "!a:x", &room_rows, true));
        let a = RoomId::parse("!a:x").unwrap();
        assert_eq!(for_event(&a, "$reply", false).map(|p| p.id().to_owned()).as_deref(), Some("sap_new"));
        assert!(for_room(&RoomId::parse("!b:x").unwrap()).is_empty(), "a room's reading only replaces that room");
        // Someone answers it: every card shows "sending" at once.
        record_for_test("sap_new", LocalDecision::Sending("approve_once".into()));
        assert_eq!(for_event(&a, "$reply", false).unwrap().local, LocalDecision::Sending("approve_once".into()));
        assert_eq!(count_for_room(&a), 0);
        // An app-wide answer read before the click still lists it undecided:
        // the request must not become answerable again, nor be announced.
        assert!(!apply(&mut cx, &answer(vec![row("sap_new", "!a:x", "$reply", true)])));
        assert_eq!(for_event(&a, "$reply", false).unwrap().local, LocalDecision::Sending("approve_once".into()));
        assert_eq!(total(), 0);
        apply_decided(&PendingApprovalDecided { id: "sap_new".into(), decision: "approve_once".into(), result: Ok(()) });
        assert_eq!(local_decision("sap_new"), LocalDecision::Sent("approve_once".into()));
        // Once used, the request leaves the list; the next one is a new request.
        assert!(merge_room("me", "!a:x", &json!({"approvals":[row("sap_next", "!a:x", "$reply", true)]}), true));
        let next = for_event(&a, "$reply", false).unwrap();
        assert_eq!((next.id(), next.local.clone()), ("sap_next", LocalDecision::None));
        assert_eq!(local_decision("sap_new"), LocalDecision::Sent("approve_once".into()));
        // A failed send makes the request answerable again.
        apply_decided(&PendingApprovalDecided { id: "sap_next".into(), decision: "deny".into(), result: Err("approval_expired".into()) });
        assert!(matches!(local_decision("sap_next"), LocalDecision::Failed(..)));
        forget_identity();
    }

    #[test]
    fn decide_refuses_what_this_client_cannot_answer() {
        let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget_identity();
        let mut waiting = row("sap_w", "!a:x", "$e", true);
        waiting["encryptedPending"] = json!(true);
        let mut decided = row("sap_d", "!a:x", "$e", true);
        decided["decision"] = json!("approve_once");
        for unanswerable in [row("sap_o", "!a:x", "$e", false), waiting, decided] {
            assert!(!decide(&unanswerable, "approve_once"));
        }
        record_for_test("sap_s", LocalDecision::Sent("deny".into()));
        assert!(!decide(&row("sap_s", "!a:x", "$e", true), "approve_once"), "answered once is enough");
        forget_identity();
    }

    /// What the daemon lists for a local Agent's request.
    fn view(id: &str, room: &str, event: &str, tool: &str, arguments: Value, in_s: i64) -> Value {
        json!({"proposalId":id,"argsDigest":"a".repeat(64),"eventId":event,"threadRoot":room,
            "proposal":{"scope":{"agent":"agt_1","binding":"bnd_1","room":room,"requester":"@guest:x","thread":room},
                "dispatch":"dsp_1","tool":tool,"arguments":arguments,"canonical_directory":"/w","risk":"high",
                "policy_revision":[1,0,0],"expires":(now_ms() / 1000) as i64 + in_s}})
    }
    fn names() -> HashMap<String, String> {
        HashMap::from([("agt_1".to_owned(), "Octos Shell".to_owned())])
    }

    #[test]
    fn a_local_agents_request_reads_like_the_others_and_says_who_asked() {
        let shell = view("p1", "!a:x", "$ask", "octos.shell", json!({"command":"ls -l","timeout_secs":30}), 90);
        let row = local_row(&shell, &names(), "@owner:x").unwrap();
        let pending = PendingApproval { row: row.clone(), local: LocalDecision::None };
        assert!(pending.is_local() && pending.can_decide() && !pending.session_allowed());
        assert_eq!((pending.id(), pending.room_id(), pending.event_id(), pending.digest()), ("local:p1", "!a:x", "$ask", "a".repeat(64).as_str()));
        // A room id is no message: the card goes on the message that asked.
        assert_eq!(pending.thread_root(), "");
        let (title, summary) = describe(&row);
        assert_eq!(title, "Octos Shell wants to use octos.shell");
        assert_eq!(summary, "ls -l\nIn /w\nRequested by @guest:x");
        // The owner's own message needs no name.
        assert_eq!(describe(&local_row(&shell, &names(), "@guest:x").unwrap()).1, "ls -l\nIn /w");
        // An Agent the owner's list does not name is still announced.
        assert!(describe(&local_row(&shell, &HashMap::new(), "@owner:x").unwrap()).0.starts_with("Agent wants to use"));
        // A request that would reach the model shows what it would send.
        let mut ask = view("p2", "!a:x", "$ask", "model.request", json!({"inputPreview":"summarise the notes"}), 90);
        ask["proposal"]["risk"] = json!("model_request");
        let (title, summary) = describe(&local_row(&ask, &names(), "@guest:x").unwrap());
        assert_eq!((title.as_str(), summary.as_str()), ("Octos Shell asks for a permission", "Request preview: summarise the notes"));
        // Anything malformed is left out.
        for (key, bad) in [("proposalId", json!("")), ("argsDigest", json!("short"))] {
            let mut broken = shell.clone();
            broken[key] = bad;
            assert!(local_row(&broken, &names(), "@owner:x").is_none(), "{key}");
        }
        for (key, bad) in [("room", json!("not-a-room")), ("agent", json!(""))] {
            let mut broken = shell.clone();
            broken["proposal"]["scope"][key] = bad;
            assert!(local_row(&broken, &names(), "@owner:x").is_none(), "{key}");
        }
        let mut broken = shell;
        broken["proposal"]["expires"] = Value::Null;
        assert!(local_row(&broken, &names(), "@owner:x").is_none());
    }

    #[test]
    fn local_and_pooled_reads_each_replace_only_their_own_requests() {
        let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget_identity();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let local = |views: Vec<Value>| LocalApprovalsUpdate {
            identity: "me".into(),
            result: Ok((Some(views.iter().filter_map(|v| local_row(v, &names(), "@owner:x")).collect()), names())),
        };
        let shell = view("p1", "!a:x", "$ask", "octos.shell", json!({"command":"ls"}), 90);
        let expired = view("p0", "!a:x", "$old", "octos.shell", json!({"command":"ls"}), -5);
        assert!(apply_local(&mut cx, &local(vec![shell.clone(), expired])));
        let a = RoomId::parse("!a:x").unwrap();
        let b = RoomId::parse("!b:x").unwrap();
        assert_eq!(for_event(&a, "$ask", false).map(|p| p.id().to_owned()).as_deref(), Some("local:p1"));
        assert!(for_event(&a, "$old", false).is_none(), "an expired request is not taken in");
        assert_eq!((count_for_room(&a), total()), (1, 1));
        // Announced once, here or by the Agent page, never by both.
        assert!(!first_notice("me", "local:p1"));
        assert!(first_notice("me", "local:p9") && !first_notice("me", "local:p9"));
        assert!(!apply_local(&mut cx, &local(vec![shell.clone()])), "the same reading changes nothing");
        // The management reads leave local requests alone, and the other way round.
        let pooled = PendingApprovalsUpdate { identity: "me".into(), result: Ok(json!({"approvals":[row("sap_b", "!b:x", "$x", true)]})) };
        assert!(apply(&mut cx, &pooled));
        assert!(!merge_room("me", "!a:x", &json!({"approvals":[]}), true));
        assert_eq!((count_for_room(&a), count_for_room(&b), total()), (1, 1, 2));
        // A failed read keeps what was known; nothing running here clears it.
        assert!(!apply_local(&mut cx, &LocalApprovalsUpdate { identity: "me".into(), result: Err("native_owner_busy".into()) }));
        assert_eq!(total(), 2);
        assert!(apply_local(&mut cx, &LocalApprovalsUpdate { identity: "me".into(), result: Ok((None, names())) }));
        assert_eq!((count_for_room(&a), count_for_room(&b)), (0, 1));
        forget_identity();
    }

    #[test]
    fn a_local_request_is_approved_once_or_refused_and_nothing_wider() {
        let _turn = TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget_identity();
        let row = local_row(&view("p1", "!a:x", "$ask", "octos.shell", json!({"command":"ls"}), 90), &names(), "@owner:x").unwrap();
        assert!(!decide(&row, "approve_session"));
        assert!(!decide(&row, "approve_always"));
        let mut nameless = row.clone();
        nameless["approval"]["agentId"] = json!("");
        assert!(!decide(&nameless, "approve_once"));
        record_for_test("local:p1", LocalDecision::Sent("deny".into()));
        assert!(!decide(&row, "approve_once"), "answered once is enough");
        forget_identity();
    }
}
