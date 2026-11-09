//! One admitted dispatch's Matrix IO. Native runtimes supply operations only;
//! the host owns room identity, authorization, paging and durable file receipts.
use crate::{
    Scope,
    codex::{Error, Result},
};
use hagency_runtime::room_tools::{RoomOperation, ROOM_HISTORY_RESULT_BYTES};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

#[derive(Clone)]
pub struct RoomQueue {
    scope: Scope,
    dispatch: String,
    sender: mpsc::Sender<PendingRoomCall>,
}

/// No room/owner/workspace supplied by the model is accepted by this channel.
pub struct PendingRoomCall {
    pub operation: RoomOperation,
    pub context: Option<hagency_runtime::codex::session::ContextUsage>,
    pub native_call_id: String,
    response: oneshot::Sender<RoomReply>,
    deadline: Instant,
}
pub struct RoomReply {
    text: String,
    is_error: bool,
}
impl RoomReply {
    pub fn new(text: String, is_error: bool) -> Result<Self> {
        if text.len() > ROOM_HISTORY_RESULT_BYTES {
            return Err(Error::Protocol("room tool result too large"));
        }
        Ok(Self { text, is_error })
    }
    pub fn unavailable() -> Self {
        Self {
            text: "{\"error\":\"room_tool_unavailable\"}".into(),
            is_error: true,
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn is_error(&self) -> bool {
        self.is_error
    }
}
impl PendingRoomCall {
    pub fn is_closed(&self) -> bool {
        self.response.is_closed() || Instant::now() >= self.deadline
    }
    pub async fn closed(&mut self) {
        let _ = tokio::time::timeout_at(self.deadline, self.response.closed()).await;
    }
    pub fn reply(self, reply: RoomReply) {
        if !self.is_closed() {
            let _ = self.response.send(reply);
        }
    }
}
pub fn room_queue(
    scope: &Scope,
    dispatch: &str,
) -> Result<(RoomQueue, mpsc::Receiver<PendingRoomCall>)> {
    scope.validate()?;
    crate::key(dispatch)?;
    let (sender, receiver) = mpsc::channel(16);
    Ok((
        RoomQueue {
            scope: scope.clone(),
            dispatch: dispatch.into(),
            sender,
        },
        receiver,
    ))
}
impl RoomQueue {
    pub(crate) fn matches(&self, scope: &Scope, dispatch: &str) -> bool {
        &self.scope == scope && self.dispatch == dispatch
    }
    pub(crate) async fn call(
        &self,
        operation: RoomOperation,
        native_call_id: String,
        deadline: Instant,
    ) -> RoomReply {
        self.call_with_context(operation, native_call_id, deadline, None)
            .await
    }
    pub(crate) async fn call_with_context(
        &self,
        operation: RoomOperation,
        native_call_id: String,
        deadline: Instant,
        context: Option<hagency_runtime::codex::session::ContextUsage>,
    ) -> RoomReply {
        let (response, receive) = oneshot::channel();
        if Instant::now() >= deadline
            || self
                .sender
                .try_send(PendingRoomCall {
                    operation,
                    context,
                    native_call_id,
                    response,
                    deadline,
                })
                .is_err()
        {
            return RoomReply::unavailable();
        }
        tokio::time::timeout_at(deadline, receive)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(RoomReply::unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_native_wait_withdraws_host_io_and_never_rebinds() {
        let scope = Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:test".into(),
            requester: "@owner:test".into(),
            thread: "!room:test".into(),
        };
        let (queue, mut receiver) = room_queue(&scope, "dispatch").unwrap();
        assert!(queue.matches(&scope, "dispatch"));
        assert!(!queue.matches(&scope, "later"));
        let task = tokio::spawn(async move {
            queue
                .call(
                    RoomOperation::History { cursor: None },
                    "native".into(),
                    Instant::now() + std::time::Duration::from_secs(60),
                )
                .await
        });
        let mut pending = receiver.recv().await.unwrap();
        assert!(!pending.is_closed());
        task.abort();
        pending.closed().await;
        assert!(pending.is_closed());
        assert!(RoomReply::new("x".repeat(ROOM_HISTORY_RESULT_BYTES + 1), false).is_err());
    }
}
