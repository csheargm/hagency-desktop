//! Shared native Claude interaction engine. The host supplies admission,
//! permission authority and a capture bound to this original native process.
//! Personal and pooled execution use the same questions, room tools, live
//! output, cooperative controls and background-completion semantics.
use super::{Claude, approval_directory, claude_controls};
use crate::codex::{ApprovalQueue, Error, Profile, Result, TurnStatus};
use crate::{Scope, Usage};
use hagency_runtime::claude;
use serde_json::Value;
use std::{future::Future, path::Path, time::Duration};

/// Trusted host only: a native tool request is never an approval. Implementors
/// must bind their decision to the original owner, requester and dispatch.
pub trait Host: Send {
    /// The shared host sends only its sealed call input after checking its live
    /// call authority. This boundary runs before any model input is written.
    fn start(
        &mut self,
        process: &mut Claude,
        input: &str,
    ) -> impl Future<Output = Result<()>> + Send;
    fn bind_session(&mut self, session: &str) -> Result<()>;
    fn permission(
        &mut self,
        request_id: &str,
        tool: String,
        arguments: Value,
        directory: &Path,
    ) -> impl Future<Output = bool> + Send;
}

/// The original owned stream is attached immediately after system/init. Every
/// subsequent native observation reaches this capture before host deferral,
/// including reads made while permissions, room IO or interrupt writes wait.
/// A result frame alone is not completion: `complete` follows native idle and
/// any required background-usage snapshot. Counts in Outcome are display data,
/// not a substitute for these private-constructor native receipts.
pub trait Capture: Send {
    fn bind(&mut self, process: &Claude) -> impl Future<Output = Result<()>> + Send;
    fn observe(
        &mut self,
        observation: &claude::session::Observation,
    ) -> impl Future<Output = Result<()>> + Send;
    fn complete(&mut self, process: &Claude, text: &str)
    -> impl Future<Output = Result<()>> + Send;
    fn complete_unaccounted(
        &mut self,
        receipt: &claude::session::CompletionReceipt,
        text: &str,
    ) -> impl Future<Output = Result<()>> + Send;
}

pub struct Outcome {
    pub text: String,
    pub usage: Usage,
    pub status: TurnStatus,
    pub requests: usize,
    pub returns: usize,
    pub cumulative: bool,
}
type TurnResult = Outcome;

