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
    /// Push freely authorized it (ADR 0022, "Codex"); checked again, as
    /// this workspace-rooted proposal, right before the answer is sent.
    Trusted(Box<ToolProposal>),
    /// The auto-merge gate answered (and, when allowing, authorized) it,
    /// exactly as it answers a Claude `Bash` call.
    Gate(bool),
    Owner(bool),
    /// The card could not be shown, expired or was withdrawn: nobody refused.
    Unanswered,
    Deny,
}
type Wait = Pin<Box<dyn Future<Output = Decision> + Send>>;
struct Pending {
    request: Request,
    wait: Wait,
}
/// The pending waits, and the command items the gate answered this turn
/// (allowed or not): a command the gate decided is never observed again.
#[derive(Default)]
pub(super) struct Approvals(BTreeMap<String, Pending>, std::collections::HashSet<String>);
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
                // Dropped with the wait: answered, expired or withdrawn.
                let waiting = if sent { queue.show_approval_wait() } else { None };
                Box::pin(async move {
                    let _waiting = waiting;
                    if !sent {
                        return Decision::Unanswered;
                    }
                    match tokio::time::timeout_at(until, receiver).await {
                        Ok(Ok(answer)) => Decision::Owner(answer),
                        Ok(Err(_)) | Err(_) => Decision::Unanswered,
                    }
                })
            }
            _ => Box::pin(std::future::ready(Decision::Deny)),
        };
        self.0
            .insert(request.id.to_string(), Pending { request, wait });
    }
    /// A request push trust answered: accepted as soon as it is polled.
    pub fn offer_trusted(&mut self, request: Request, rooted: ToolProposal) {
        let wait: Wait = Box::pin(std::future::ready(Decision::Trusted(Box::new(rooted))));
        self.0.insert(request.id.to_string(), Pending { request, wait });
    }
    /// A request the auto-merge gate answered: sent as soon as it is polled.
    pub fn offer_gated(&mut self, request: Request, allowed: bool) {
        self.1.extend(command_key(&request));
        let wait: Wait = Box::pin(std::future::ready(Decision::Gate(allowed)));
        self.0.insert(request.id.to_string(), Pending { request, wait });
    }
    /// Whether the gate answered the command item `key` this turn.
    pub fn answered_by_gate(&self, key: &str) -> bool {
        self.1.contains(key)
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
    /// The held-review key of a command the gate allowed, to note once its
    /// accept is sent ([`crate::merge_policy::codex_review::settle`]).
    pub fn gated_call(&self) -> Option<String> {
        command_key(&self.request).filter(|_| matches!(self.decision, Decision::Gate(true)))
    }
    /// A command a saved rule or the owner's card answered: its key and the
    /// exact proposal decided (its real directory and risk), noted once the
    /// accept is sent so a review it posts can be recorded.
    pub fn decided_call(&self) -> Option<(String, ToolProposal)> {
        // A nested execve approval is not the enclosing command's (GPT review).
        let decided = matches!(self.decision, Decision::Rules | Decision::Owner(true)) && self.request.proposal.arguments["approvalId"].is_null();
        command_key(&self.request).filter(|_| decided).map(|key| (key, self.request.proposal.clone()))
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
            // The owner may have paused the watch since (GPT review).
            Decision::Trusted(rooted) => matches!(
                ledger.trusted_publish(owner, &rooted, super::now()),
                Ok(Some(crate::watch_trust::TrustedStep::Allow))
            ),
            Decision::Gate(allowed) => allowed,
            Decision::Unanswered => {
                if let Err(error) = ledger.note_unanswered_approval(&request.proposal.dispatch) {
                    // Only the issue loop's attempt count reads the mark: without
                    // it the run counts as an ended attempt, as before this fix.
                    eprintln!("issue hand-off unanswered-approval mark not saved: {error}");
                }
                false
            }
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

/// The review key of a shell-command request (`None` for other requests or without an item id).
fn command_key(request: &Request) -> Option<String> {
    let item = request.proposal.arguments["itemId"].as_str().filter(|item| !item.is_empty())?;
    (request.method == crate::merge_policy::codex_review::COMMAND_APPROVAL)
        .then(|| crate::merge_policy::codex_review::call_key(&request.thread, &request.turn, item))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;
    use hagency_runtime::live_text::{APPROVAL_WAIT_STEP, LiveText};

    fn request() -> Request {
        let proposal = ToolProposal {
            scope: Scope { agent: "bird".into(), binding: "b".into(), room: "!dev:x".into(), requester: "@anna:x".into(), thread: "t".into() },
            dispatch: "d".into(), tool: "codex.commandExecution".into(), arguments: json!({"command": "gh issue close 1"}),
            canonical_directory: "/tmp".into(), risk: "high".into(), policy_revision: [1, 1, 1], expires: 100,
        };
        Request { id: json!(7), method: "item/commandExecution/requestApproval".into(), thread: "th".into(), turn: "tu".into(), native: None, proposal }
    }

    /// The requester's flow: Anna's request needs Betty's card; while it
    /// waits, the run card's step says so, and it goes back once Betty answers.
    #[tokio::test]
    async fn a_run_waiting_for_its_owner_says_so_on_its_card() {
        let text = LiveText::default();
        text.step("running gh issue close 1");
        let (queue, mut cards) = crate::codex::approval_queue(1).unwrap();
        let queue = queue.with_live_text(text.clone());
        let mut approvals = Approvals::default();
        approvals.offer(request(), Some(ToolPolicy::AskOwner), &queue);
        assert_eq!(text.current_step().0, APPROVAL_WAIT_STEP);
        cards.recv().await.unwrap().decide(true);
        let answer = approvals.next().await;
        assert!(matches!(answer.decision, Decision::Owner(true)));
        assert_eq!(text.current_step(), ("running gh issue close 1".into(), 1));
        // A withdrawn request ends the wait too.
        approvals.offer(request(), Some(ToolPolicy::AskOwner), &queue);
        approvals.clear();
        assert_eq!(text.current_step().0, "running gh issue close 1");
    }

    /// A card that ended unanswered is told apart from a refusal, so an issue
    /// hand-off's run is not charged to the Agent (e2e, Nov 17 23:32).
    #[tokio::test]
    async fn a_card_nobody_answered_is_unanswered_not_refused() {
        let (queue, mut cards) = crate::codex::approval_queue(1).unwrap();
        let mut approvals = Approvals::default();
        approvals.offer(request(), Some(ToolPolicy::AskOwner), &queue);
        drop(cards.recv().await.unwrap()); // the card went away unanswered
        assert!(matches!(approvals.next().await.decision, Decision::Unanswered));
        approvals.offer(request(), Some(ToolPolicy::AskOwner), &queue);
        cards.recv().await.unwrap().decide(false);
        assert!(matches!(approvals.next().await.decision, Decision::Owner(false)), "a refusal stays a refusal");
    }

    /// GPT review: a rule's or the owner's accept is noted for observation only
    /// for the command itself, never for a nested execve approval; every gate
    /// answer (allowed or not) marks its item as the gate's.
    #[tokio::test]
    async fn only_a_top_level_decision_is_noted_and_any_gate_answer_marks_its_item() {
        let (queue, _cards) = crate::codex::approval_queue(1).unwrap();
        let mut approvals = Approvals::default();
        let mut top = request();
        top.proposal.arguments["itemId"] = json!("exec-1");
        let mut nested = Request { id: json!(8), native: None, method: top.method.clone(), thread: top.thread.clone(), turn: top.turn.clone(), proposal: top.proposal.clone() };
        nested.proposal.arguments["approvalId"] = json!("execve-1");
        approvals.offer(top, Some(ToolPolicy::AllowWithRules { tools: vec![], directories: vec![] }), &queue);
        assert!(approvals.next().await.decided_call().is_some());
        approvals.offer(nested, Some(ToolPolicy::AllowWithRules { tools: vec![], directories: vec![] }), &queue);
        assert!(approvals.next().await.decided_call().is_none());
        let mut refused = request();
        refused.proposal.arguments["itemId"] = json!("exec-2");
        approvals.offer_gated(refused, false);
        assert!(approvals.answered_by_gate(&crate::merge_policy::codex_review::call_key("th", "tu", "exec-2")));
    }
}
