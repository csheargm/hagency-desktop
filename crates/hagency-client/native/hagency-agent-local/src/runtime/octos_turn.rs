//! Shared Octos interaction loop. The host owns admission and durable capture;
//! questions and room tools carry no permission to bypass that authority.
use super::{Octos, OctosSteers, OctosWait, octos_control, octos_room_tools, octos_wait, octos_progress, octos_progress_tool};
use crate::{
    Scope, Usage,
    codex::{ApprovalQueue, Error, Result, TurnStatus},
};
use hagency_runtime::octos::{
    self,
    session::{Event, Observation, ObservationKind},
};
use std::{
    collections::{BTreeSet, VecDeque},
    future::Future,
    time::Duration,
};
use serde_json::Value;
use hagency_runtime::owned::SharedOctosShell;

pub trait Host: Send {
    /// Send only the original admitted input, and bind it before its first byte.
    fn start(
        &mut self,
        process: &mut Octos,
        turn: &str,
        input: &str,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Approve only this native request under the owner's current policy.
    fn permission(
        &mut self,
        _id: &str,
        _session: &str,
        _params: Value,
    ) -> impl Future<Output = bool> + Send { std::future::ready(false) }
    fn bind_shell(&mut self, _shell: SharedOctosShell) -> Result<()> { Ok(()) }
    fn shell(&mut self, _session: &str, _id: &str, _args: Value)
        -> impl Future<Output = std::result::Result<Value, String>> + Send {
        std::future::ready(Err("shell_unavailable".into()))
    }
    fn exec_command(&mut self, _session: &str, _id: &str, _args: Value)
        -> impl Future<Output = std::result::Result<Value, String>> + Send {
        std::future::ready(Err("shell_unavailable".into()))
    }
    fn write_stdin(&mut self, _session: &str, _id: &str, _args: Value)
        -> impl Future<Output = std::result::Result<Value, String>> + Send {
        std::future::ready(Err("shell_unavailable".into()))
    }
    fn apply_patch(&mut self, _session: &str, _id: &str, _args: Value)
        -> impl Future<Output = std::result::Result<Value, String>> + Send {
        std::future::ready(Err("patch_unavailable".into()))
    }
}
pub trait Capture: Send {
    fn bind(&mut self, process: &Octos) -> impl Future<Output = Result<()>> + Send;
    fn observe(&mut self, observation: &Observation) -> impl Future<Output = Result<()>> + Send;
    fn complete(&mut self, process: &Octos, text: &str) -> impl Future<Output = Result<()>> + Send;
}

fn host_question_request(session: &str, turn: &str, call_id: &str, args: &Value)
    -> std::result::Result<hagency_runtime::codex::session::UserInputRequest, &'static str> {
    let request = hagency_runtime::codex::session::UserInputRequest::parse(
        hagency_runtime::codex::RequestId::String(call_id.to_owned()),
        &serde_json::json!({"threadId":session,"turnId":turn,"itemId":call_id,
            "isBlocking":true,"questions":args["questions"]}))
        .map_err(|_| "invalid_user_input_request")?;
    if request.is_codex_approval() || request.questions().iter().any(|question| question.is_secret) {
        return Err("invalid_user_input_request");
    }
    Ok(request)
}

#[cfg(test)]
mod question_tests {
    use super::*;
    #[test]
    fn room_questions_cannot_be_used_as_owner_approval_or_secret_prompts() {
        let question = serde_json::json!({"questions":[{"id":"choice","header":"Choice",
            "question":"Which one?","options":[]}]});
        assert!(host_question_request("session", "turn", "call", &question).is_ok());
        let mut secret = question.clone(); secret["questions"][0]["isSecret"] = serde_json::json!(true);
        assert!(host_question_request("session", "turn", "call", &secret).is_err());
        let mut approval = question.clone(); approval["questions"][0]["id"] = serde_json::json!("mcp_tool_call_approval_123");
        assert!(host_question_request("session", "turn", "call", &approval).is_err());
        let mut duplicate = question; let repeated = duplicate["questions"][0].clone();
        duplicate["questions"].as_array_mut().unwrap().push(repeated);
        assert!(host_question_request("session", "turn", "call", &duplicate).is_err());
    }
}