async fn unresolved<C: Capture>(
    controls: &mut claude_controls::Controls<'_, C>,
    receipt: Option<&claude::session::CompletionReceipt>,
    completed: Option<(String, Usage, TurnStatus, bool)>,
    requests: usize,
    returns: usize,
) -> Result<TurnResult> {
    let (text, _, status, _) = completed.ok_or(Error::Unknown)?;
    let receipt = receipt.ok_or(Error::Unknown)?;
    controls.complete_unaccounted(receipt, &text).await?;
    Ok(TurnResult {
        text,
        usage: crate::unreported_usage(),
        status: if receipt.interrupted() {
            TurnStatus::Interrupted
        } else {
            status
        },
        requests,
        returns,
        cumulative: false,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run<H: Host, C: Capture>(
    wire: &mut Claude,
    profile: &Profile,
    scope: &Scope,
    dispatch: &str,
    input: &str,
    queue: &ApprovalQueue,
    started: Option<tokio::sync::oneshot::Sender<()>>,
    stream: Option<crate::run_controls::Stream>,
    host: &mut H,
    capture: &mut C,
) -> Result<Outcome> {
    if queue
        .room
        .as_ref()
        .is_some_and(|room| !room.matches(scope, dispatch))
    {
        return Err(Error::Protocol("room tool dispatch differs"));
    }
    host.start(wire, input).await?;
    // Claude binds its session on the first system/init event after prompt.
    let init = wire
        .next_message()
        .await
        .map_err(|_| Error::Protocol("Claude session init"))?;
    if !matches!(
        init,
        claude::Message::Event {
            kind: claude::EventKind::System,
            ..
        }
    ) {
        return Err(Error::Protocol("Claude session init"));
    }
    capture.bind(wire).await?;
    host.bind_session(wire.session_id().ok_or(Error::Unknown)?)?;
    wire.supervise_turn()
        .map_err(|_| Error::Protocol("Claude turn supervision"))?;
    wire.enable_approval_control(claude::session::ApprovalControlPolicy {
        owner_wait_ms: 300_000,
        response_reserve_ms: 10_000,
    })
    .map_err(|_| Error::Protocol("Claude control"))?;
    wire.set_question_wait(610_000)
        .map_err(|_| Error::Protocol("Claude question wait"))?;
    if queue.room.is_some() {
        wire.enable_room_tool_callbacks()
            .map_err(|_| Error::Protocol("Claude room tools"))?;
    }
    let mut controls = claude_controls::Controls::new(stream, queue.live_text.clone(), capture);
    if let Some(send) = started {
        let _ = send.send(());
    }
    let mut requests = std::collections::BTreeSet::new();
    let mut returns = std::collections::BTreeSet::new();
    let mut buffered = std::collections::VecDeque::new();
    let mut completed: Option<(String, Usage, TurnStatus, bool)> = None;
    let mut completion_receipt = None;
    loop {
        if buffered.is_empty()
            && controls.settled()
            && wire.phase() == claude::session::Phase::ResultObserved
        {
            if wire.needs_usage_snapshot() && !wire.usage_snapshot_requested() {
                completion_receipt = Some(wire.completion_receipt().map_err(|_| Error::Unknown)?);
                if wire.request_usage_snapshot().await.is_err() {
                    return unresolved(
                        &mut controls,
                        completion_receipt.as_ref(),
                        completed.take(),
                        requests.len(),
                        returns.len(),
                    )
                    .await;
                }
                continue;
            }
            let (text, mut usage, status, cumulative) = completed.take().ok_or(Error::Unknown)?;
            if wire.needs_usage_snapshot() {
                usage = crate::unreported_usage();
            }
            controls.complete(wire, &text).await?;
            return Ok(TurnResult {
                text,
                usage,
                status: if controls.accepted() {
                    TurnStatus::Interrupted
                } else {
                    status
                },
                requests: requests.len(),
                returns: returns.len(),
                cumulative,
            });
        }
        let (message, observation) = if let Some(item) = buffered.pop_front() {
            item
        } else {
            match controls.message(wire).await {
                Ok(Some(observed)) => observed,
                Ok(None) => continue,
                Err(_) if wire.usage_snapshot_requested() => {
                    // The original answer and idle state were already proven.
                    // A failed accounting query must not discard that answer;
                    // its unknown usage remains recorded for reconciliation.
                    return unresolved(
                        &mut controls,
                        completion_receipt.as_ref(),
                        completed.take(),
                        requests.len(),
                        returns.len(),
                    )
                    .await;
                }
                Err(_) => return Err(Error::Protocol("Claude event")),
            }
        };
        if controls.accepted()
            && matches!(
                message,
                claude::Message::Mcp { .. } | claude::Message::Permission { .. }
            )
        {
            continue;
        }
        match message {
            claude::Message::Mcp { request_id, .. } => {
                let request = wire
                    .room_tool_request(&request_id)
                    .map_err(|_| Error::Protocol("Claude room callback"))?;
                let mut prepared = if let Some(operation) = request.operation() {
                    let channel = queue.room.as_ref().ok_or(Error::Protocol("Claude room authority missing"))?;
                    // Leave the native writer its own ten-second bound so a
                    // slow Matrix operation can return an error and reasoning
                    // can continue instead of expiring the entire callback.
                    let deadline = request.response_deadline().checked_sub(Duration::from_secs(10))
                        .unwrap_or_else(tokio::time::Instant::now)
                        .min(tokio::time::Instant::now() + Duration::from_secs(65));
                    let reply = channel.call(operation.clone(), request.tool_use_id().unwrap_or(&request_id).into(), deadline);
                    tokio::pin!(reply);
                    let answer = loop {
                        match controls.next(wire, reply.as_mut(), true).await? {
                            claude_controls::Update::Ready(answer) => break Some(answer),
                            claude_controls::Update::Interrupted | claude_controls::Update::Idle => break None,
                            claude_controls::Update::Message((message, observation)) => {
                                let cancelled = matches!(&message, claude::Message::ControlCancel { request_id: id } if id == &request_id);
                                let ended = wire.phase() == claude::session::Phase::ResultObserved;
                                claude_controls::defer(&mut buffered, message, observation)?;
                                if cancelled || ended { break None; }
                            }
                        }
                    };
                    let Some(answer) = answer else { continue; };
                    wire.prepare_room_tool_reply(&request, answer.text(), answer.is_error())
                } else {
                    wire.prepare_room_metadata_reply(&request)
                }.map_err(|_| Error::Protocol("Claude room response"))?;
                loop {
                    match wire
                        .send_prepared_room_reply(&mut prepared)
                        .await
                        .map_err(|_| Error::Protocol("Claude room response write"))?
                    {
                        claude::session::PreparedUpdate::WriteAccepted(_) => break,
                        claude::session::PreparedUpdate::Message(message) => {
                            controls.observe_message(wire, &message).await?;
                            claude_controls::defer(
                                &mut buffered,
                                message,
                                wire.last_observation().cloned(),
                            )?;
                        }
                    }
                }
            }
            claude::Message::Permission {
                request_id,
                tool_name,
                input,
                ..
            } => {
                let directory = approval_directory(&profile.cwd, &input);
                enum Decision {
                    Tool(bool),
                    Question(Option<crate::questions::SubmittedAnswer>),
                }
                let questions = if tool_name == "AskUserQuestion" {
                    Some(
                        wire.question(&request_id)
                            .map_err(|_| Error::Protocol("Claude question"))?,
                    )
                } else {
                    None
                };
                let question_id = request_id.clone();
                let question_deadline = wire
                    .approval_deadline(&request_id)
                    .map_err(|_| Error::Protocol("Claude question deadline"))?
                    .min(tokio::time::Instant::now() + Duration::from_secs(600));
                let decision = async {
                    // Only our bound, closed-schema Matrix SDK tools inherit
                    // this dispatch's room authority. External MCP tools still
                    // use the owner's ordinary permission policy below.
                    if let Some(name) = tool_name.strip_prefix("mcp__hagency_matrix__") {
                        return Decision::Tool(
                            queue.room.is_some()
                                && hagency_runtime::room_tools::RoomTools::history_and_files()
                                    .parse(name, &input)
                                    .is_ok(),
                        );
                    }
                    if let Some(questions) = questions {
                        return Decision::Question(match &queue.questions {
                            Some(channel) => {
                                channel
                                    .ask(scope, dispatch, question_id, questions, question_deadline)
                                    .await
                            }
                            None => None,
                        });
                    }
                    Decision::Tool(
                        host.permission(
                            &request_id,
                            format!("claude.{tool_name}"),
                            input,
                            &directory,
                        )
                        .await,
                    )
                };
                tokio::pin!(decision);
                let allow = loop {
                    match controls.next(wire, decision.as_mut(), true).await? {
                        claude_controls::Update::Ready(allow) => break Some(allow),
                        claude_controls::Update::Interrupted | claude_controls::Update::Idle => {
                            break None;
                        }
                        claude_controls::Update::Message((message, observation)) => {
                            let cancelled = matches!(&message,claude::Message::ControlCancel {request_id:id} if id==&request_id);
                            let ended = wire.phase() == claude::session::Phase::ResultObserved;
                            claude_controls::defer(&mut buffered, message, observation)?;
                            if cancelled || ended {
                                break None;
                            }
                        }
                    }
                };
                let Some(decision) = allow else {
                    continue;
                };
                let mut submitted = None;
                let mut prepared = match decision {
                    Decision::Question(answer) => {
                        let answers = answer
                            .as_ref()
                            .map(|a| a.answers.clone())
                            .unwrap_or_default();
                        submitted = answer;
                        wire.prepare_question(&request_id, &answers)
                    }
                    Decision::Tool(allow) => wire.prepare_approval(
                        &request_id,
                        if allow {
                            claude::session::PermissionDecision::Allow
                        } else {
                            claude::session::PermissionDecision::Deny
                        },
                    ),
                }
                .map_err(|_| Error::Protocol("Claude decision"))?;
                loop {
                    match wire
                        .send_prepared_approval(&mut prepared)
                        .await
                        .map_err(|_| Error::Protocol("Claude approval write"))?
                    {
                        claude::session::PreparedUpdate::WriteAccepted(_) => break,
                        claude::session::PreparedUpdate::Message(message) => {
                            controls.observe_message(wire, &message).await?;
                            claude_controls::defer(
                                &mut buffered,
                                message,
                                wire.last_observation().cloned(),
                            )?;
                        }
                    }
                }
                if let Some(answer) = submitted {
                    answer.accepted();
                }
            }
            claude::Message::Event { kind, payload, .. } => {
                if let Some(content) = payload["message"]["content"].as_array() {
                    for block in content {
                        if block["type"] == "tool_use"
                            && let Some(id) = block["id"].as_str()
                        {
                            claude_controls::remember_tool(&mut requests, id)?;
                        }
                        if block["type"] == "tool_result"
                            && let Some(id) = block["tool_use_id"].as_str()
                        {
                            claude_controls::remember_tool(&mut returns, id)?;
                        }
                    }
                }
                if kind == claude::EventKind::Result {
                    let Some(observation) = observation else {
                        return Err(Error::Unknown);
                    };
                    let is_error = observation.result_error().ok_or(Error::Unknown)?;
                    let (usage, cumulative) =
                        if let claude::session::ObservationKind::Result { usage, .. } =
                            observation.kind()
                        {
                            let counts = usage.counts();
                            let cumulative = usage.coverage()
                                == claude::session::UsageCoverage::ReportedModelsResult;
                            let projected = match (
                                counts.input(),
                                counts.cache_read(),
                                counts.cache_write(),
                                counts.output(),
                            ) {
                                (Some(fresh), Some(cache), Some(write), Some(output)) => Usage {
                                    input: fresh
                                        .checked_add(cache)
                                        .and_then(|v| v.checked_add(write))
                                        .ok_or(Error::Unknown)?,
                                    output,
                                    cached_input: cache,
                                    cache_write_input: Some(write),
                                    reasoning_output: 0,
                                    accounting_version: "claude-reported-v1".into(),
                                },
                                _ => crate::unreported_usage(),
                            };
                            (
                                if cumulative {
                                    projected
                                } else {
                                    crate::unreported_usage()
                                },
                                cumulative,
                            )
                        } else {
                            (crate::unreported_usage(), false)
                        };
                    if wire.usage_snapshot_requested() {
                        let original = completed.as_mut().ok_or(Error::Unknown)?;
                        if usage.is_reported()
                            && original.1.is_reported()
                            && (usage.input < original.1.input
                                || usage.output < original.1.output
                                || usage.cached_input < original.1.cached_input
                                || matches!((usage.cache_write_input, original.1.cache_write_input), (Some(next), Some(prior)) if next < prior))
                        {
                            return unresolved(
                                &mut controls,
                                completion_receipt.as_ref(),
                                completed.take(),
                                requests.len(),
                                returns.len(),
                            )
                            .await;
                        }
                        original.1 = if is_error || !cumulative {
                            crate::unreported_usage()
                        } else {
                            usage
                        };
                        original.3 = cumulative;
                    } else {
                        completed = Some((
                            payload["result"].as_str().unwrap_or("").into(),
                            usage,
                            if controls.accepted()
                                || payload["terminal_reason"] == "aborted_streaming"
                            {
                                TurnStatus::Interrupted
                            } else if is_error {
                                TurnStatus::Failed
                            } else {
                                TurnStatus::Completed
                            },
                            cumulative,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}
