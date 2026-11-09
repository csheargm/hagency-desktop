//! App-wide awareness of pending shared-execution permission requests.
//!
//! The executor gives the owner about two minutes to answer a Codex permission
//! request. Until now the request was visible only inside the open room or the
//! owner console page, so an owner elsewhere in the app simply timed out. This
//! module polls the authenticated management list (never chat text) on a fixed
//! cadence, announces new actionable requests, and lets the rooms list, the
//! Agent tab and the message timeline show them in place.
//!
//! Nothing here grants authority: every decision still goes through
//! `hagency.shared_approvals.decide`, where the management server re-checks the
//! deciding account, the request digest and expiry.
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use ::hagency::native_owner::ManagementCommand;
use makepad_widgets::*;
use matrix_sdk::ruma::RoomId;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

/// Seconds between management polls. Requests expire after at most 120 s, so
/// this leaves the owner well over a minute even when nothing else refreshes.
pub const POLL_SECONDS: f64 = 10.0;

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
}

/// What this client has done with a request since the last poll.
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
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

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

/// Ask the management server for every request this account may read.
/// Silently does nothing without a signed-in Hagency identity.
pub fn poll() {
    let Ok(service) = crate::hagency::backend::Service::for_current() else { return };
    let identity = service.identity();
    crate::sliding_sync::spawn_async_task(async move {
        let result = service
            .shared_permissions(ManagementCommand::SharedApprovals { room_id: None })
            .await;
        Cx::post_action(PendingApprovalsUpdate { identity, result });
    });
}

/// Record a poll answer. Returns true when the pending set changed, so callers
/// can redraw lists and timelines. New actionable requests are announced once.
pub fn apply(cx: &mut Cx, update: &PendingApprovalsUpdate) -> bool {
    let Ok(value) = &update.result else { return false };
    let now = now_ms();
    let fresh: Vec<Value> = value["approvals"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["decision"].is_null())
                .filter(|row| row["approval"]["expiresAtMs"].as_u64().is_none_or(|t| t > now))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let (changed, announce) = {
        let Ok(mut state) = STATE.lock() else { return false };
        if state.identity != update.identity {
            state.rows.clear();
            state.announced.clear();
            state.identity = update.identity.clone();
        }
        let before: Vec<String> = state.rows.iter().map(|p| p.id().to_owned()).collect();
        let previous: HashMap<String, LocalDecision> =
            state.rows.iter().map(|p| (p.id().to_owned(), p.local.clone())).collect();
        state.rows = fresh
            .into_iter()
            .map(|row| {
                let id = row["approval"]["id"].as_str().unwrap_or_default().to_owned();
                PendingApproval { local: previous.get(&id).cloned().unwrap_or(LocalDecision::None), row }
            })
            .collect();
        let after: Vec<String> = state.rows.iter().map(|p| p.id().to_owned()).collect();
        let announce: Vec<PendingApproval> = state
            .rows
            .iter()
            .filter(|p| p.can_decide() && !state.announced.contains(p.id()))
            .cloned()
            .collect();
        for p in &announce {
            state.announced.insert(p.id().to_owned());
        }
        (before != after, announce)
    };
    for p in announce {
        let (title, summary) = describe(&p.row);
        enqueue_popup_notification(
            format!("{title}\n{summary}\n{}", crate::i18n::tr("Open the room or the Agent page to decide.")),
            PopupKind::Info,
            Some(20.0),
        );
        // A no-op on backends without system notifications; iOS shows it.
        cx.show_notification(&title, &summary);
    }
    changed
}

/// Record the outcome of a decision this client sent.
pub fn apply_decided(update: &PendingApprovalDecided) {
    let Ok(mut state) = STATE.lock() else { return };
    if let Some(p) = state.rows.iter_mut().find(|p| p.id() == update.id) {
        p.local = match &update.result {
            Ok(()) => LocalDecision::Sent(update.decision.clone()),
            Err(error) => LocalDecision::Failed(update.decision.clone(), error.clone()),
        };
    }
    if let Err(error) = &update.result {
        enqueue_popup_notification(
            format!("{} {}", crate::i18n::tr("Could not send the decision:"), decide_error(error)),
            PopupKind::Error,
            Some(8.0),
        );
    }
}

