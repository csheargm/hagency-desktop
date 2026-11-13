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
    /// The owner's own command emptied the conversation (`/clear`) and the
    /// CLI opened another session: the saved pointer moves to it. Only a host
    /// whose caller can be the owner's own command implements this.
    fn replace_session(&mut self, _session: &str) -> Result<()> {
        Err(Error::Protocol("Claude context changed"))
    }
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
    /// The failure that ended the native stream after this answer was read.
    /// The answer is still delivered. The host leaves the call's usage as it
    /// does for any failed stream and never reuses the process.
    pub late_failure: Option<Error>,
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
        late_failure: None,
    })
}

/// A clean final answer that was already read stays deliverable when the
/// native stream then fails or closes: a later frame over the size bound, a
/// fatal frame, process exit or an expired read bound. That stream can report
/// no further usage, so usage stays unreported. Everything else keeps the
/// original failure: an error or aborted result, text that arrived under a
/// stop, a refused capture, and any host-side failure while the stream is
/// still open.
async fn kept_answer<C: Capture>(
    wire: &Claude,
    controls: &mut claude_controls::Controls<'_, C>,
    requests: usize,
    returns: usize,
    error: Error,
) -> Result<TurnResult> {
    if wire.phase() != claude::session::Phase::Closed {
        return Err(error);
    }
    let Some(answer) = controls
        .take_answer()
        .filter(|answer| answer.clean && !answer.text.is_empty())
    else {
        return Err(error);
    };
    let Ok(receipt) = wire.closed_answer_receipt() else {
        return Err(error);
    };
    if receipt.is_error()
        || receipt.interrupted()
        || !receipt.matches_text(&answer.text)
        || controls
            .complete_unaccounted(&receipt, &answer.text)
            .await
            .is_err()
    {
        return Err(error);
    }
    Ok(TurnResult {
        text: answer.text,
        usage: crate::unreported_usage(),
        status: TurnStatus::Completed,
        requests,
        returns,
        cumulative: false,
        late_failure: Some(error),
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
    // No frame in time and a wrong frame are different faults: say which.
    let init = wire.next_message().await.map_err(|error| {
        Error::Protocol(match error {
            claude::session::Error::Timeout => "Claude session init timed out",
            _ => "Claude session init",
        })
    })?;
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
    let session = wire.session_id().ok_or(Error::Unknown)?;
    if wire.conversation_reset() {
        host.replace_session(session)?;
    } else {
        host.bind_session(session)?;
    }
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
    let driven = drive(
        wire,
        profile,
        scope,
        dispatch,
        queue,
        host,
        &mut controls,
        &mut requests,
        &mut returns,
    )
    .await;
    match driven {
        Err(error) => kept_answer(wire, &mut controls, requests.len(), returns.len(), error).await,
        // A command of the owner's CLI may print nothing (`/clear`). The room
        // still gets an answer: which command ran.
        Ok(mut done)
            if done.text.is_empty()
                && done.status == TurnStatus::Completed
                && wire.owner_command_turn() =>
        {
            done.text = command_notice(input, wire.conversation_reset());
            Ok(done)
        }
        done => done,
    }
}

/// The reply for an owner's command that printed nothing. Only the command's
/// own name is repeated, never its arguments.
fn command_notice(input: &str, reset: bool) -> String {
    let name: String = input
        .split_whitespace()
        .next()
        .unwrap_or("/")
        .chars()
        .take(64)
        .collect();
    if reset {
        format!("✅ 已执行 {name}：对话已清空，下一条消息开始新的对话。")
    } else {
        format!("✅ 已执行 {name}。")
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive<H: Host, C: Capture>(
    wire: &mut Claude,
    profile: &Profile,
    scope: &Scope,
    dispatch: &str,
    queue: &ApprovalQueue,
    host: &mut H,
    controls: &mut claude_controls::Controls<'_, C>,
    requests: &mut std::collections::BTreeSet<String>,
    returns: &mut std::collections::BTreeSet<String>,
) -> Result<TurnResult> {
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
                        controls,
                        completion_receipt.as_ref(),
                        completed.take(),
                        requests.len(),
                        returns.len(),
                    )
                    .await;
                }
                continue;
            }
            if wire.needs_usage_snapshot() {
                // The one accounting query is spent and background work has
                // reported again. The answer and idle state remain proven:
                // keep that answer and leave its usage unreported.
                return unresolved(
                    controls,
                    completion_receipt.as_ref(),
                    completed.take(),
                    requests.len(),
                    returns.len(),
                )
                .await;
            }
            let (text, usage, status, cumulative) = completed.take().ok_or(Error::Unknown)?;
            controls.complete(wire, &text).await?;
            return Ok(TurnResult {
                text,
                usage,
                // A stop accepted after the final answer does not undo it.
                status: if wire.answer_interrupted() {
                    TurnStatus::Interrupted
                } else {
                    status
                },
                requests: requests.len(),
                returns: returns.len(),
                cumulative,
                late_failure: None,
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
                        controls,
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
                controls.finish_write(wire, &mut buffered).await?;
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
                            claude_controls::remember_tool(requests, id)?;
                        }
                        if block["type"] == "tool_result"
                            && let Some(id) = block["tool_use_id"].as_str()
                        {
                            claude_controls::remember_tool(returns, id)?;
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
                                controls,
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
                        // Joined exactly as the driver's receipt verifies it.
                        let mut text = match completed.take() {
                            Some((text, ..)) if observation.continues_answer() => text,
                            _ => String::new(),
                        };
                        claude::session::join_answer(
                            &mut text,
                            payload["result"].as_str().unwrap_or(""),
                        );
                        completed = Some((
                            text,
                            usage,
                            if wire.answer_interrupted()
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