/// One scope, one fresh session and one admitted call. The retained process
/// separately enforces its profile, workspace, tool surface and live lease.
#[allow(clippy::too_many_arguments)]
pub async fn run<H: Host, C: Capture>(
    wire: &mut Octos,
    profile_id: &str,
    session_id: &str,
    workspace: &str,
    turn: &str,
    scope: &Scope,
    dispatch: &str,
    input: &str,
    queue: &ApprovalQueue,
    started: Option<tokio::sync::oneshot::Sender<()>>,
    mut controls: Option<crate::run_controls::Stream>,
    room_tools: Option<hagency_runtime::room_tools::RoomTools>,
    host: &mut H,
    capture: &mut C,
) -> Result<super::claude_turn::Outcome> {
    let policy = wire.shared_policy().map_err(|_| Error::Protocol("Octos shared policy"))?;
    wire.open(session_id, profile_id, workspace, policy.permissions())
        .await
        .map_err(|_| Error::Protocol("Octos shared session"))?;
    let mut tools = match (room_tools, queue.room.as_ref()) {
        (Some(tools), Some(room)) if room.matches(scope, dispatch) => octos_room_tools(tools)?,
        (None, None) => Vec::new(),
        _ => return Err(Error::Protocol("room tool dispatch differs")),
    };
    if policy.tools() {
        host.bind_shell(wire.shared_shell().map_err(|_| Error::Protocol("Octos shell scope"))?)?;
        tools.push(octos::session::HostTool {
            name: "hagency.shell".into(),
            description: "Run a command in the authorized room workspace. Trusted system Apple Command Line Tools may be available read-only; commands cannot read private configuration or use the network.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{
                "command":{"type":"string"},"timeout_secs":{"type":"integer","minimum":1,"maximum":90}},
                "required":["command"],"additionalProperties":false}),
            read_only: false,
        });
        tools.push(octos::session::HostTool {
            name: "hagency.exec_command".into(),
            description: "Run a command in the room workspace. Trusted system Apple Command Line Tools may be available read-only; set tty for an interactive terminal and use hagency.write_stdin to continue a running session. Commands cannot use the network.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{
                "cmd":{"type":"string"},"workdir":{"type":"string"},"tty":{"type":"boolean"},
                "yield_time_ms":{"type":"integer","minimum":0,"maximum":30000},
                "timeout_secs":{"type":"integer","minimum":1,"maximum":1800},
                "max_output_tokens":{"type":"integer","minimum":1,"maximum":16384}},
                "required":["cmd"],"additionalProperties":false}),
            read_only: false,
        });
        tools.push(octos::session::HostTool {
            name: "hagency.write_stdin".into(),
            description: "Send input to or collect output from a running hagency.exec_command session.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{
                "session_id":{"type":"string"},"chars":{"type":"string"},
                "yield_time_ms":{"type":"integer","minimum":0,"maximum":30000},
                "max_output_tokens":{"type":"integer","minimum":1,"maximum":16384}},
                "required":["session_id"],"additionalProperties":false}),
            read_only: false,
        });
        if wire.supports_host_patch_worker() {
            tools.push(octos::session::HostTool {
                name: "hagency.apply_patch".into(),
                description: "Apply a Codex-format multi-file patch inside the room workspace. Accepts a patch envelope or path and unified diff.".into(),
                input_schema: serde_json::json!({"type":"object","properties":{
                    "patch":{"type":"string"},"path":{"type":"string"},"diff":{"type":"string"}},
                    "additionalProperties":false}),
                read_only: false,
            });
        }
    }
    if queue.questions.is_some() {
        tools.push(octos::session::HostTool {
            name: "hagency.request_user_input".into(),
            description: "Ask room participants one or more questions and wait for their answers; use for interactive choices or free-form input.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{
                "questions":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"object",
                    "properties":{"id":{"type":"string"},"header":{"type":"string"},
                        "question":{"type":"string"},"options":{"type":"array","items":{"type":"object",
                            "properties":{"label":{"type":"string"},"description":{"type":"string"}},
                            "required":["label","description"]}},"isOther":{"type":"boolean"}},
                    "required":["id","header","question"]}}},"required":["questions"],"additionalProperties":false}),
            read_only: true,
        });
    }
    if queue.live_text.as_ref().is_some_and(|output| output.has_messages()) {
        tools.push(octos_progress_tool());
    }
    wire.register_shared_tools(&tools)
        .await
        .map_err(|_| Error::Protocol("Octos scoped tools"))?;
    if let Some(output) = &queue.live_text {
        wire.enable_live_text(output.clone())
            .map_err(|_| Error::Protocol("Octos live text"))?;
    }
    host.start(wire, turn, input).await?;
    capture.bind(wire).await?;
    wire.enable_approval_control(octos::session::ApprovalControlPolicy {
        owner_wait_ms: if policy.tools() { 105_000 } else { 5_000 },
        response_reserve_ms: 10_000,
    })
    .map_err(|_| Error::Protocol("Octos control"))?;
    if let Some(stream) = &mut controls {
        stream.activate();
    }
    if let Some(started) = started {
        let _ = started.send(());
    }
    let mut buffered: VecDeque<(Event, Option<Observation>)> = VecDeque::new();
    let mut steers = OctosSteers::default();
    let mut stopping = false;
    let (mut requests, mut returns) = (BTreeSet::new(), BTreeSet::new());
    for _ in 0..16384 {
        let (event, observation) = loop {
            if let Some(event) = buffered.pop_front() {
                break event;
            }
            let never = std::future::pending::<()>();
            tokio::pin!(never);
            match octos_wait(wire, never.as_mut(), &mut controls).await? {
                OctosWait::Event(event) => break (event, wire.last_observation().cloned()),
                OctosWait::Control(action) => {
                    if octos_control(wire, action, &mut steers).await? {
                        stopping = true;
                        controls.take();
                    }
                }
                OctosWait::Host(()) => unreachable!(),
            }
        };
        let observation = observation.ok_or(Error::Unknown)?;
        capture.observe(&observation).await?;
        match event {
            Event::Approval { approval_id, params, .. } => {
                if stopping { continue; }
                let deadline = wire.approval_deadline(&approval_id)
                    .map_err(|_| Error::Protocol("Octos approval deadline"))?;
                let decision = async {
                    tokio::time::timeout_at(
                        deadline - Duration::from_secs(1),
                        host.permission(&approval_id, session_id, params),
                    ).await.unwrap_or(false)
                };
                tokio::pin!(decision);
                let allow = loop {
                    match octos_wait(wire, decision.as_mut(), &mut controls).await? {
                        OctosWait::Host(allow) => break Some(allow),
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let withdrawn = matches!(&event, Event::ApprovalSettled { approval_id: id } if id == &approval_id)
                                || matches!(&event, Event::Idle(_));
                            if buffered.len() >= 256 { return Err(Error::Unknown); }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if withdrawn { break None; }
                        }
                    }
                };
                let Some(allow) = allow else { continue; };
                let mut prepared = wire.prepare_approval(&approval_id, if allow {
                    octos::session::PermissionDecision::Allow
                } else { octos::session::PermissionDecision::Deny })
                    .map_err(|_| Error::Protocol("Octos decision"))?;
                loop {
                    match wire.send_prepared_approval(&mut prepared).await
                        .map_err(|_| Error::Protocol("Octos approval write"))? {
                        octos::session::PreparedUpdate::WriteAccepted(_) => break,
                        octos::session::PreparedUpdate::Event(event) => {
                            if buffered.len() >= 256 { return Err(Error::Unknown); }
                            buffered.push_back((event, wire.last_observation().cloned()));
                        }
                    }
                }
            }
            Event::Tool {
                tool_call_id,
                ended,
                ..
            } => {
                if ended {
                    returns.insert(tool_call_id);
                } else {
                    requests.insert(tool_call_id);
                }
            }
            Event::Question {
                question_id,
                turn_id,
                questions,
            } => {
                if stopping || !wire.question_pending(&question_id) {
                    continue;
                }
                let waiter = async {
                    match &queue.questions {
                        Some(channel) => {
                            channel
                                .ask(
                                    scope,
                                    dispatch,
                                    question_id.clone(),
                                    questions,
                                    tokio::time::Instant::now() + Duration::from_secs(300),
                                )
                                .await
                        }
                        None => None,
                    }
                };
                tokio::pin!(waiter);
                let answer = loop {
                    match octos_wait(wire, waiter.as_mut(), &mut controls).await? {
                        OctosWait::Host(answer) => break Some(answer),
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let withdrawn = matches!(&event, Event::QuestionCancelled {question_id:id} if id==&question_id)
                                || matches!(&event, Event::TurnEnded {turn_id:id,..} if id==&turn_id)
                                || matches!(&event, Event::Idle(_));
                            if buffered.len() >= 256 {
                                return Err(Error::Unknown);
                            }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if withdrawn {
                                break None;
                            }
                        }
                    }
                };
                if let Some(submitted) = answer {
                    let empty = Default::default();
                    let answers = submitted
                        .as_ref()
                        .map(|answer| &answer.answers)
                        .unwrap_or(&empty);
                    let accepted = wire
                        .answer_question(&question_id, answers)
                        .await
                        .map_err(|_| Error::Protocol("Octos answer"))?;
                    if accepted {
                        if let Some(answer) = submitted {
                            answer.accepted();
                        }
                    }
                }
            }
            Event::SteerDropped { turn_id, inputs } if turn_id == turn => steers.dropped(inputs)?,
            Event::TurnEnded { turn_id, .. } if turn_id == turn => steers.consumed(),
            Event::ToolCall { params } => {
                if params["call_id"]
                    .as_str()
                    .is_some_and(|id| !wire.host_tool_pending(id))
                {
                    continue;
                }
                let call = wire
                    .host_tool_call(&params)
                    .map_err(|_| Error::Protocol("Octos host tool"))?;
                if call.name == "hagency.report_progress" {
                    if stopping { continue; }
                    let reply = octos_progress(&call, queue.live_text.as_ref());
                    wire.host_tool_result(&call.call_id, reply).await
                        .map_err(|_| Error::Protocol("Octos progress response"))?;
                    continue;
                }
                if call.name == "hagency.request_user_input" {
                    if stopping { continue; }
                    let request = match host_question_request(session_id, turn, &call.call_id, &call.args) {
                        Ok(request) => request,
                        _ => {
                            wire.host_tool_result(&call.call_id, Err("invalid_user_input_request".into())).await
                                .map_err(|_| Error::Protocol("Octos question result"))?;
                            continue;
                        }
                    };
                    let waiter = async {
                        match &queue.questions {
                            Some(channel) => channel.ask(scope, dispatch, format!("octos-host-{}", call.call_id),
                                request.host_questions(), tokio::time::Instant::now() + Duration::from_secs(160)).await,
                            None => None,
                        }
                    };
                    tokio::pin!(waiter);
                    let answer = loop {
                        match octos_wait(wire, waiter.as_mut(), &mut controls).await? {
                            OctosWait::Host(answer) => break Some(answer),
                            OctosWait::Control(action) => {
                                if octos_control(wire, action, &mut steers).await? {
                                    stopping = true; controls.take(); break None;
                                }
                            }
                            OctosWait::Event(event) => {
                                let ended = matches!(&event, Event::TurnEnded { .. } | Event::Idle(_))
                                    || matches!(&event, Event::ToolCancelled {call_id} if call_id == &call.call_id);
                                if buffered.len() >= 256 { return Err(Error::Unknown); }
                                buffered.push_back((event, wire.last_observation().cloned()));
                                if ended { break None; }
                            }
                        }
                    };
                    if let Some(answer) = answer && wire.host_tool_pending(&call.call_id) {
                        let response = match &answer {
                            Some(submitted) if !submitted.answers.is_empty() => request.answer_host(&submitted.answers)
                                .map_err(|_| Error::Protocol("Octos question answer"))?,
                            _ => request.unanswered(),
                        };
                        let accepted = wire.host_tool_result(&call.call_id, Ok(response.result().clone())).await
                            .map_err(|_| Error::Protocol("Octos question result"))?;
                        if accepted && let Some(submitted) = answer { submitted.accepted(); }
                    }
                    continue;
                }
                if matches!(call.name.as_str(), "hagency.shell" | "hagency.exec_command" | "hagency.write_stdin" | "hagency.apply_patch") && policy.tools() {
                    if stopping { continue; }
                    let tool = call.name.clone();
                    let call_id = call.call_id.clone();
                    let args = call.args.clone();
                    let work = async {
                        match tool.as_str() {
                            "hagency.shell" => host.shell(session_id, &call_id, args).await,
                            "hagency.exec_command" => host.exec_command(session_id, &call_id, args).await,
                            "hagency.write_stdin" => host.write_stdin(session_id, &call_id, args).await,
                            _ => host.apply_patch(session_id, &call_id, args).await,
                        }
                    };
                    tokio::pin!(work);
                    let reply = loop {
                        match octos_wait(wire, work.as_mut(), &mut controls).await? {
                            OctosWait::Host(reply) => break wire.host_tool_pending(&call.call_id).then_some(reply),
                            OctosWait::Control(action) => {
                                if octos_control(wire, action, &mut steers).await? {
                                    stopping = true; controls.take(); break None;
                                }
                            }
                            OctosWait::Event(event) => {
                                let ended = matches!(&event, Event::TurnEnded { .. } | Event::Idle(_))
                                    || matches!(&event, Event::ToolCancelled {call_id} if call_id == &call.call_id);
                                if buffered.len() >= 256 { return Err(Error::Unknown); }
                                buffered.push_back((event, wire.last_observation().cloned()));
                                if ended { break None; }
                            }
                        }
                    };
                    if let Some(reply) = reply {
                        wire.host_tool_result(&call.call_id, reply).await
                            .map_err(|_| Error::Protocol("Octos shell result"))?;
                    }
                    continue;
                }
                let operation = room_tools.and_then(|tools| {
                    call.name
                        .strip_prefix("hagency.")
                        .and_then(|name| tools.parse(name, &call.args).ok())
                });
                let Some((room, operation)) = queue.room.as_ref().zip(operation) else {
                    wire.host_tool_result(&call.call_id, Err("unsupported_host_tool".into()))
                        .await
                        .map_err(|_| Error::Protocol("Octos tool response"))?;
                    continue;
                };
                if stopping {
                    continue;
                }
                let work = room.call(
                    operation,
                    call.call_id.clone(),
                    tokio::time::Instant::now() + Duration::from_secs(55),
                );
                tokio::pin!(work);
                let reply = loop {
                    match octos_wait(wire, work.as_mut(), &mut controls).await? {
                        OctosWait::Host(reply) => {
                            break wire.host_tool_pending(&call.call_id).then_some(reply);
                        }
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let ended = matches!(&event, Event::TurnEnded { .. } | Event::Idle(_))
                                || matches!(&event, Event::ToolCancelled {call_id} if call_id==&call.call_id);
                            if buffered.len() >= 256 {
                                return Err(Error::Unknown);
                            }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if ended {
                                break None;
                            }
                        }
                    }
                };
                if let Some(reply) = reply {
                    let result = if reply.is_error() {
                        Err(reply.text().to_owned())
                    } else {
                        Ok(serde_json::from_str(reply.text())
                            .unwrap_or_else(|_| serde_json::Value::String(reply.text().into())))
                    };
                    wire.host_tool_result(&call.call_id, result)
                        .await
                        .map_err(|_| Error::Protocol("Octos tool response"))?;
                }
            }
            Event::Idle(idle) => {
                let usage = match observation.kind() {
                    ObservationKind::Idle(evidence) => match (
                        evidence.input(),
                        evidence.cache_read(),
                        evidence.cache_write(),
                        evidence.output(),
                    ) {
                        (Some(input), Some(cache), Some(write), Some(output)) => Usage {
                            input: input
                                .checked_add(cache)
                                .and_then(|v| v.checked_add(write))
                                .ok_or(Error::Unknown)?,
                            output,
                            cached_input: cache,
                            cache_write_input: Some(write),
                            reasoning_output: 0,
                            accounting_version: "octos-reported-v1".into(),
                        },
                        _ => crate::unreported_usage(),
                    },
                    _ => crate::unreported_usage(),
                };
                let text = idle.reply.unwrap_or_default();
                capture.complete(wire, &text).await?;
                return Ok(super::claude_turn::Outcome {
                    text,
                    usage,
                    status: match idle.outcome {
                        octos::Outcome::Completed => TurnStatus::Completed,
                        octos::Outcome::Interrupted => TurnStatus::Interrupted,
                        _ => TurnStatus::Failed,
                    },
                    requests: requests.len(),
                    returns: returns.len(),
                    cumulative: false,
                    late_failure: None,
                });
            }
            _ => {}
        }
    }
    Err(Error::Unknown)
}
