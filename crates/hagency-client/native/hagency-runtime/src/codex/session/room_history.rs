//! The single host-owned, read-only conversation tool. No model-supplied room,
//! credentials, URL or filesystem path can cross this interface.
use super::{Error, valid_id};
use crate::codex::RequestId;
use serde_json::Value;

pub use crate::room_tools::{ROOM_HISTORY_TOOL, ROOM_HISTORY_RESULT_BYTES};

#[derive(Clone)]
pub struct RoomHistoryCall {
    pub(crate) id: RequestId,
    pub(crate) thread: String,
    pub(crate) turn: String,
    pub(super) root: bool,
    pub cursor: Option<String>,
}

impl RoomHistoryCall {
    pub fn thread_id(&self) -> &str {
        &self.thread
    }
    pub fn turn_id(&self) -> &str {
        &self.turn
    }
    pub fn is_root(&self) -> bool {
        self.root
    }
    pub(super) fn parse(id: RequestId, params: &Value) -> Result<Self, Error> {
        if params["tool"] != ROOM_HISTORY_TOOL
            || params.get("namespace").is_some_and(|v| !v.is_null())
        {
            return Err(Error::UnsupportedRequest);
        }
        valid_id(params["callId"].as_str().ok_or(Error::Malformed)?)?;
        let cursor = crate::room_tools::history_cursor(&params["arguments"])
            .map_err(|_| Error::Malformed)?;
        let thread = params["threadId"].as_str().ok_or(Error::Scope)?;
        let turn = params["turnId"].as_str().ok_or(Error::Scope)?;
        valid_id(thread)?;
        valid_id(turn)?;
        Ok(Self {
            id,
            thread: thread.into(),
            turn: turn.into(),
            root: false,
            cursor,
        })
    }
}

pub(super) fn definition() -> Value {
    let mut tool = crate::room_tools::history_definition();
    tool["type"] = "function".into();
    tool
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn room_history_tool_rejects_room_substitution_and_arbitrary_arguments() {
        let mut p = json!({"threadId":"thread","turnId":"turn","tool":ROOM_HISTORY_TOOL,"callId":"call_1","arguments":{"cursor":null}});
        assert!(RoomHistoryCall::parse(RequestId::Number(1), &p).is_ok());
        p["arguments"]["roomId"] = "!other:example.test".into();
        assert!(RoomHistoryCall::parse(RequestId::Number(1), &p).is_err());
        p["arguments"] = json!({"cursor":null});
        p["tool"] = "send_message".into();
        assert!(RoomHistoryCall::parse(RequestId::Number(1), &p).is_err());
    }
}
