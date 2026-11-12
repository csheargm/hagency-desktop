//! Supervised offline process exercising the original SDK MCP pipe. Only the
//! native peer and Matrix reads are fixtures; the host executes the real reader.
use super::*;

fn read(input: &mut impl BufRead) -> io::Result<Value> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
fn ack(request: &Value, body: Value) -> io::Result<()> {
    emit(
        json!({"type":"control_response","response":{"subtype":"success",
        "request_id":request["request_id"],"response":body}}),
    )
}
fn mcp(input: &mut impl BufRead, id: &str, message: Value) -> io::Result<Value> {
    emit(json!({"type":"control_request","request_id":id,"request":{
        "subtype":"mcp_message","server_name":"hagency_matrix","message":message}}))?;
    let reply = read(input)?;
    if reply["type"] != "control_response"
        || reply["response"]["request_id"] != id
        || reply["response"]["subtype"] != "success"
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(reply["response"]["response"]["mcp_response"].clone())
}

pub(super) fn bind_and_prompt(input: &mut impl BufRead, status: Value) -> io::Result<Value> {
    if status["request"]["subtype"] != "mcp_status" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    ack(&status, json!({"mcpServers":[]}))?;
    let bind = read(input)?;
    if bind["request"]["subtype"] != "mcp_set_servers"
        || bind["request"]["servers"]["hagency_matrix"]["type"] != "sdk"
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    mcp(
        input,
        "metadata-init",
        json!({"jsonrpc":"2.0","id":0,"method":"initialize",
        "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{
            "name":"offline-history","version":"1"}}}),
    )?;
    mcp(
        input,
        "metadata-ready",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )?;
    ack(
        &bind,
        json!({"added":["hagency_matrix"],"removed":[],"errors":{}}),
    )?;
    let status = read(input)?;
    if status["request"]["subtype"] != "mcp_status" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    ack(
        &status,
        json!({"mcpServers":[{
        "name":"hagency_matrix","status":"connected","transport":"sdk","scope":"dynamic",
        "source":"sdk","tools":[{"name":"read_room_history"}]}]}),
    )?;
    read(input)
}

pub(super) fn read_all(input: &mut impl BufRead, marker: &Path) -> io::Result<()> {
    let mut cursor = Value::Null;
    let mut pages = Vec::new();
    for n in 0..128 {
        let id = format!("history-{n}");
        let reply = mcp(
            input,
            &id,
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"read_room_history","arguments":{"cursor":cursor},
                "_meta":{"claudecode/toolUseId":format!("tool-{id}")}}}),
        )?;
        if reply["result"]["isError"] != false {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let raw = reply["result"]["content"][0]["text"]
            .as_str()
            .ok_or(io::ErrorKind::InvalidData)?;
        let page: Value = serde_json::from_str(raw)?;
        if raw.len() > hagency_runtime::room_tools::ROOM_HISTORY_RESULT_BYTES
            || page.get("reason").is_some()
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        cursor = page["nextCursor"].clone();
        let complete = page["complete"] == true;
        pages.push(page);
        if cursor.is_null() {
            if !complete {
                return Err(io::ErrorKind::InvalidData.into());
            }
            fs::write(
                marker.with_extension("history"),
                serde_json::to_vec(&pages)?,
            )?;
            return Ok(());
        }
    }
    Err(io::ErrorKind::InvalidData.into())
}
