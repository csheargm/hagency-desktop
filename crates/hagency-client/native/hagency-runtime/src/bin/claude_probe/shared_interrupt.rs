//! Offline provider which has a real output-token result only after interrupt.
use super::*;

pub(super) fn run(input: &mut impl BufRead, marker: &Path, prompt: &Value) -> io::Result<()> {
    emit(
        json!({"type":"assistant","session_id":"owned-claude","uuid":"step",
        "parent_tool_use_id":null,"message":{"id":"step-one","usage":{
            "input_tokens":10,"output_tokens":999,"cache_read_input_tokens":3,"cache_creation_input_tokens":4
        }}}),
    )?;
    fs::write(
        marker.with_extension("awaiting-interrupt"),
        b"original turn",
    )?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    if request["type"] != "control_request" || request["request"]["subtype"] != "interrupt" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(
        marker.with_extension("interrupted"),
        serde_json::to_vec(&request)?,
    )?;
    emit(
        json!({"type":"control_response","response":{"subtype":"success","request_id":request["request_id"],"response":{}}}),
    )?;
    let usage = json!({"offline-model":{
        "inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,
        "webSearchRequests":0,"costUSD":0.0,"contextWindow":200000,"maxOutputTokens":32000,
        "thinkingTokens":0,"cacheCreationEphemeral1hInputTokens":0,"cacheCreationEphemeral5mInputTokens":4,
        "canonicalModel":"offline-model","provider":"firstParty","costBasis":"list"
    }});
    emit(
        json!({"type":"result","subtype":"success","session_id":"owned-claude", "is_error":false,
            "uuid":"01234567-89ab-cdef-0123-456789abcd00",
            "result":"answer captured during cooperative stop","user_message_uuid":prompt["uuid"],"user_message_uuids":[prompt["uuid"]],
            "modelUsage":usage,"model_usage_since_last_result":usage
        }),
    )?;
    emit(
        json!({"type":"system","subtype":"session_state_changed","session_id":"owned-claude","state":"idle"}),
    )
}
