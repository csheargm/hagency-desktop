//! Matrix channels carry host-scoped inputs; original Claude observations alone
//! settle a call. Questions never grant permission to execute a native tool.
use super::*;
use crate::{
    agent_run_controls::{Action as MatrixAction, Card, State},
    console::owned_runtime::run_controls::CardHost,
};
use hagency_agent_local::{
    codex,
    questions::PendingQuestion,
    room_tools::{PendingRoomCall, RoomReply},
    run_controls as controls,
};
use hagency_runtime::{owned::SharedClaudePolicy, room_tools::RoomOperation};
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct Host {
    live: Live,
    policy: SharedClaudePolicy,
    pending: Option<SharedCallCustody>,
    send: Option<oneshot::Sender<SharedCallCustody>>,
    session: Option<String>,
    work: SharedWorkItem,
}
pub(super) struct Capture {
    receive: Option<oneshot::Receiver<SharedCallCustody>>,
    sink: Option<SharedCallSink>,
    receipt: Option<SharedWorkReceipt>,
    native_session: Option<SharedClaudeSession>,
    progress: watch::Sender<tokio::time::Instant>,
}
pub(super) fn capture(
    live: Live,
    policy: SharedClaudePolicy,
    pending: SharedCallCustody,
    native_session: SharedClaudeSession,
) -> (Host, Capture) {
    let work = pending.work().clone();
    let (send, receive) = oneshot::channel();
    (
        Host {
            live,
            policy,
            pending: Some(pending),
            send: Some(send),
            session: None,
            work,
        },
        Capture {
            receive: Some(receive),
            sink: None,
            receipt: None,
            native_session: Some(native_session),
            progress: watch::channel(tokio::time::Instant::now()).0,
        },
    )
}
impl claude_turn::Host for Host {
    async fn start(&mut self, process: &mut OwnedClaudeSession, input: &str) -> codex::Result<()> {
        // Keep sole custody on Host while the start future is pending. If that
        // future is canceled, finish() can still persist its original outcome.
        let custody = self.pending.as_ref().ok_or(codex::Error::Unknown)?;
        if input != custody.model_input() {
            return Err(codex::Error::Protocol("shared Claude input differs"));
        }
        custody
            .start_claude_turn(process)
            .await
            .map_err(|_| codex::Error::Unknown)?;
        let custody = self.pending.take().ok_or(codex::Error::Unknown)?;
        if let Err(custody) = self.send.take().ok_or(codex::Error::Unknown)?.send(custody) {
            self.pending = Some(custody);
            return Err(codex::Error::Unknown);
        }
        Ok(())
    }
    fn bind_session(&mut self, session: &str) -> codex::Result<()> {
        if session.is_empty() || session.len() > 255 || self.session.is_some() {
            return Err(codex::Error::Unknown);
        }
        self.session = Some(session.into());
        Ok(())
    }
    async fn permission(
        &mut self,
        id: &str,
        tool: String,
        arguments: Value,
        _directory: &Path,
    ) -> claude_turn::ToolAnswer {
        use claude_turn::ToolAnswer;
        let tool = tool.strip_prefix("claude.").unwrap_or(&tool);
        // These modes come from the resource owner's signed successor policy.
        // Every refusal is logged with codes and the tool's name only.
        if let Err(e) = self
            .live
            .io
            .check_running(self.live.authority.root())
            .await
        {
            tracing::warn!(tool, status = e.status, code = %e.code, "Shared Claude tool request refused: the run's authority check failed");
            return ToolAnswer::Deny;
        }
        let web_tool = matches!(tool, "WebSearch" | "WebFetch");
        let refused = |reason: &'static str| {
            tracing::info!(tool, reason, "Shared Claude tool request refused by the pool's policy");
            ToolAnswer::Deny
        };
        match self.policy {
            SharedClaudePolicy::Text => refused("text_only"),
            SharedClaudePolicy::LiveWeb if !web_tool => refused("web_only"),
            SharedClaudePolicy::LiveWeb => ToolAnswer::allowed(true),
            SharedClaudePolicy::Yolo { web } if !web && web_tool => refused("web_off"),
            SharedClaudePolicy::Yolo { .. } => ToolAnswer::allowed(true),
            SharedClaudePolicy::Standard { web } => {
                if !web && web_tool {
                    return refused("web_off");
                }
                let Some(session) = &self.session else {
                    tracing::warn!(tool, "Shared Claude tool request refused: no native session is bound yet");
                    return ToolAnswer::Deny;
                };
                // The person who sent the message decides. The whole CLI runs
                // in the sandbox that keeps it writing only in its own folder.
                let request = json!({"id":id,"method":"claude/can_use_tool","params":{"sessionId":session,"toolName":tool,"input":arguments},"sessionAllowed":false,"managerCanDecide":true});
                match tokio::time::timeout(
                    Duration::from_secs(120),
                    permission(&self.live, &self.work, request),
                )
                .await
                {
                    // The person who asked never answered: nobody refused.
                    // The server ends its own record a little before this
                    // wait does and then answers the next read with
                    // `approval_expired`.
                    Err(_) => {
                        tracing::info!("Shared Claude tool request was not answered in time");
                        ToolAnswer::Unanswered
                    }
                    Ok(Err(e)) if e.code == "approval_expired" => {
                        tracing::info!("Shared Claude tool request expired unanswered");
                        ToolAnswer::Unanswered
                    }
                    // Codes only: never the tool input or a Matrix ID.
                    Ok(Err(e)) => {
                        tracing::warn!(status = e.status, code = %e.code, "Shared Claude tool request failed and was refused");
                        ToolAnswer::Deny
                    }
                    Ok(Ok(allowed)) => ToolAnswer::allowed(allowed),
                }
            }
        }
    }
}
impl claude_turn::Capture for Capture {
    async fn bind(&mut self, process: &OwnedClaudeSession) -> codex::Result<()> {
        self.progress.send_replace(tokio::time::Instant::now());
        let custody = self
            .receive
            .take()
            .ok_or(codex::Error::Unknown)?
            .await
            .map_err(|_| codex::Error::Unknown)?;
        self.sink = Some(
            custody
                .bind_started_claude_turn(process)
                .await
                .map_err(|_| codex::Error::Unknown)?,
        );
        self.native_session
            .as_ref()
            .ok_or(codex::Error::Unknown)?
            .bind_capture(self.sink.as_mut().ok_or(codex::Error::Unknown)?, process)
            .map_err(|_| codex::Error::Unknown)?;
        Ok(())
    }
    async fn observe(
        &mut self,
        observation: &hagency_runtime::claude::session::Observation,
    ) -> codex::Result<()> {
        self.progress.send_replace(tokio::time::Instant::now());
        if let Some(receipt) = self
            .sink
            .as_mut()
            .ok_or(codex::Error::Unknown)?
            .observe_claude(observation)
            .await
            .map_err(|_| codex::Error::Unknown)?
        {
            self.receipt = Some(receipt);
        }
        Ok(())
    }
    async fn complete(&mut self, process: &OwnedClaudeSession, text: &str) -> codex::Result<()> {
        self.receipt = Some(
            self.sink
                .as_mut()
                .ok_or(codex::Error::Unknown)?
                .finish_claude(process, text)
                .await
                .map_err(|_| codex::Error::Unknown)?,
        );
        if let Some(session) = self.native_session.take()
            && session
                .retain(self.sink.as_ref().ok_or(codex::Error::Unknown)?, process)
                .await
                .is_err()
        {
            tracing::warn!("Shared Claude completed without a reusable native checkpoint");
        }
        Ok(())
    }
    async fn complete_unaccounted(
        &mut self,
        receipt: &hagency_runtime::claude::session::CompletionReceipt,
        text: &str,
    ) -> codex::Result<()> {
        self.receipt = Some(
            self.sink
                .as_mut()
                .ok_or(codex::Error::Unknown)?
                .finish_claude_unaccounted(receipt, text)
                .await
                .map_err(|_| codex::Error::Unknown)?,
        );
        // Answer completion is proven, but unknown usage cannot seed a future
        // turn's accounting baseline or reusable native checkpoint.
        self.native_session.take();
        Ok(())
    }
}
impl Capture {
    pub fn progress(&self) -> watch::Receiver<tokio::time::Instant> {
        self.progress.subscribe()
    }
    pub async fn finish(
        &mut self,
        host: &mut Host,
    ) -> Result<Option<SharedWorkReceipt>, NativeError> {
        if let Some(sink) = &mut self.sink {
            if !self
                .receipt
                .as_ref()
                .is_some_and(|r| matches!(r.state.as_str(), "completed" | "failed"))
            {
                self.receipt = Some(sink.record_unknown().await.map_err(store)?);
            }
        } else {
            let custody = host
                .pending
                .take()
                .or_else(|| self.receive.as_mut().and_then(|r| r.try_recv().ok()));
            if let Some(custody) = custody {
                self.receipt = Some(custody.finish_failed_start().await.map_err(store)?);
            }
        }
        Ok(self.receipt.clone())
    }
}
pub(in crate::console::native::shared_execution) async fn permission(
    live: &Live,
    work: &SharedWorkItem,
    request: Value,
) -> Result<bool, NativeError> {
    #[cfg(test)]
    if let Some(fixture) = live.io.crypto_fixture.as_ref().and_then(|f| f.approvals.as_ref()) {
        live.proof().await?;
        fixture.requests.lock().unwrap().push(request);
        return Ok(fixture.allow);
    }
    let id = format!(
        "sap_{}",
        digest(&json!({"work":work.id,"request":request["id"]}))?
    );
    let (b, _, _) = live.proof().await?;
    let private = if work.encrypted {
        Some((
            super::super::e2ee::device(&live.io, &b, &work.assignment_id).await?,
            crate::encrypted_approvals::nonce().map_err(|_| invalid())?,
        ))
    } else {
        None
    };
    let wire = if let Some((device, nonce)) = &private {
        crate::encrypted_approvals::metadata(
            &request,
            nonce,
            device
                .approval_client()
                .device_id()
                .ok_or_else(invalid)?
                .as_str(),
        )
        .map_err(|_| invalid())?
    } else {
        request.clone()
    };
    // Opening an encrypted device can span an activity renewal. Register with
    // the current proof, without canceling or repeating the native callback.
    let reply = upload(live, "register", |authority| Action::ApprovalRegister {
        authority,
        work: work.clone(),
        id: id.clone(),
        request: wire.clone(),
        remaining_ms: 115_000,
    })
    .await?;
    let row = reply.value;
    let hash_value = row["digest"].as_str().ok_or_else(invalid)?.to_owned();
    hash(&hash_value)?;
    if row["approval"]["id"] != id
        || row["approval"]["workId"] != work.id
        || row["approval"]["request"] != wire
        || row["approval"]["deciderMxid"] != work.requester_mxid.as_str()
        || digest(&row["approval"])? != hash_value
    {
        return Err(invalid());
    }
    let mut last_send = Instant::now() - Duration::from_secs(10);
    loop {
        if let Some((device, nonce)) = &private {
            if device.is_revoked() {
                tracing::warn!("Shared Claude tool request refused: the Agent's encrypted device was replaced");
                return Ok(false);
            }
            if !device.approvals.has_decision(&row).unwrap_or(false) {
                if last_send.elapsed() >= Duration::from_secs(10) {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        crate::encrypted_approvals::send_request(
                            device.approval_client(),
                            &row,
                            &request,
                            nonce,
                        ),
                    )
                    .await;
                    last_send = Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        }
        let reply = upload(live, "take", |authority| Action::ApprovalTake {
            authority,
            id: id.clone(),
            digest: hash_value.clone(),
        })
        .await?;
        if reply.value["id"] != id || reply.value["digest"] != hash_value {
            return Err(invalid());
        }
        if let Some(decision) = reply.value["decision"].as_str() {
            if let Some((device, _)) = &private
                && !device
                    .approvals
                    .matches_decision(
                        &row,
                        reply.value["decidedByMxid"].as_str().unwrap_or(""),
                        decision,
                    )
                    .unwrap_or(false)
            {
                tracing::warn!(decision, "Shared Claude tool request refused: the server's decision differs from the encrypted one");
                return Ok(false);
            }
            let by_sender = reply.value["decidedByMxid"] == work.requester_mxid.as_str();
            tracing::info!(decision, by_sender, "Shared Claude tool request decided");
            return Ok(decision == "approve_once" && by_sender);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
/// One server step of a waiting tool request under the current lease. A lease
/// renewal or a credential refresh can refuse a step while the person decides;
/// registering is idempotent and a decision stays on the server until it is
/// taken, so such a refusal is asked again. The caller's 120 s bound ends it.
async fn upload(
    live: &Live,
    step: &'static str,
    action: impl Fn(SharedActivityRef) -> Action,
) -> Result<Timed<Value>, NativeError> {
    loop {
        let attempt: Result<Timed<Value>, NativeError> = async {
            let (b, _, state) = live.proof().await?;
            b.upload(action(state.lease.authority), state.until).await
        }
        .await;
        match attempt {
            Ok(reply) => return Ok(reply),
            Err(error) if retry_later(live.io.transient(&error), &error.code) => {
                tracing::info!(step, code = %error.code, "Shared Claude tool request step retrying");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => {
                tracing::warn!(step, status = error.status, code = %error.code, "Shared Claude tool request step failed");
                return Err(error);
            }
        }
    }
}
/// The server refuses a step proved with the lease a renewal just replaced.
fn retry_later(transient: bool, code: &str) -> bool {
    transient || code == "shared_activity_expired_or_changed"
}
#[cfg(test)]
mod permission_tests {
    use super::retry_later;
    #[test]
    fn a_replaced_lease_or_a_refresh_is_asked_again_and_a_refusal_is_not() {
        assert!(retry_later(true, "management_scope_changed"));
        assert!(retry_later(false, "shared_activity_expired_or_changed"));
        for code in ["approval_expired", "approval_request_changed", "resource_owner_required", "invalid_server_response"] {
            assert!(!retry_later(false, code), "{code}");
        }
    }
}

pub(in crate::console::native::shared_execution) struct Matrix {
    live: Live,
    work: SharedWorkItem,
    scope: hagency_agent_local::Scope,
    pub queue: codex::ApprovalQueue,
    pub controls: controls::Inbox,
    sender: controls::Sender,
    card: Arc<CardHost>,
    questions: tokio::sync::mpsc::Receiver<PendingQuestion>,
    room: tokio::sync::mpsc::Receiver<PendingRoomCall>,
    history: Option<super::super::history::Reader<super::super::history::Source>>,
    files: Option<super::super::files::Files<super::super::files::Source>>,
}
impl Matrix {
    pub fn stop_sender(&self) -> controls::Sender {
        self.sender.clone()
    }
    pub fn new(
        live: Live,
        work: SharedWorkItem,
        scope: hagency_agent_local::Scope,
        call: &str,
        message_root: &std::path::Path,
        history: Option<super::super::history::Reader<super::super::history::Source>>,
        files: Option<super::super::files::Files<super::super::files::Source>>,
    ) -> Result<Self, NativeError> {
        let mut card = Card::for_shared(&work, call).map_err(|_| invalid())?;
        card.runtime = match live.authority.root().runtime().map_err(store)? {
            "claude" => "claude",
            "octos" => "octos",
            _ => return Err(invalid()),
        }.into();
        card.validate().map_err(|_| invalid())?;
        let card = Arc::new(CardHost::with_messages(card, message_root)
            .map_err(|_| failure(503, "shared_message_storage_unavailable"))?);
        let (sender, controls) =
            controls::channel(&scope, &work.id, call).map_err(|_| invalid())?;
        let (questions, qrx) = hagency_agent_local::questions::question_queue();
        let (room, rx) =
            hagency_agent_local::room_tools::room_queue(&scope, &work.id).map_err(|_| invalid())?;
        let (queue, unused) = codex::approval_queue(1).map_err(|_| invalid())?;
        drop(unused);
        let mut queue = queue
            .with_questions(questions)
            .with_live_text(card.live_text());
        if history.is_some() {
            queue = queue.with_room_tools(room);
        }
        Ok(Self {
            live,
            work,
            scope,
            queue,
            controls,
            sender,
            card,
            questions: qrx,
            room: rx,
            history,
            files,
        })
    }
    pub async fn serve(&mut self) {
        let room = serve_room(
            &self.live,
            &mut self.room,
            &mut self.history,
            &mut self.files,
        );
        let questions = serve_questions(&self.live, &self.work, &self.scope, &mut self.questions);
        let controls = serve_controls(&self.live, &self.card, &self.sender);
        let messages = serve_messages(&self.live, &self.work, &self.card);
        tokio::join!(room, questions, controls, messages);
    }
    pub async fn flush_messages(&self) -> Result<(), NativeError> {
        let Some(messages) = self.card.messages() else { return Ok(()); };
        if messages.next().map_err(|_| invalid())?.is_none() { return Ok(()); }
        while messages.next().map_err(|_| invalid())?.is_some() {
            let (b, _, _) = self.live.proof().await?;
            super::super::e2ee::send_message(&self.live.io, &b, &self.work, &messages).await?;
        }
        Ok(())
    }
    pub fn finish(&self, result: &Result<claude_turn::Outcome, NativeError>) {
        self.card.finish(match result {
            Ok(v) => match v.status {
                codex::TurnStatus::Completed => State::Completed,
                codex::TurnStatus::Interrupted => State::Interrupted,
                _ => State::Failed,
            },
            Err(_) => State::Unknown,
        });
    }
}
async fn serve_messages(live: &Live, work: &SharedWorkItem, card: &Arc<CardHost>) {
    let Some(messages) = card.messages() else { return; };
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if messages.next().ok().flatten().is_none() { continue; }
        let Ok((bound, _, _)) = live.proof().await else { continue; };
        let _ = super::super::e2ee::send_message(&live.io, &bound, work, &messages).await;
    }
}
impl Drop for Matrix {
    fn drop(&mut self) {
        self.card.finish(State::Unknown);
    }
}
async fn serve_room(
    live: &Live,
    receiver: &mut tokio::sync::mpsc::Receiver<PendingRoomCall>,
    history: &mut Option<super::super::history::Reader<super::super::history::Source>>,
    files: &mut Option<super::super::files::Files<super::super::files::Source>>,
) {
    while let Some(mut pending) = receiver.recv().await {
        if pending.is_closed() {
            continue;
        }
        let operation = pending.operation.clone();
        let result = tokio::select! {biased;
            _=pending.closed()=>continue,
            result=tokio::time::timeout(Duration::from_secs(60),async{
                if live.io.check_running(live.authority.root()).await.is_err(){return RoomReply::unavailable();}
                let Some(history)=history.as_mut()else{return RoomReply::unavailable()};
                let text=match &operation{
                    RoomOperation::History{cursor}=>match history.read(cursor.as_deref()).await{Ok(v)=>v,Err(_)=>return RoomReply::unavailable()},
                    RoomOperation::File(operation)=>match files.as_mut(){Some(files)=>files.handle(operation,history).await,None=>return RoomReply::unavailable()},
                    RoomOperation::Memory(_)=>return RoomReply::unavailable(),
                };
                let error=serde_json::from_str::<Value>(&text).ok().is_some_and(|v|v.get("error").is_some());
                RoomReply::new(text,error).unwrap_or_else(|_|RoomReply::unavailable())
            })=>result.unwrap_or_else(|_|RoomReply::unavailable()),
        };
        pending.reply(result);
    }
}
#[derive(Default)]
struct SharedQuestionHost {
    questions: super::super::questions::Questions<PendingQuestion>,
    receipts: Vec<hagency_agent_local::questions::Delivery>,
}

impl SharedQuestionHost {
    fn settle(&mut self) {
        self.receipts.retain_mut(|receipt| {
            if let Some(accepted) = receipt.status() {
                self.questions.delivered(&receipt.id, accepted);
                false
            } else {
                true
            }
        });
    }
}

impl Drop for SharedQuestionHost {
    fn drop(&mut self) {
        // The run can finish before the next poll. Settle any confirmed
        // native write before Questions closes the remaining cards on drop.
        self.settle();
    }
}

async fn serve_questions(
    live: &Live,
    work: &SharedWorkItem,
    scope: &hagency_agent_local::Scope,
    receiver: &mut tokio::sync::mpsc::Receiver<PendingQuestion>,
) {
    // Use the shared room source so production uses the authenticated Agent
    // device and offline tests can substitute only the Matrix boundary.
    let mut host = SharedQuestionHost::default();
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased;
            pending=receiver.recv()=>{
                let Some(pending)=pending else{break};
                if pending.scope!=*scope||pending.dispatch!=work.id||pending.is_closed(){continue;}
                if live.io.check_running(live.authority.root()).await.is_err() {
                    let _ = pending.reply(Default::default());
                    continue;
                }
                if let Ok(Some(delivery)) = host.questions.ask(&live.io, work, pending).await {
                    host.receipts.push(delivery);
                }
            },
            _=poll.tick()=>{
                host.settle();
                if live.io.check_running(live.authority.root()).await.is_err() {
                    host.questions.close();
                    break;
                }
                if let Ok(ready) = host.questions.collect(&live.io).await { host.receipts.extend(ready); }
                let _ = host.questions.publish().await;
            },
        }
    }
}
async fn serve_controls(live: &Live, card: &Arc<CardHost>, sender: &controls::Sender) {
    // The card needs the Agent's device. One that is not ready yet is asked
    // for again while the run lasts, as on the Codex path, so a long run still
    // gets its Stop and its added instructions. The run's end drops this
    // future, which ends the waiting.
    let Some(device) = crate::console::owned_runtime::run_controls::open_while_running(
        Duration::from_secs(10),
        || async move {
            live.io.check_running(live.authority.root()).await?;
            let b = live.io.bound().await?;
            super::super::e2ee::device(&live.io, &b, &live.authority.root().assignment_id).await
        },
        || false,
    )
    .await
    else {
        return;
    };
    if card.open(device.clone()).is_err() {
        std::future::pending::<()>().await;
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        card.drain_receipts(false);
        let _ = card.publish().await;
        for (event, action) in card.actions().await.unwrap_or_default() {
            if live.io.check_running(live.authority.root()).await.is_err() {
                let _ = card.receipt(&event.event_id, &event.sender, "unavailable", false);
                continue;
            }
            let (action, stop) = match action {
                MatrixAction::Steer { text } => (controls::Action::Steer { text }, false),
                MatrixAction::Interrupt {} => (controls::Action::Interrupt, true),
                MatrixAction::Queue { .. } => {
                    let _ = card.receipt(&event.event_id, &event.sender, "unsupported", false);
                    continue;
                }
            };
            match sender.submit(action) {
                Ok(submission) => card.track(
                    event.event_id,
                    event.sender,
                    stop,
                    submission.receipt,
                    outcome_name,
                ),
                Err(error) => {
                    let _ = card.receipt(
                        &event.event_id,
                        &event.sender,
                        if error == controls::SubmitError::Busy {
                            "busy"
                        } else {
                            "unavailable"
                        },
                        stop,
                    );
                }
            }
        }
    }
}
fn outcome_name(outcome: controls::Outcome) -> &'static str {
    match outcome {
        controls::Outcome::Accepted => "accepted",
        controls::Outcome::Rejected => "rejected",
        controls::Outcome::Unsupported => "unsupported",
        controls::Outcome::InProgress => "in_progress",
        controls::Outcome::Busy => "busy",
        controls::Outcome::Unavailable => "unavailable",
        controls::Outcome::Unknown => "unknown",
    }
}
