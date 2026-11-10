//! Explicit offline peer mode; never invokes Claude or another provider.
use serde_json::{Value, json};
mod shared_session;
mod shared_history;
mod shared_interrupt;
mod shared_answer;
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
};

fn emit(mut value: Value) -> io::Result<()> {
    if value["type"] == "result" {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        value["uuid"] = json!(format!(
            "fixture-result-{}",
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        // Each offline probe process emits one model turn. Current native
        // result deltas are explicit even when restored totals are present.
        if value.get("model_usage_since_last_result").is_none() && value["modelUsage"].is_object() {
            value["model_usage_since_last_result"] = value["modelUsage"].clone();
        }
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
pub(super) fn run(mode: &str, marker: &Path) -> io::Result<()> {
    if !matches!(
        mode,
        "normal"
            | "malformed"
            | "stall"
            | "permission-allow"
            | "permission-deny"
            | "permission-cancel"
            | "permission-hold"
            | "usage"
            | "usage-conflict"
            | "account-match"
            | "account-wrong-email"
            | "account-api-key"
            | "account-provider"
            | "account-missing"
            | "shared-usage"
            | "shared-result-exit"
            | "shared-wait"
            | "shared-session"
            | "shared-history"
            | "shared-interrupt"
            | "shared-followup-success"
            | "shared-followup-error"
            | "shared-background-exit"
            | "shared-result-oversized"
            | "shared-result-garbled"
            | "shared-error-exit"
            | "shared-snapshot-again"
            | "shared-stop-after-answer"
    ) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(marker.with_extension("entered"), b"offline")?;
    let mut stdin = io::stdin().lock();
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    if request["type"] != "control_request" || request["request"]["subtype"] != "initialize" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if mode == "stall" {
        drop(stdin);
        return super::pulse(marker);
    }
    if mode == "malformed" {
        io::stdout().write_all(b"{\"type\":\"control_response\"}\n")?;
        io::stdout().flush()?;
        drop(stdin);
        return super::pulse(marker);
    }
    if mode == "shared-session" {
        let args: Vec<_> = std::env::args().collect();
        // A resumed launch can fail to start while its saved context is still
        // on disk. `<id>.startup` names that failure; none of these outputs
        // says the context is missing.
        if let Some(id) = args.windows(2).find(|p| p[0] == "--resume").map(|p| &p[1])
            && let Ok(failure) = fs::read_to_string(marker.with_file_name(format!("{id}.startup")))
        {
            match failure.as_str() {
                "refused" => emit(
                    json!({"type":"control_response","response":{"subtype":"error","request_id":request["request_id"],"error":"Resume failed"}}),
                )?,
                "unexpected" => emit(
                    json!({"type":"system","subtype":"status","session_id":id,"status":"compacting"}),
                )?,
                "reworded" => emit(
                    json!({"type":"result","session_id":id,"subtype":"error_during_execution",
                        "is_error":true,"num_turns":0,"duration_api_ms":0,"total_cost_usd":0,
                        "modelUsage":{},"permission_denials":[],"errors":[format!("Native session file cannot be opened: {id}")],
                        "usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}),
                )?,
                "stall" => {
                    drop(stdin);
                    return super::pulse(marker);
                }
                // "eof": the process ends without a word.
                _ => {}
            }
            return Ok(());
        }
        // A fresh launch can be made to end before it answers, too.
        if !args.iter().any(|a| a == "--resume") && marker.with_file_name("fresh.startup").exists() {
            return Ok(());
        }
        if let Some(id) = args.windows(2).find(|p| p[0] == "--resume").map(|p| &p[1])
            && !marker.with_file_name(format!("{id}.json")).exists()
        {
            emit(
                json!({"type":"result","session_id":id,"subtype":"error_during_execution",
                    "is_error":true,"num_turns":0,"duration_api_ms":0,"total_cost_usd":0,
                    "modelUsage":{},"permission_denials":[],"errors":[format!("No conversation found with session ID: {id}")],
                    "usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}),
            )?;
            return Ok(());
        }
    }
    let mut initialization = json!({"claude_code_version":"2.1.321"});
    if (mode.starts_with("account-") && mode != "account-missing") || mode.starts_with("shared-") {
        initialization["account"] =
            json!({"email":"owner@example.test","apiProvider":"firstParty"});
        match mode {
            "account-wrong-email" => {
                initialization["account"]["email"] = json!("another@example.test")
            }
            "account-api-key" => initialization["account"]["apiKeySource"] = json!("user"),
            "account-provider" => initialization["account"]["apiProvider"] = json!("bedrock"),
            _ => {}
        }
    }
    emit(
        json!({"type":"control_response","response":{"subtype":"success","request_id":request["request_id"],"response":initialization}}),
    )?;
    line.clear();
    stdin.read_line(&mut line)?;
    let first: Value = serde_json::from_str(&line)?;
    let prompt: Value =
        if mode.starts_with("shared-") && first["request"]["subtype"] == "mcp_status" {
            shared_history::bind_and_prompt(&mut stdin, first)?
        } else {
            first
        };
    if mode == "shared-session" {
        drop(stdin);
        return shared_session::run(marker, &prompt);
    }
    if prompt["type"] != "user" || prompt["message"]["role"] != "user" || prompt["session_id"] != ""
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(
        marker.with_extension("prompt"),
        serde_json::to_vec(&prompt)?,
    )?;
    emit(
        json!({"type":"system","subtype":"init","session_id":"owned-claude","user_message_uuid":prompt["uuid"]}),
    )?;
    if mode.starts_with("shared-") {
        if let Some(hold) = shared_answer::run(mode, &mut stdin, &prompt)? {
            drop(stdin);
            return if hold { super::pulse(marker) } else { Ok(()) };
        }
        if matches!(mode, "shared-followup-success" | "shared-followup-error") {
            let final_error = mode == "shared-followup-error";
            let delta = json!({"offline-model":{"inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}});
            emit(
                json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":!final_error,"result":"earlier native answer","user_message_uuid":prompt["uuid"],"modelUsage":delta}),
            )?;
            emit(
                json!({"type":"system","subtype":"background_tasks_changed","session_id":"owned-claude","tasks":[{"task_id":"child","run_id":"original-child-run"}]}),
            )?;
            emit(
                json!({"type":"system","subtype":"task_notification","session_id":"owned-claude","task_id":"child","run_id":"original-child-run","status":"completed"}),
            )?;
            emit(
                json!({"type":"system","subtype":"background_tasks_changed","session_id":"owned-claude","tasks":[]}),
            )?;
            emit(json!({"type":"system","subtype":"init","session_id":"owned-claude"}))?;
            emit(
                json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":final_error,"result":"shared background answer","origin":{"kind":"task-notification","producer":"session-task","runId":"original-child-run"},"modelUsage":{"offline-model":{"inputTokens":20,"outputTokens":4,"cacheReadInputTokens":6,"cacheCreationInputTokens":8}},"model_usage_since_last_result":delta}),
            )?;
            emit(
                json!({"type":"system","subtype":"session_state_changed","session_id":"owned-claude","state":"idle"}),
            )?;
            drop(stdin);
            return super::pulse(marker);
        }
        if mode == "shared-interrupt" {
            shared_interrupt::run(&mut stdin, marker, &prompt)?;
            drop(stdin);
            return super::pulse(marker);
        }
        if mode == "shared-history" {
            shared_history::read_all(&mut stdin, marker)?;
        }
        if matches!(
            mode,
            "shared-usage" | "shared-result-exit" | "shared-history"
        ) {
            emit(
                json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":false,"result":"shared offline answer","user_message_uuid":prompt["uuid"],"user_message_uuids":[prompt["uuid"]],"modelUsage":{"offline-model":{"inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}),
            )?;
            if mode == "shared-result-exit" {
                return Ok(());
            }
            emit(
                json!({"type":"system","subtype":"session_state_changed","session_id":"owned-claude","state":"idle"}),
            )?;
        }
        drop(stdin);
        return super::pulse(marker);
    }
    if mode.starts_with("permission-") {
        let input = json!({"command":"offline literal $(no shell) 中文"});
        emit(
            json!({"type":"control_request","request_id":"owned-permission","request":{
            "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"owned-tool","input":input}}),
        )?;
        if mode == "permission-cancel" {
            emit(json!({"type":"control_cancel_request","request_id":"owned-permission"}))?;
            fs::write(marker.with_extension("cancelled"), b"offline")?;
            drop(stdin);
            return super::pulse(marker);
        }
        if mode == "permission-hold" {
            drop(stdin);
            return super::pulse(marker);
        }
        line.clear();
        stdin.read_line(&mut line)?;
        let response: Value = serde_json::from_str(&line)?;
        let expected = if mode == "permission-allow" {
            json!({"behavior":"allow","updatedInput":input})
        } else {
            json!({"behavior":"deny","message":hagency_runtime::claude::session::DENIED,"interrupt":false})
        };
        if response
            != json!({"type":"control_response","response":{
            "subtype":"success","request_id":"owned-permission","response":expected}})
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        fs::write(
            marker.with_extension("response"),
            serde_json::to_vec(&response)?,
        )?;
    }
    if mode.starts_with("usage") {
        let mut message = json!({"type":"assistant","session_id":"owned-claude","uuid":"one","parent_tool_use_id":null,
            "message":{"id":"step-one","usage":{"input_tokens":10,"output_tokens":999,"cache_read_input_tokens":20,"cache_creation_input_tokens":30}}});
        emit(message.clone())?;
        message["uuid"] = json!("two");
        if mode == "usage-conflict" {
            message["message"]["usage"]["input_tokens"] = json!(11);
        }
        emit(message.clone())?;
        message["parent_tool_use_id"] = json!("parent");
        emit(message.clone())?;
        message["parent_tool_use_id"] = Value::Null;
        message["message"]["id"] = json!("step-two");
        emit(message)?;
        emit(
            json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":true,
            "modelUsage":{"private-model-one":{"inputTokens":100,"outputTokens":200,"cacheReadInputTokens":300,"cacheCreationInputTokens":400},
                "private-model-two":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}),
        )?;
    } else {
        emit(
            json!({"type":"assistant","session_id":"owned-claude","message":{"content":[{"type":"text","text":"offline"}]}}),
        )?;
        emit(
            json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":false,"result":"offline"}),
        )?;
    }
    // Deliberately remains alive after result AND after stdin closes. Only the
    // original guardian's stop or the bounded fixture fuse ends the pulse.
    drop(stdin);
    super::pulse(marker)
}
