//! Owner waits never own the native reader. Cancellation and terminal events
//! can retire their exact callback while an owner is still deciding.
use super::{ApprovalQueue, Ledger, PendingApproval, ToolPolicy, ToolProposal};
use hagency_runtime::codex::{approval::ApprovalRequest, tree::TurnTree};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::sync::oneshot;

pub(super) struct Request {
    pub id: Value,
    pub method: String,
    pub thread: String,
    pub turn: String,
    pub native: Option<ApprovalRequest>,
    pub proposal: ToolProposal,
}
enum Decision {
    Rules,
    Owner(bool),
    Deny,
}
type Wait = Pin<Box<dyn Future<Output = Decision> + Send>>;
struct Pending {
    request: Request,
    wait: Wait,
}
#[derive(Default)]
pub(super) struct Approvals(BTreeMap<String, Pending>);
pub(super) struct Answer {
    request: Request,
    decision: Decision,
}
impl Approvals {
    pub fn full(&self) -> bool {
        self.0.len() >= 16
    }
    pub fn offer(
        &mut self,
        request: Request,
        disposition: Option<ToolPolicy>,
        queue: &ApprovalQueue,
    ) {
        let wait: Wait = match disposition {
            Some(ToolPolicy::AllowWithRules { .. }) => {
                Box::pin(std::future::ready(Decision::Rules))
            }
            Some(ToolPolicy::AskOwner) => {
                let (decision, receiver) = oneshot::channel();
                let sent = queue
                    .sender
                    .try_send(PendingApproval {
                        proposal: request.proposal.clone(),
                        decision,
                    })
                    .is_ok();
                let until = tokio::time::Instant::now() + Duration::from_secs(300);
                Box::pin(async move {
                    Decision::Owner(
                        sent && matches!(
                            tokio::time::timeout_at(until, receiver).await,
                            Ok(Ok(true))
                        ),
                    )
                })
            }
            _ => Box::pin(std::future::ready(Decision::Deny)),
        };
        self.0
            .insert(request.id.to_string(), Pending { request, wait });
    }
    pub async fn next(&mut self) -> Answer {
        let (id, decision) = poll_fn(|cx| {
            for (id, pending) in &mut self.0 {
                if let Poll::Ready(decision) = pending.wait.as_mut().poll(cx) {
                    return Poll::Ready((id.clone(), decision));
                }
            }
            Poll::Pending
        })
        .await;
        let pending = self.0.remove(&id).expect("original approval wait");
        Answer {
            request: pending.request,
            decision,
        }
    }
    pub fn withdraw(&mut self, params: &Value) {
        self.0
            .retain(|_, pending| !pending.request.withdrawn(params));
    }
    pub fn retain_active(&mut self, tree: &TurnTree) {
        self.0
            .retain(|_, pending| tree.is_active(&pending.request.thread, &pending.request.turn));
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
impl Request {
    fn withdrawn(&self, params: &Value) -> bool {
        params["threadId"] == self.thread && params["requestId"] == self.id
    }
}
impl Answer {
    pub fn active(&self, tree: &TurnTree) -> bool {
        tree.is_active(&self.request.thread, &self.request.turn)
    }
    pub fn withdrawn(&self, params: &Value) -> bool {
        self.request.withdrawn(params)
    }
    pub fn response(self, ledger: &mut Ledger, owner: &str) -> Value {
        let request = self.request;
        let allow = match self.decision {
            Decision::Rules => ledger
                .authorize_tool(&request.proposal, super::now())
                .is_ok(),
            Decision::Owner(true) => ledger
                .approve_tool(owner, &request.proposal, super::now())
                .and_then(|_| ledger.authorize_tool(&request.proposal, super::now()))
                .is_ok(),
            Decision::Owner(false) | Decision::Deny => false,
        };
        let result = if let Some(native) = request.native {
            native.response(allow).wire_result().clone()
        } else if request.method == "mcpServer/elicitation/request" {
            json!({"action":if allow {"accept"} else {"decline"},"content":null,"_meta":null})
        } else {
            json!({"decision":if allow {"accept"} else {"decline"}})
        };
        json!({"id":request.id,"result":result})
    }
}
