//! Bounded, read-only work observations. No prompts, reply bodies or executor
//! credentials are projected, and per-call retained tokens are not free capacity.
use super::{ManagementCommand, NativeError, ServerOperation, failure};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_EXACT: i64 = 9_007_199_254_740_991;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    known_consumed_tokens: i64,
    retained_tokens: Option<i64>,
    complete: bool,
    period_key: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    execution_id: String,
    event_id: String,
    requester_mxid: String,
    state: String,
    reply_state: Option<String>,
    created_at_ms: i64,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    usage: Option<Usage>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    items: Vec<Item>,
    next_after: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    work: Page,
    observed_at_ms: i64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestItem {
    request_id: String,
    project_id: Option<String>,
    project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pool_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pool_name: Option<String>,
    agent_name: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestPage {
    items: Vec<RequestItem>,
    next_after: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestReply {
    requests: RequestPage,
    observed_at_ms: i64,
}
fn invalid() -> NativeError {
    failure(503, "management_agent_work_invalid")
}
fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn timestamp(value: i64) -> bool {
    (1..=MAX_EXACT).contains(&value)
}
fn key(value: &str) -> bool {
    super::super::key(value).is_ok()
}
pub(super) fn prepare(command: &ManagementCommand) -> Result<ServerOperation, NativeError> {
    if let ManagementCommand::AgentWorkList {
        project_id,
        after,
        limit,
        include_pool_agents,
    } = command
    {
        for id in [project_id, after].into_iter().flatten() {
            super::super::key(id)?;
        }
        if limit.is_some_and(|v| !(1..=16).contains(&v)) {
            return Err(failure(400, "invalid_management_arguments"));
        }
        let mut input = json!({"projectId":project_id,"after":after,"limit":limit.unwrap_or(16)});
        // Omit the extension entirely for older servers with closed query DTOs.
        if *include_pool_agents { input["includePoolAgents"] = json!(true); }
        return Ok(ServerOperation {
            method: Method::POST,
            suffix: "agent-work/list".into(),
            input: Some(input),
        });
    }
    let ManagementCommand::AgentWork { id, after, limit } = command else {
        return Err(failure(400, "invalid_management_arguments"));
    };
    super::super::key(id)?;
    if let Some(after) = after {
        super::super::key(after)?;
    }
    if limit.is_some_and(|v| !(1..=16).contains(&v)) {
        return Err(failure(400, "invalid_management_arguments"));
    }
    Ok(ServerOperation {
        method: Method::POST,
        suffix: format!("agent-requests/{id}/work/query"),
        input: Some(json!({"after":after,"limit":limit.unwrap_or(16)})),
    })
}
pub(super) fn project_reply(
    command: &ManagementCommand,
    value: Value,
) -> Result<Value, NativeError> {
    if let ManagementCommand::AgentWorkList {
        project_id,
        after,
        limit,
        include_pool_agents,
    } = command
    {
        if value["requests"]["items"]
            .as_array()
            .is_none_or(|rows| rows.len() > usize::from(limit.unwrap_or(16)))
        {
            return Err(invalid());
        }
        let reply: RequestReply = serde_json::from_value(value).map_err(|_| invalid())?;
        if !timestamp(reply.observed_at_ms) {
            return Err(invalid());
        }
        let mut previous = after.as_deref();
        for row in &reply.requests.items {
            let source_valid = match (&row.project_id, &row.project_name, &row.pool_id, &row.pool_name) {
                (Some(id), Some(name), None, None) => key(id) && bounded(name, 160),
                (None, None, Some(id), Some(name)) => *include_pool_agents && project_id.is_none() && key(id) && bounded(name, 160),
                _ => false,
            };
            if !key(&row.request_id)
                || !source_valid
                || !bounded(&row.agent_name, 160)
                || previous.is_some_and(|last| row.request_id.as_str() <= last)
                || project_id.as_ref().is_some_and(|id| Some(id) != row.project_id.as_ref())
            {
                return Err(invalid());
            }
            previous = Some(&row.request_id);
        }
        if reply.requests.next_after.as_deref().is_some_and(|next| {
            !key(next)
                || reply
                    .requests
                    .items
                    .last()
                    .is_none_or(|row| row.request_id != next)
        }) {
            return Err(invalid());
        }
        return Ok(
            json!({"protocol":"hagency-management-v1","requests":reply.requests,"observedAtMs":reply.observed_at_ms}),
        );
    }
    let ManagementCommand::AgentWork { id, after, limit } = command else {
        return Err(invalid());
    };
    // Bound before allocating a typed page; transport also bounds total bytes.
    if value["work"]["items"]
        .as_array()
        .is_none_or(|items| items.len() > usize::from(limit.unwrap_or(16)))
    {
        return Err(invalid());
    }
    let reply: Reply = serde_json::from_value(value).map_err(|_| invalid())?;
    if !timestamp(reply.observed_at_ms) {
        return Err(invalid());
    }
    let mut previous = after.as_deref();
    for item in &reply.work.items {
        if !key(&item.id)
            || !key(&item.execution_id)
            || previous.is_some_and(|p| item.id.as_str() <= p)
            || !bounded(&item.event_id, 255)
            || !item.event_id.starts_with('$')
            || !bounded(&item.requester_mxid, 255)
            || !item.requester_mxid.starts_with('@')
            || !bounded(&item.state, 80)
            || item.reply_state.as_deref().is_some_and(|s| !bounded(s, 80))
            || !timestamp(item.created_at_ms)
            || item.started_at_ms.is_some_and(|v| !timestamp(v))
            || item.completed_at_ms.is_some_and(|v| !timestamp(v))
        {
            return Err(invalid());
        }
        if let Some(usage) = &item.usage {
            let bytes = usage.period_key.as_bytes();
            let period_valid = bytes.len() == 7
                && bytes[4] == b'-'
                && bytes[..4].iter().all(u8::is_ascii_digit)
                && matches!(
                    &usage.period_key[5..],
                    "01" | "02"
                        | "03"
                        | "04"
                        | "05"
                        | "06"
                        | "07"
                        | "08"
                        | "09"
                        | "10"
                        | "11"
                        | "12"
                );
            if !period_valid
                || !(0..=MAX_EXACT).contains(&usage.known_consumed_tokens)
                || usage
                    .retained_tokens
                    .is_some_and(|v| !(usage.known_consumed_tokens..=MAX_EXACT).contains(&v))
                || (usage.complete && usage.retained_tokens != Some(usage.known_consumed_tokens))
            {
                return Err(invalid());
            }
        }
        previous = Some(&item.id);
    }
    if reply.work.next_after.as_deref().is_some_and(|next| {
        !key(next) || reply.work.items.last().is_none_or(|last| last.id != next)
    }) {
        return Err(invalid());
    }
    // Identity comes from the validated fixed request route, not server extras.
    Ok(
        json!({"protocol":"hagency-management-v1","requestId":id,"work":reply.work,"observedAtMs":reply.observed_at_ms}),
    )
}
