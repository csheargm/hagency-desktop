//! Codex envelope adapter for the common host-bound room file operations.
use super::{Error, valid_id};
use crate::codex::RequestId;
use serde_json::{Value, json};
pub use crate::room_tools::{RoomFileOperation, ROOM_FILE_TOOLS, ROOM_FILE_RESULT_BYTES};

#[derive(Clone)]
pub struct RoomFileCall {
    pub(crate) id: RequestId,
    pub(crate) thread: String,
    pub(crate) turn: String,
    pub operation: RoomFileOperation,
}
impl RoomFileCall {
    pub(super) fn parse(id: RequestId, params: &Value) -> Result<Self, Error> {
        if params.get("namespace").is_some_and(|v| !v.is_null()) {
            return Err(Error::UnsupportedRequest);
        }
        valid_id(params["callId"].as_str().ok_or(Error::Malformed)?)?;
        let thread = params["threadId"]
            .as_str()
            .ok_or(Error::Malformed)?
            .to_owned();
        let turn = params["turnId"]
            .as_str()
            .ok_or(Error::Malformed)?
            .to_owned();
        valid_id(&thread)?;
        valid_id(&turn)?;
        let operation: RoomFileOperation =
            serde_json::from_value(json!({"tool":params["tool"],"arguments":params["arguments"]}))
                .map_err(|_| Error::Malformed)?;
        operation.validate().map_err(|_| Error::Malformed)?;
        Ok(Self {
            id,
            thread,
            turn,
            operation,
        })
    }
}

pub(super) fn definitions() -> Vec<Value> {
    crate::room_tools::file_definitions()
        .into_iter()
        .map(|mut tool| {
            tool["type"] = "function".into();
            tool
        })
        .collect()
}
