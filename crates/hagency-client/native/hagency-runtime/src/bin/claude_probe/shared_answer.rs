//! Offline peers whose stream fails, or is stopped, after the final answer.
use super::*;

fn answer(prompt: &Value) -> Value {
    json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":false,
        "result":"shared offline answer","user_message_uuid":prompt["uuid"],"user_message_uuids":[prompt["uuid"]],
        "modelUsage":{"offline-model":{"inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}})
}
fn background(live: bool) -> Value {
    json!({"type":"system","subtype":"background_tasks_changed","session_id":"owned-claude",
        "tasks":if live { vec![json!({"task_id":"child"})] } else { vec![] }})
}
fn idle() -> Value {
    json!({"type":"system","subtype":"session_state_changed","session_id":"owned-claude","state":"idle"})
}
fn raw(line: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(line)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// `None`: not one of these modes. `Some(hold)`: whether the process then
/// stays alive until its original owner stops it.
pub(super) fn run(mode: &str, input: &mut impl BufRead, prompt: &Value) -> io::Result<Option<bool>> {
    let mut line = String::new();
    match mode {
        // The process dies while its background child is still running.
        "shared-background-exit" => {
            emit(background(true))?;
            emit(answer(prompt))?;
            Ok(Some(false))
        }
        // One later tool result is larger than the host's 1 MiB frame bound.
        "shared-result-oversized" => {
            emit(answer(prompt))?;
            emit(
                json!({"type":"user","session_id":"owned-claude","parent_tool_use_id":"child-tool",
                "message":{"content":[{"type":"tool_result","tool_use_id":"child-tool","content":"x".repeat(1024 * 1024 + 1)}]}}),
            )?;
            Ok(Some(true))
        }
        // A frame the host cannot parse follows the answer.
        "shared-result-garbled" => {
            emit(answer(prompt))?;
            raw(br#"{"type":"assistant","session_id":"#)?;
            Ok(Some(true))
        }
        // An error result is not an answer, whatever happens afterwards.
        "shared-error-exit" => {
            let mut failed = answer(prompt);
            failed["subtype"] = json!("error_during_execution");
            failed["is_error"] = json!(true);
            emit(failed)?;
            Ok(Some(false))
        }
        // Background work reports again after the one accounting query.
        "shared-snapshot-again" => {
            emit(background(true))?;
            emit(answer(prompt))?;
            emit(background(false))?;
            emit(idle())?;
            input.read_line(&mut line)?;
            let query: Value = serde_json::from_str(&line)?;
            if query["type"] != "user" || query["message"]["content"] != "/usage" {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            emit(
                json!({"type":"system","subtype":"init","session_id":"owned-claude","user_message_uuids":[query["uuid"]]}),
            )?;
            let mut snapshot = answer(prompt);
            snapshot["result"] = json!("local accounting report");
            snapshot["user_message_uuid"] = query["uuid"].clone();
            snapshot["user_message_uuids"] = json!([query["uuid"]]);
            snapshot["model_usage_since_last_result"] = json!({"offline-model":{
                "inputTokens":0,"outputTokens":0,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}});
            emit(snapshot)?;
            emit(
                json!({"type":"system","subtype":"task_notification","session_id":"owned-claude","task_id":"child","status":"completed"}),
            )?;
            emit(idle())?;
            Ok(Some(true))
        }
        // The stop request arrives only after the final answer was written.
        "shared-stop-after-answer" => {
            emit(answer(prompt))?;
            input.read_line(&mut line)?;
            let request: Value = serde_json::from_str(&line)?;
            if request["type"] != "control_request" || request["request"]["subtype"] != "interrupt" {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            emit(
                json!({"type":"control_response","response":{"subtype":"success","request_id":request["request_id"],"response":{"still_queued":[]}}}),
            )?;
            emit(idle())?;
            Ok(Some(true))
        }
        _ => Ok(None),
    }
}
