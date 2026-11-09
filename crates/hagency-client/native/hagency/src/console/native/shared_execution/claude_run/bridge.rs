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
}
pub(super) fn capture(
    live: Live,
    policy: SharedClaudePolicy,
    pending: SharedCallCustody,
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
    ) -> bool {
        let tool = tool.strip_prefix("claude.").unwrap_or(&tool);
        // These modes come from the resource owner's signed successor policy.
        if self
            .live
            .io
            .check_running(self.live.authority.root())
            .await
            .is_err()
        {
            return false;
        }
        match self.policy {
            SharedClaudePolicy::Text => false,
            SharedClaudePolicy::LiveWeb => matches!(tool, "WebSearch" | "WebFetch"),
            SharedClaudePolicy::Yolo { web } => web || !matches!(tool, "WebSearch" | "WebFetch"),
            SharedClaudePolicy::Standard { web } => {
                if !web && matches!(tool, "WebSearch" | "WebFetch") {
                    return false;
                }
                let Some(session) = &self.session else {
                    return false;
                };
                let request = json!({"id":id,"method":"claude/can_use_tool","params":{"sessionId":session,"toolName":tool,"input":arguments},"sessionAllowed":false,"managerCanDecide":false});
                tokio::time::timeout(
                    Duration::from_secs(120),
                    permission(&self.live, &self.work, request),
                )
                .await
                .is_ok_and(|v| v.unwrap_or(false))
            }
        }
    }
}
impl claude_turn::Capture for Capture {
    async fn bind(&mut self, process: &OwnedClaudeSession) -> codex::Result<()> {
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
        Ok(())
    }
    async fn observe(
        &mut self,
        observation: &hagency_runtime::claude::session::Observation,
    ) -> codex::Result<()> {
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
        Ok(())
    }
}
impl Capture {
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
async fn permission(
    live: &Live,
    work: &SharedWorkItem,
    request: Value,
) -> Result<bool, NativeError> {
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
    let (b, _, state) = live.proof().await?;
    let reply: Timed<Value> = b
        .upload(
            Action::ApprovalRegister {
                authority: state.lease.authority,
                work: work.clone(),
                id: id.clone(),
                request: wire.clone(),
                remaining_ms: 115_000,
            },
            state.until,
        )
        .await?;
    let row = reply.value;
    let hash_value = row["digest"].as_str().ok_or_else(invalid)?.to_owned();
    hash(&hash_value)?;
    if row["approval"]["id"] != id
        || row["approval"]["workId"] != work.id
        || row["approval"]["request"] != wire
        || digest(&row["approval"])? != hash_value
    {
        return Err(invalid());
    }
    let mut last_send = Instant::now() - Duration::from_secs(10);
    loop {
        if let Some((device, nonce)) = &private {
            if device.is_revoked() {
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
        let (b, _, state) = live.proof().await?;
        let reply: Timed<Value> = b
            .upload(
                Action::ApprovalTake {
                    authority: state.lease.authority,
                    id: id.clone(),
                    digest: hash_value.clone(),
                },
                state.until,
            )
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
                    return Ok(false);
            }
            return Ok(decision == "approve_once" && reply.value["ownerApproved"] == true);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) struct Matrix {
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
    pub fn new(
        live: Live,
        work: SharedWorkItem,
        scope: hagency_agent_local::Scope,
        call: &str,
        history: Option<super::super::history::Reader<super::super::history::Source>>,
        files: Option<super::super::files::Files<super::super::files::Source>>,
    ) -> Result<Self, NativeError> {
        let mut card = Card::for_shared(&work, call).map_err(|_| invalid())?;
        card.runtime = "claude".into();
        card.validate().map_err(|_| invalid())?;
        let card = Arc::new(CardHost::new(card));
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
        tokio::join!(room, questions, controls);
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
                };
                let error=serde_json::from_str::<Value>(&text).ok().is_some_and(|v|v.get("error").is_some());
                RoomReply::new(text,error).unwrap_or_else(|_|RoomReply::unavailable())
            })=>result.unwrap_or_else(|_|RoomReply::unavailable()),
        };
        pending.reply(result);
    }
}
async fn serve_questions(
    live: &Live,
    work: &SharedWorkItem,
    scope: &hagency_agent_local::Scope,
    receiver: &mut tokio::sync::mpsc::Receiver<PendingQuestion>,
) {
    use crate::console::owned_runtime::questions::Host as QuestionHost;
    let context = super::super::questions::QuestionContext {
        id: work.id.clone(),
        room_id: work.room_id.clone(),
        event_id: work.event_id.clone(),
        thread_root: work.thread_root.clone(),
        responders: vec![work.requester_mxid.clone()],
    };
    let mut host = QuestionHost::default();
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased;
            pending=receiver.recv()=>{
                let Some(pending)=pending else{break};
                if pending.scope!=*scope||pending.dispatch!=work.id||pending.is_closed(){continue;}
                if !host.is_open(){
                    let began=Instant::now();
                    let result=tokio::time::timeout_at(pending.deadline,async{live.io.check_running(live.authority.root()).await?;let b=live.io.bound().await?;super::super::e2ee::device(&live.io,&b,&work.assignment_id).await}).await;
                    if let Ok(Ok(device))=result{host.open(device,began.elapsed());}
                }
                host.ask(&context,pending).await;
            },
            _=poll.tick()=>host.poll().await,
        }
    }
}
async fn serve_controls(live: &Live, card: &Arc<CardHost>, sender: &controls::Sender) {
    let opened = tokio::time::timeout(Duration::from_secs(10), async {
        live.io.check_running(live.authority.root()).await?;
        let b = live.io.bound().await?;
        super::super::e2ee::device(&live.io, &b, &live.authority.root().assignment_id).await
    })
    .await;
    let Ok(Ok(device)) = opened else {
        std::future::pending::<()>().await;
        return;
    };
    if card.open(device).is_err() {
        std::future::pending::<()>().await;
        return;
    }
    let mut receipts = Vec::<(String, String, bool, oneshot::Receiver<controls::Outcome>)>::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        for i in (0..receipts.len()).rev() {
            let outcome = match receipts[i].3.try_recv() {
                Ok(v) => v,
                Err(oneshot::error::TryRecvError::Empty) => continue,
                Err(_) => controls::Outcome::Unknown,
            };
            let (event, who, stop, _) = receipts.remove(i);
            let _ = card.receipt(&event, &who, outcome_name(outcome), stop);
        }
        let _ = card.publish().await;
        for (event, action) in card.actions().await.unwrap_or_default() {
            if !card.claim(&event.event_id) {
                continue;
            }
            if live.io.check_running(live.authority.root()).await.is_err() {
                let _ = card.receipt(&event.event_id, &event.sender, "unavailable", false);
                continue;
            }
            let action = match action {
                MatrixAction::Interrupt {} => controls::Action::Interrupt,
                _ => {
                    let _ = card.receipt(&event.event_id, &event.sender, "unsupported", false);
                    continue;
                }
            };
            match sender.submit(action) {
                Ok(submission) => {
                    receipts.push((event.event_id, event.sender, true, submission.receipt))
                }
                Err(error) => {
                    let _ = card.receipt(
                        &event.event_id,
                        &event.sender,
                        if error == controls::SubmitError::Busy {
                            "busy"
                        } else {
                            "unavailable"
                        },
                        true,
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
