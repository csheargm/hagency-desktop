//! The same scoped host-file capability used by Codex, transported over OUP.
//! Octos keeps its native workspace read-only; file mutation goes through the
//! host's owner policy, fresh dispatch lease, and exact prepared operation.
use super::*;
use crate::room_files::{FileError, FileResult};
use serde_json::json;
use std::collections::VecDeque;

type Buffered = VecDeque<(octos::session::Event, Option<octos::session::Observation>)>;

pub(super) fn definitions(files: &OctosHostFiles) -> Result<Vec<octos::session::HostTool>> {
    let specs = codex::host_files::specifications(files.workspace.is_shared());
    specs
        .as_array()
        .ok_or(Error::Unknown)?
        .iter()
        .map(|spec| {
            let name = spec["name"]
                .as_str()
                .and_then(|s| s.strip_prefix("hagency_"))
                .ok_or(Error::Unknown)?;
            Ok(octos::session::HostTool {
                name: format!("hagency.{name}"),
                description: spec["description"].as_str().ok_or(Error::Unknown)?.into(),
                input_schema: spec["inputSchema"].clone(),
                read_only: matches!(name, "file_list" | "file_read"),
            })
        })
        .collect()
}

pub(super) fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "hagency.file_list"
            | "hagency.file_read"
            | "hagency.file_create"
            | "hagency.directory_create"
            | "hagency.file_update"
    )
}

fn result(
    outcome: std::result::Result<FileResult, FileError>,
) -> Result<std::result::Result<Value, String>> {
    match outcome {
        Ok(value) => Ok(Ok(serde_json::to_value(value).map_err(|_| Error::Unknown)?)),
        Err(FileError::Unknown) => Err(Error::Unknown),
        Err(FileError::NeedsOwner | FileError::Denied) => Ok(Err("host_file_denied".into())),
        Err(FileError::Conflict) => Ok(Err("file_exists_or_changed".into())),
        Err(FileError::Capacity) => Ok(Err("file_capacity_exceeded".into())),
        Err(_) => Ok(Err("host_file_unavailable".into())),
    }
}

async fn reply(
    wire: &mut Octos,
    call: &str,
    value: std::result::Result<Value, String>,
) -> Result<()> {
    wire.host_tool_result(call, value)
        .await
        .map_err(|_| Error::Protocol("Octos file tool response"))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle(
    wire: &mut Octos,
    files: &mut OctosHostFiles,
    ledger: &mut Ledger,
    owner: &str,
    scope: &Scope,
    execution: &str,
    dispatch: &str,
    session: &str,
    turn: &str,
    call: &octos::session::HostToolCall,
    queue: &ApprovalQueue,
    controls: &mut Option<crate::run_controls::Stream>,
    buffered: &mut Buffered,
    stopping: &mut bool,
    steers: &mut OctosSteers,
) -> Result<()> {
    if *stopping || !wire.host_tool_pending(&call.call_id) {
        return Ok(());
    }

    // The gate is scoped to the exact current execution and is checked again
    // after an owner decision, immediately before any host operation.
    let gate = files.gate.clone();
    let permitted =
        tokio::time::timeout(Duration::from_secs(5), gate.authorize(dispatch, execution));
    tokio::pin!(permitted);
    let permitted = octos_wait_tool(
        wire,
        permitted.as_mut(),
        controls,
        buffered,
        stopping,
        steers,
        &call.call_id,
    )
    .await?;
    match permitted {
        None => return Ok(()),
        Some(Ok(true)) => {}
        _ => return reply(wire, &call.call_id, Err("server_scope_unavailable".into())).await,
    }

    let name = call.name.strip_prefix("hagency.").ok_or(Error::Unknown)?;
    let params =
        json!({"tool":format!("hagency_{name}"),"arguments":call.args,"callId":call.call_id});
    let operation = match codex::host_files::operation(&params, owner, scope, dispatch) {
        Ok(operation) => operation,
        Err(_) => return reply(wire, &call.call_id, Err("invalid_host_file_request".into())).await,
    };
    let prepared = match files.workspace.prepare(
        ledger,
        scope,
        dispatch,
        session,
        turn,
        &call.call_id,
        operation,
        now(),
    ) {
        Ok(prepared) => prepared,
        Err(_) => return reply(wire, &call.call_id, Err("invalid_host_file_request".into())).await,
    };
    let outcome = files.workspace.execute(ledger, &prepared, now());
    if !matches!(outcome, Err(FileError::NeedsOwner)) {
        return reply(wire, &call.call_id, result(outcome)?).await;
    }

    let (pending, decision) = codex::pending_approval(prepared.proposal().clone());
    if !queue.offer(pending) {
        return reply(wire, &call.call_id, Err("host_file_denied".into())).await;
    }
    // Octos's host-tool timeout is 60 seconds. Leave time to recheck the
    // server and return the result before Octos cancels the call.
    let decision = tokio::time::timeout(Duration::from_secs(45), decision);
    tokio::pin!(decision);
    let decision = octos_wait_tool(
        wire,
        decision.as_mut(),
        controls,
        buffered,
        stopping,
        steers,
        &call.call_id,
    )
    .await?;
    match decision {
        None => return Ok(()),
        Some(Ok(Ok(true))) => {}
        _ => return reply(wire, &call.call_id, Err("host_file_denied".into())).await,
    }

    let permitted =
        tokio::time::timeout(Duration::from_secs(5), gate.authorize(dispatch, execution));
    tokio::pin!(permitted);
    let permitted = octos_wait_tool(
        wire,
        permitted.as_mut(),
        controls,
        buffered,
        stopping,
        steers,
        &call.call_id,
    )
    .await?;
    match permitted {
        None => return Ok(()),
        Some(Ok(true)) => {}
        _ => return reply(wire, &call.call_id, Err("server_scope_unavailable".into())).await,
    }
    if ledger
        .approve_tool(owner, prepared.proposal(), now())
        .is_err()
    {
        return reply(wire, &call.call_id, Err("host_file_denied".into())).await;
    }
    let outcome = files.workspace.execute(ledger, &prepared, now());
    reply(wire, &call.call_id, result(outcome)?).await
}
