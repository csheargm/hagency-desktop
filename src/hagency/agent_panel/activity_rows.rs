//! Pure text for the Activity tab: waiting approvals first, then the recent
//! requests (`LocalAction::RecentActivity`), newest first. No widget state.
use crate::i18n::{format, tr};
use serde_json::Value;

/// Longest approval detail (a shell command) shown on one line.
const DETAIL_CHARS: usize = 60;
/// Room id characters kept when the room has no known name.
const ROOM_ID_CHARS: usize = 10;

/// The Activity answer the panel keeps: `{requests, approvals}` where each
/// request may carry `roomName` / `requesterName` resolved by this client.
pub(crate) fn activity_lines(value: &Value, time: impl Fn(i64) -> String) -> Vec<String> {
    let approvals = json_approvals(value);
    let mut lines: Vec<String> = approvals["approvals"].as_array().into_iter().flatten()
        .map(|a| {
            let tool = a["proposal"]["tool"].as_str().unwrap_or("a tool").to_owned();
            let args = &a["proposal"]["arguments"];
            let detail = args["command"].as_str().or(args["cmd"].as_str()).map(str::to_owned).unwrap_or_else(|| args.to_string());
            format("Waiting for your approval: {tool} · {detail}", &[("tool", tool), ("detail", clip(&detail, DETAIL_CHARS))])
        })
        .collect();
    let waiting: Vec<&str> = approvals["approvals"].as_array().into_iter().flatten()
        .filter_map(|a| a["proposal"]["dispatch"].as_str()).collect();
    for request in value["requests"].as_array().into_iter().flatten() {
        let held = request["dispatchId"].as_str().is_some_and(|id| waiting.contains(&id));
        lines.push(request_line(request, held, &time(request["receivedAt"].as_i64().unwrap_or(0))));
    }
    lines
}
fn json_approvals(value: &Value) -> Value {
    serde_json::json!({"approvals": value["approvals"].clone()})
}

/// "time · requester · room · outcome[ · N tokens]".
pub(crate) fn request_line(request: &Value, waiting: bool, time: &str) -> String {
    let requester = non_empty(&request["requesterName"]).or(non_empty(&request["requester"])).unwrap_or("?").to_owned();
    let room = non_empty(&request["roomName"]).map(str::to_owned).unwrap_or_else(|| short_room(request["roomId"].as_str().unwrap_or("")));
    let outcome = tr(outcome_label(request["outcome"].as_str().unwrap_or(""), waiting));
    let mut parts = vec![time.to_owned(), requester, room, outcome.to_owned()];
    if let Some(tokens) = request["tokens"].as_u64().filter(|t| *t > 0) {
        parts.push(format("{tokens} tokens", &[("tokens", tokens.to_string())]));
    }
    parts.join(" · ")
}

/// Ledger outcome (hagency-agent-local `activity::Outcome`) to its UI label.
pub(crate) fn outcome_label(outcome: &str, waiting: bool) -> &'static str {
    match outcome {
        "running" if waiting => "Waiting for approval",
        "running" => "Running",
        "answered" => "Answered",
        "declined" => "Declined",
        _ => "Failed",
    }
}

/// "!abcdefghi…" for a room this client cannot name.
pub(crate) fn short_room(room_id: &str) -> String {
    let local = room_id.split(':').next().unwrap_or(room_id);
    clip(local, ROOM_ID_CHARS)
}
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_owned(); }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}
fn non_empty(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// Local "MM-DD HH:MM" for unix seconds.
pub(crate) fn local_time(seconds: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(seconds, 0).single().map(|t| t.format("%m-%d %H:%M").to_string()).unwrap_or_default()
}