/// Send `decision` for the request `id`. The card shows "sending" until the
/// server answers; the next poll drops the row once the executor consumed it.
pub fn decide(id: &str, digest: &str, decision: &str) {
    {
        let Ok(mut state) = STATE.lock() else { return };
        let Some(p) = state.rows.iter_mut().find(|p| p.id() == id) else { return };
        if !p.can_decide() || !matches!(p.local, LocalDecision::None | LocalDecision::Failed(..)) {
            return;
        }
        p.local = LocalDecision::Sending(decision.to_owned());
    }
    let (id, digest, decision) = (id.to_owned(), digest.to_owned(), decision.to_owned());
    let service = crate::hagency::backend::Service::for_current();
    crate::sliding_sync::spawn_async_task(async move {
        let result = match service {
            Ok(service) => service
                .shared_permissions(ManagementCommand::SharedApprovalDecide {
                    id: id.clone(),
                    digest,
                    decision: decision.clone(),
                })
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        };
        Cx::post_action(PendingApprovalDecided { id, decision, result });
    });
}

/// Actionable requests in `room`, for list badges.
pub fn count_for_room(room: &RoomId) -> u64 {
    STATE.lock().map_or(0, |s| {
        s.rows.iter().filter(|p| p.room_id() == room.as_str() && p.can_decide() && p.local == LocalDecision::None).count() as u64
    })
}
/// Actionable requests anywhere, for the Agent tab.
pub fn total() -> u64 {
    STATE.lock().map_or(0, |s| s.rows.iter().filter(|p| p.can_decide() && p.local == LocalDecision::None).count() as u64)
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

/// A title and a one-paragraph summary of what the Agent asked for, in the
/// same words the owner console uses. Both languages are shown.
pub fn describe(row: &Value) -> (String, String) {
    let a = &row["approval"];
    let name = a["agentName"].as_str().unwrap_or("Agent");
    if row["encryptedPending"] == true {
        return (
            format!("{name} · request on its way / 请求还在路上"),
            crate::i18n::tr("Waiting for the encrypted details from the Agent's device.").to_owned(),
        );
    }
    let request = &a["request"];
    let params = &request["params"];
    let reason = params["reason"].as_str().or_else(|| request["reason"].as_str()).unwrap_or("");
    let mut lines = Vec::new();
    let title = match request["method"].as_str() {
        Some("item/commandExecution/requestApproval") => {
            lines.push(command_words(&params["command"]));
            if let Some(cwd) = params["cwd"].as_str() {
                lines.push(format!("in / 位置：{cwd}"));
            }
            format!("{name} wants to run a command / 想运行一条命令")
        }
        Some("item/fileChange/requestApproval") => {
            if let Some(root) = params["grantRoot"].as_str() {
                lines.push(root.to_owned());
            }
            format!("{name} wants to change files / 想修改文件")
        }
        Some("claude/can_use_tool") => {
            let tool=params["toolName"].as_str().unwrap_or("tool");
            lines.push(short_json(&params["input"]));
            format!("{name} wants to use {tool} / 想调用 {tool}")
        }
        Some("item/permissions/requestApproval") => {
            let p = &params["permissions"];
            if p.get("network").is_some() {
                lines.push("network access / 网络访问".into());
            }
            if p.get("fileSystem").is_some() {
                lines.push(format!("file system: {} / 文件系统", short_json(&p["fileSystem"])));
            }
            format!("{name} asks for permissions / 请求权限")
        }
        _ => format!("{name} asks for a permission / 请求一项权限"),
    };
    if !reason.is_empty() {
        lines.push(format!("reason / 原因：{reason}"));
    }
    (title, lines.join("\n"))
}
fn command_words(command: &Value) -> String {
    match command {
        Value::Array(parts) => parts.iter().map(|p| p.as_str().map(str::to_owned).unwrap_or_else(|| p.to_string())).collect::<Vec<_>>().join(" "),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
fn short_json(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 160 { format!("{}…", &text[..text.floor_char_boundary(160)]) } else { text }
}
fn decide_error(code: &str) -> &'static str {
    match code {
        "approval_request_changed" | "permission_request_changed" => "the request changed; reopen it / 请求已变化，请重新打开",
        "approval_already_decided" => "someone already decided / 已有人决定",
        "approval_expired" => "the request expired / 请求已过期",
        "resource_owner_required" => "only the resource owner can approve this / 只有资源所有者能批准",
        "encrypted_approval_unavailable" => "the encrypted details have not arrived / 加密详情尚未到达",
        _ => "please retry / 请重试",
    }
}
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

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
        assert!(summary.contains("git push") && summary.contains("in / 位置：/w") && summary.contains("deploy"));
        let mut pending = row("sap_1", "!r:x", "$e", true);
        pending["encryptedPending"] = json!(true);
        assert!(describe(&pending).0.contains("on its way"));
    }

    #[test]
    fn pending_rows_are_indexed_by_room_event_and_thread_root() {
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
}
