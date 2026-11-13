//! Offline provider whose turn asks one question with the native question
//! tool, then answers with exactly what the host returned for it.
use super::*;

pub(super) fn run(input: &mut impl BufRead, marker: &Path, prompt: &Value) -> io::Result<()> {
    let asked = json!({"questions":[{
        "question":"Which database should this use?","header":"Database","multiSelect":false,
        "options":[
            {"label":"SQLite","description":"One file"},
            {"label":"Postgres (Recommended)","description":"A server"}
        ]}]});
    emit(
        json!({"type":"control_request","request_id":"owned-question","request":{
        "subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"owned-question-tool","input":asked}}),
    )?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line)?;
    fs::write(
        marker.with_extension("question"),
        serde_json::to_vec(&response)?,
    )?;
    let decision = &response["response"]["response"];
    let text = if decision["behavior"] == "allow" {
        format!("Answer: {}", decision["updatedInput"]["answers"])
    } else {
        "No answer".to_owned()
    };
    emit(
        json!({"type":"result","subtype":"success","session_id":"owned-claude","is_error":false,
        "result":text,"user_message_uuid":prompt["uuid"],"user_message_uuids":[prompt["uuid"]],
        "modelUsage":{"offline-model":{"inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}),
    )?;
    emit(
        json!({"type":"system","subtype":"session_state_changed","session_id":"owned-claude","state":"idle"}),
    )
}
