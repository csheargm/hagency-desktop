use super::{ApprovalQueue, Error, Result};
use crate::{Scope, questions::SubmittedAnswer};
use hagency_runtime::codex::{RequestId, session::UserInputRequest, tree::TurnTree};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};

type Wait = Pin<Box<dyn Future<Output = Option<SubmittedAnswer>> + Send>>;
struct Pending {
    request: UserInputRequest,
    wait: Wait,
}
#[derive(Default)]
pub(super) struct Questions(BTreeMap<RequestId, Pending>);
pub(super) struct Answer {
    pub request: UserInputRequest,
    pub submitted: Option<SubmittedAnswer>,
}
impl Questions {
    pub fn ask(
        &mut self,
        request: UserInputRequest,
        scope: &Scope,
        dispatch: &str,
        queue: &ApprovalQueue,
        enabled: bool,
    ) -> Result<()> {
        if self.0.len() >= 8 {
            return Err(Error::Protocol("concurrent question capacity"));
        }
        let allowed = enabled && !request.is_codex_approval();
        let channel = queue.questions.clone().filter(|_| allowed);
        let scope = scope.clone();
        let dispatch = dispatch.to_owned();
        let id = serde_json::to_string(request.id())?;
        let data = request.host_questions();
        let wait = Box::pin(async move {
            let queue = channel?;
            queue
                .ask(
                    &scope,
                    &dispatch,
                    id,
                    data,
                    tokio::time::Instant::now() + Duration::from_secs(600),
                )
                .await
        });
        self.0
            .insert(request.id().clone(), Pending { request, wait });
        Ok(())
    }
    pub async fn next(&mut self) -> Answer {
        let (id, submitted) = poll_fn(|cx| {
            for (id, pending) in &mut self.0 {
                if let Poll::Ready(answer) = pending.wait.as_mut().poll(cx) {
                    return Poll::Ready((id.clone(), answer));
                }
            }
            Poll::Pending
        })
        .await;
        let pending = self.0.remove(&id).expect("polled original question");
        Answer {
            request: pending.request,
            submitted,
        }
    }
    pub fn withdraw(&mut self, params: &Value) {
        let Some(id) = params
            .get("requestId")
            .and_then(|v| serde_json::from_value::<RequestId>(v.clone()).ok())
        else {
            return;
        };
        if self
            .0
            .get(&id)
            .is_some_and(|p| params["threadId"] == p.request.thread_id())
        {
            self.0.remove(&id);
        }
    }
    pub fn retain_active(&mut self, tree: &TurnTree) {
        self.0
            .retain(|_, p| tree.is_active(p.request.thread_id(), p.request.turn_id()));
    }
}
impl Answer {
    pub fn withdrawn(&self, params: &Value) -> bool {
        params["threadId"] == self.request.thread_id()
            && serde_json::from_value::<RequestId>(params["requestId"].clone())
                .ok()
                .as_ref()
                == Some(self.request.id())
    }
}
