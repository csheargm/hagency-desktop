//! Offline persisted conversation with process-local native usage totals.
use super::*;

pub(super) fn run(marker: &Path, prompt: &Value) -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let resume = args
        .windows(2)
        .find(|a| a[0] == "--resume")
        .map(|a| a[1].as_str());
    if prompt["type"] != "user"
        || prompt["message"]["role"] != "user"
        || prompt["session_id"] != resume.unwrap_or("")
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut native = resume
        .map(str::to_owned)
        .unwrap_or_else(|| format!("offline-{}", std::process::id()));
    let path = marker.with_file_name(format!("{native}.json"));
    let previous: Vec<String> = if resume.is_some() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        Vec::new()
    };
    let text = format!("Previous native inputs: {}", previous.join("; "));
    let mut inputs = previous;
    inputs.push(
        prompt["message"]["content"]
            .as_str()
            .ok_or(io::ErrorKind::InvalidInput)?
            .into(),
    );
    fs::write(&path, serde_json::to_vec(&inputs)?)?;
    fs::write(marker.with_extension("prompt"), serde_json::to_vec(prompt)?)?;
    if marker.with_extension("wrong_identity").exists() {
        native = "wrong-native-context".into();
    }
    if marker.with_extension("early_permission").exists() {
        emit(
            json!({"type":"control_request","request_id":"early","request":{
            "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"early-tool","input":{}}}),
        )?;
        return super::super::pulse(marker);
    }
    // Installed CLI 2.1.321 restores messages, but a resumed process starts
    // modelUsage at zero. This fixture runs exactly one turn per process.
    let calls = 1;
    let input = if marker.with_extension("invalid_usage").exists() {
        json!(-1)
    } else {
        json!(10 * calls)
    };
    emit(
        json!({"type":"system","subtype":"init","session_id":native,"user_message_uuid":prompt["uuid"]}),
    )?;
    emit(
        json!({"type":"result","subtype":"success","session_id":native,"is_error":false,"result":text,
        "user_message_uuid":prompt["uuid"],"user_message_uuids":[prompt["uuid"]],
        "modelUsage":{"offline-model":{"inputTokens":input,"outputTokens":2*calls,
            "cacheReadInputTokens":3*calls,"cacheCreationInputTokens":4*calls,
            "thinkingTokens":1,"cacheCreationEphemeral5mInputTokens":4,"cacheCreationEphemeral1hInputTokens":0,
            "webSearchRequests":0,"costUSD":0.001,"contextWindow":200000,"maxOutputTokens":64000,
            "canonicalModel":"offline-model","provider":"fixture","costBasis":"synthetic"}}}),
    )?;
    emit(
        json!({"type":"system","subtype":"session_state_changed","session_id":native,"state":"idle"}),
    )?;
    super::super::pulse(marker)
}
