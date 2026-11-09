use super::launch::Launch;
use super::attempt_io::{
    Acknowledgement as SharedRuntimeAcknowledgement, AttemptAction,
    Delivery as SharedRuntimeDelivery, Reply as DeliveryReply, ResultInput as SharedProcessInput,
};
use super::io::{AckReply, ActivityReply, TicketReply, WorkReply};
use super::*;
use crate::console::lifecycle::DrainFailure;
use hagency_runtime::owned::{ManagedProcessState, OwnedSession, SharedTextActivityControl};
use tokio::sync::{oneshot, watch};
struct Activity {
    lease: SharedActivityLease,
    until: Instant,
}
pub(super) async fn upload_process(
    b: &Bound,
    d: &SharedRuntimeDelivery,
    until: Instant,
    receipt: &SharedAttemptProcessReceipt,
) -> Result<(SharedRuntimeAcknowledgement, u64, Instant), NativeError> {
    receipt.validate_for(&d.command).map_err(store)?;
    let started = Instant::now();
    let ack: Timed<SharedRuntimeAcknowledgement> = b
        .upload(
            AttemptAction::Result {
                id: d.command.command_id.clone(),
                input: SharedProcessInput {
                    command_digest: d.command_digest.clone(),
                    token: d.token.clone(),
                    receipt: receipt.clone(),
                },
            }
            .into(),
            until,
        )
        .await?;
    if ack.value.observation_id != receipt.observation_id
        || ack.value.receipt_digest != digest(receipt)?
        || !matches!(
            ack.value.state.as_str(),
            "issued" | "qualified" | "stopped" | "unknown" | "fenced" | "cleanup_pending"
        )
    {
        return Err(invalid());
    }
    Ok((ack.value, ack.observed, started))
}
/// Upload end evidence. A time-bound stop that the server's own bound does
/// not yet allow returns false and is retried by a later cleanup poll.
pub(super) async fn upload_end(
    b: &Bound,
    d: &SharedRuntimeDelivery,
    until: Instant,
    receipt: &SharedAttemptProcessReceipt,
) -> Result<bool, NativeError> {
    match upload_process(b, d, until, receipt).await {
        Ok(_) => Ok(true),
        Err(e) if receipt.reason.is_some() && e.code == "shared_attempt_timeout_pending" => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}
/// Upload a demand attempt's not_started evidence through the demand retry
/// route, which re-offers the same event in place. `false` whenever the route
/// did not record it (no demand, no retry capability, or any refusal by the
/// server): the caller then uploads the evidence on the plain result route, so
/// evidence is always recorded. Only a failure that may not have reached the
/// server's decision, or a revoked authority, is returned as an error.
pub(super) async fn upload_retry(
    b: &Bound,
    r: &OwnerResourceRegistry,
    d: &SharedRuntimeDelivery,
    until: Instant,
    receipt: &SharedAttemptProcessReceipt,
) -> Result<bool, NativeError> {
    receipt.validate_for(&d.command).map_err(store)?;
    let a = b.cleanup(r, &d.command.original_runtime_command).await?;
    let Ok(Some((offer, n))) = r.shared_demand_for_attempt(a, d.command.clone()).await else {
        return Ok(false);
    };
    match super::demand_io::retries(b).await {
        Ok(true) => {}
        Err(e) if undecided(&e) => return Err(e),
        Ok(false) | Err(_) => return Ok(false),
    }
    let reply: Timed<super::demand_io::RetryReply> = match b
        .upload(
            super::demand_io::DemandAction::Retry {
                id: offer.demand_id.clone(),
                input: super::demand_io::RetryInput {
                    demand_digest: offer.digest().map_err(store)?,
                    expected_control_revision: offer.control_revision,
                    retry: n + 1,
                    attempt_id: d.command.command_id.clone(),
                    result: SharedProcessInput {
                        command_digest: d.command_digest.clone(),
                        token: d.token.clone(),
                        receipt: receipt.clone(),
                    },
                },
            }
            .into(),
            until,
        )
        .await
    {
        Ok(v) => v,
        Err(e) if undecided(&e) => return Err(e),
        // Refused (gate off, demand moved on, stop): report the evidence alone.
        Err(_) => return Ok(false),
    };
    let ack = &reply.value.acknowledgement;
    reply.value.demand.validate()?;
    if ack.observation_id != receipt.observation_id
        || ack.receipt_digest != digest(receipt)?
        || ack.state != "stopped"
        || reply.value.demand.offer.demand_id != offer.demand_id
    {
        return Err(invalid());
    }
    Ok(true)
}
/// The request may not have reached the server's decision, or authority was
/// revoked: neither is a refusal that permits another route. A server storage
/// failure (503 storage_unavailable, which includes trigger exceptions) is
/// also undecided: falling back would end the message instead of retrying it.
fn undecided(e: &NativeError) -> bool {
    matches!(
        e.code.as_str(),
        "server_unavailable"
            | "storage_unavailable"
            | "invalid_server_response"
            | "server_request_failed"
            | "management_outcome_unknown"
            | "allocation_ticket_expired"
            | "management_scope_changed"
            | "owner_authorization_required"
            | "sign_in_required"
            | "device_authorization_required"
    )
}
fn activity(
    sink: &SharedAttemptProcessSink,
    lease: SharedActivityLease,
    observed: u64,
    started: Instant,
    b: &Bound,
) -> Result<Activity, NativeError> {
    lease.authority.validate().map_err(store)?;
    if lease.command_digest != digest(sink.command())?
        || lease.authority.runtime_command_id != sink.command().command_id
        || lease.process_digest != sink.process_digest().map_err(store)?
    {
        return Err(invalid());
    }
    let until = io::window(
        &lease.authority.token,
        lease.expires_at_ms,
        observed,
        started,
        &b.scope,
    )?;
    if Instant::now() >= until {
        return Err(failure(409, "shared_execution_expired"));
    }
    Ok(Activity {
        lease,
        until,
    })
}
async fn proof(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    a: &Activity,
) -> Result<(Bound, VerifiedSharedActivity), NativeError> {
    let b = io.bound().await?;
    let e = b
        .enrollment(r, &sink.command().original_runtime_command, true)
        .await?;
    let verified = sink
        .authorize_activity(e, &a.lease, a.until.min(b.scope.deadline))
        .map_err(store)?;
    Ok((b, verified))
}
async fn renew(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    control: &SharedTextActivityControl,
    a: &mut Activity,
) -> Result<(), NativeError> {
    let (b, verified) = proof(io, r, sink, a).await?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| failure(503, "native_random_unavailable"))?;
    let id = format!(
        "sar_{}",
        random
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>()
    );
    r.prepare_shared_renewal(verified, id.clone())
        .await
        .map_err(store)?;
    let started = Instant::now();
    let renewal = SharedActivityRenewInput {
        authority: a.lease.authority.clone(),
        renewal_id: id,
    };
    let (b, reply): (Bound, Timed<ActivityReply>) = io
        .upload_renewal(b, || Action::Renew(renewal.clone()), a.until)
        .await?;
    let next = activity(
        sink,
        reply.value.activity,
        reply.observed,
        started,
        &b,
    )?;
    if next.lease.authority.epoch <= a.lease.authority.epoch {
        return Err(invalid());
    }
    control
        .renew_lease(next.lease.authority.epoch, next.until)
        .map_err(|_| failure(409, "shared_execution_expired"))?;
    *a = next;
    Ok(())
}
async fn maybe_renew(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    control: &SharedTextActivityControl,
    a: &mut Activity,
) -> Result<(), NativeError> {
    if a.until.saturating_duration_since(Instant::now()) <= Duration::from_secs(20) {
        renew(io, r, sink, control, a).await?
    }
    Ok(())
}
// Matrix recovery and provider thread setup can outlive the first 30-second
// lease. Keep the SAME future and process alive; refresh the proof only after
// local preparation, before admitting the one model call.
#[allow(clippy::too_many_arguments)]
async fn prepare_while_active<T>(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    control: &SharedTextActivityControl,
    a: &mut Activity,
    cancel: &mut watch::Receiver<bool>,
    preparation: impl std::future::Future<Output = Result<T, NativeError>>,
) -> Result<T, NativeError> {
    tokio::pin!(preparation);
    loop {
        let wake = a
            .until
            .checked_sub(Duration::from_secs(20))
            .unwrap_or_else(Instant::now);
        tokio::select! { biased;
            _ = cancel.changed() => return Err(failure(409, "shared_execution_closed")),
            _ = tokio::time::sleep_until(a.until.into()) => return Err(failure(409, "shared_execution_expired")),
            // Poll once under the proof that admitted this operation before
            // renewing its epoch. The provider acknowledgement may then wait
            // across later renewals without rechecking that superseded proof.
            result = &mut preparation => return result,
            _ = tokio::time::sleep_until(wake.into()) => {
                guarded(cancel, a.until, || maybe_renew(io,r,sink,control,a)).await?;
            }
        }
    }
}
// A start acknowledgement belongs to the same pending dispatch. On stop or
// renewal failure, retain it briefly so final usage can still attach and drain.
#[allow(clippy::too_many_arguments)]
async fn start_while_active(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    control: &SharedTextActivityControl,
    a: &mut Activity,
    cancel: &mut watch::Receiver<bool>,
    start: impl std::future::Future<Output = Result<(), NativeError>>,
) -> Result<Option<NativeError>, NativeError> {
    if super::stop_capture::requested(cancel) {
        return Err(failure(409, "shared_execution_closed"));
    }
    tokio::pin!(start);
    loop {
        let wake = a
            .until
            .checked_sub(Duration::from_secs(20))
            .unwrap_or_else(Instant::now);
        let error = tokio::select! { biased;
            _ = cancel.changed() => failure(409, "shared_execution_closed"),
            _ = tokio::time::sleep_until(a.until.into()) => failure(409, "shared_execution_expired"),
            // Begin under the exact admitted proof before advancing its epoch.
            result = &mut start => return result.map(|()| None),
            _ = tokio::time::sleep_until(wake.into()) => {
                match guarded(cancel,a.until,|| maybe_renew(io,r,sink,control,a)).await {
                    Ok(()) => continue,
                    Err(error) => error,
                }
            }
        };
        if sink.source().turn_dispatch_started() && Instant::now() < a.until {
            let deadline = a.until.min(Instant::now() + Duration::from_secs(3));
            if let Ok(Ok(())) = tokio::time::timeout_at(deadline.into(), start.as_mut()).await {
                return Ok(Some(error));
            }
        }
        return Err(error);
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    r: OwnerResourceRegistry,
    custody: SharedAttemptCustody,
    launch: Launch,
    io: Io,
    d: SharedRuntimeDelivery,
    observed: u64,
    started: Instant,
    until: Instant,
    mut cancel: watch::Receiver<bool>,
    sender: oneshot::Sender<Result<SharedExecutionProgress, NativeError>>,
    telemetry: crate::console::native::telemetry::OwnerTelemetry,
) -> Result<(), DrainFailure> {
    let launch = match launch {
        Launch::Codex(launch) => launch,
        Launch::Claude(launch) => {
            return super::claude_run::run(
                r,
                super::claude_run::Claimed::Attempt(Box::new((custody, d))),
                launch,
                io,
                observed,
                started,
                until,
                cancel,
                sender,
            )
            .await;
        }
    };
    let mut sender = Some(sender);
    let mut process = match launch.spawn(until) {
        Ok(v) => v,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    let sink = match custody.bind_process(process.shared_text_process_source()) {
        Ok(v) => Arc::new(v),
        Err(e) => {
            process.stop();
            let _ = sender.take().unwrap().send(Err(store(e)));
            return Err(DrainFailure::Unknown);
        }
    };
    let result = guarded(&mut cancel, until, || async {
        io.check_running(&d.command.original_runtime_command)
            .await?;
        process
            .initialize()
            .await
            .map_err(|_| failure(409, "shared_execution_initialize_failed"))?;
        io.check_running(&d.command.original_runtime_command)
            .await?;
        let observation = process
            .qualify_shared_tools(
                launch.instructions(),
                d.command.web_search,
                d.command.tool_policy.as_ref().map(|p| p.mode.as_str()),
            )
            .await
            .map_err(|_| failure(409, "shared_execution_identity_failed"))?;
        let receipt = sink.record(observation).await.map_err(store)?;
        let b = io.bound().await?;
        io::binding(&b.scope, &d.command.original_runtime_command, true)?;
        let (ack, at, sent) = upload_process(&b, &d, until, &receipt).await?;
        let lease = ack
            .activity
            .ok_or_else(|| failure(409, "shared_execution_activity_missing"))?;
        let active = activity(&sink, lease, at, sent, &b)?;
        process
            .install_renewable_shared_text_activity(active.lease.authority.epoch, active.until)
            .map_err(|_| failure(409, "shared_execution_expired"))?;
        Ok(active)
    })
    .await;
    let result = match result {
        Ok(mut active) => {
            let _ = sender
                .take()
                .unwrap()
                .send(Ok(SharedExecutionProgress::Running {
                    command_id: d.command.command_id.clone(),
                }));
            match process.shared_text_activity_control() {
                Ok(control) => {
                    work_loop(
                        &io,
                        &r,
                        &sink,
                        &control,
                        &mut active,
                        &mut process,
                        &mut cancel,
                        &telemetry,
                    )
                    .await
                }
                Err(_) => Err(failure(409, "shared_execution_closed")),
            }
        }
        Err(e) => Err(e),
    };
    if let Some(sender) = sender.take() {
        let _ = sender.send(
            result
                .as_ref()
                .map_err(|e| failure(e.status, &e.code))
                .map(|_| SharedExecutionProgress::Recorded {
                    command_id: d.command.command_id.clone(),
                    observation_id: String::new(),
                    process_state: None,
                }),
        );
    }
    if let Err(error) = &result {
        tracing::warn!(code = %error.code, "Shared text process ending after an operation failure");
    }
    // Original process cleanup is retained even when SDK/business authority is
    // gone. Upload may fail, but local evidence is never discarded or recreated.
    let observation = process.stop_shared_text_observation();
    let stopped = observation.state() == ManagedProcessState::Stopped;
    let receipt = sink
        .record(observation)
        .await
        .map_err(|_| DrainFailure::Unknown)?;
    let _ = report_process(&io, &r, &d.command, &receipt).await;
    if stopped {
        Ok(())
    } else {
        Err(DrainFailure::Unknown)
    }
}
async fn work_loop(
    io: &Io,
    r: &OwnerResourceRegistry,
    sink: &Arc<SharedAttemptProcessSink>,
    control: &SharedTextActivityControl,
    a: &mut Activity,
    process: &mut OwnedSession,
    cancel: &mut watch::Receiver<bool>,
    telemetry: &crate::console::native::telemetry::OwnerTelemetry,
) -> Result<(), NativeError> {
    let admission = io.hosting_admission()?;
    let (work, mut turn) = loop {
        guarded(cancel, a.until, || maybe_renew(io, r, sink, control, a)).await?;
        let Some(turn) = admission.enter_turn() else {
            guarded(cancel, a.until, || async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(())
            })
            .await?;
            continue;
        };
        let (b, verified) = proof(io, r, sink, a).await?;
        let reply: Timed<WorkReply> = guarded(cancel, a.until, || {
            b.upload(Action::WorkPoll(a.lease.authority.clone()), a.until)
        })
        .await?;
        if let Some(w) = reply.value.work {
            w.validate().map_err(store)?;
            if cfg!(feature = "shared-intake") {
                super::demands::bind_work(
                    &b,
                    r,
                    &sink.command().original_runtime_command,
                    &w,
                    verified,
                    a.until,
                )
                .await?;
                let (_, fresh) = proof(io, r, sink, a).await?;
                r.receive_shared_work(fresh, w.clone())
                    .await
                    .map_err(store)?;
            } else {
                r.receive_shared_work(verified, w.clone())
                    .await
                    .map_err(store)?;
            }
            let input = SharedWorkInput {
                authority: a.lease.authority.clone(),
                work_id: w.id.clone(),
                work_digest: digest(&w)?,
            };
            let ack: Timed<AckReply> = guarded(cancel, a.until, || {
                b.upload(Action::WorkAck(input), a.until)
            })
            .await?;
            if !ack.value.acknowledged {
                return Err(invalid());
            }
            break (w, turn);
        }
        drop(turn);
        guarded(cancel, a.until, || async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await?;
    };
    guarded(cancel, a.until, || maybe_renew(io, r, sink, control, a)).await?;
    let (b, p) = proof(io, r, sink, a).await?;
    // Pin the exact V3 plaintext input before selecting the retained native session.
    let plaintext_input = if !work.encrypted {
        Some(guarded(cancel, a.until, || super::context_io::prepare(
            io, &b, r, p, &work, a.lease.authority.clone(), a.until)).await?)
    } else { None };
    let (_, p) = proof(io, r, sink, a).await?;
    let mut native_session = prepare_while_active(io, r, sink, control, a, cancel, async {
        r.prepare_shared_native_session(p, work.clone(), process).await.map_err(store)
    }).await?;
    let (b, _) = proof(io, r, sink, a).await?;
    let mut history = prepare_while_active(
        io,
        r,
        sink,
        control,
        a,
        cancel,
        super::history::Reader::new(io, &b, &work, plaintext_input.as_ref()),
    )
    .await?;
    // Opening a provider thread does not run the model. Finish it before
    // recording call admission, so startup failure cannot reserve the budget.
    prepare_while_active(io, r, sink, control, a, cancel, async {
        io.check_running(&sink.command().original_runtime_command)
            .await?;
        if history.is_some() {
            process
                .enable_shared_room_history()
                .map_err(|_| failure(409, "shared_history_tool_unavailable"))?;
        }
        if history.is_some() && process.shared_tools_enabled() {
            process
                .enable_shared_room_files()
                .map_err(|_| failure(409, "shared_file_tool_unavailable"))?;
        }
        super::questions::offer(process);
        native_session
            .open_codex(process)
            .await
            .map_err(|error| {
                tracing::warn!(
                    ?error,
                    "Shared Agent thread setup failed before call admission"
                );
                failure(409, "shared_execution_thread_unknown")
            })?;
        io.check_running(&sink.command().original_runtime_command)
            .await
    })
    .await?;
    let input = if work.encrypted {
        let text = prepare_while_active(
            io,
            r,
            sink,
            control,
            a,
            cancel,
            super::e2ee::input(io, &b, &work),
        )
        .await?;
        let text = if let Some(history) = history.as_mut() {
            prepare_while_active(io, r, sink, control, a, cancel, async {
                Ok(if native_session.is_resuming() {
                    history.followup_prompt(text)
                } else {
                    history.prompt(text).await
                })
            })
            .await?
        } else {
            text
        };
        let (_, fresh) = proof(io, r, sink, a).await?;
        r.prepare_shared_decrypted_input(fresh, work.clone(), text).await.map_err(store)?
    } else {
        plaintext_input.ok_or_else(|| failure(409, "native_context_protocol_required"))?
    };
    guarded(cancel, a.until, || maybe_renew(io, r, sink, control, a)).await?;
    let mut files = prepare_while_active(
        io,
        r,
        sink,
        control,
        a,
        cancel,
        super::files::Files::new(io, &b, &work, process, Some(&input)),
    )
    .await?;
    let (_, p) = proof(io, r, sink, a).await?;
    let call = r.prepare_shared_input_call(p, work.clone(), input.clone())
        .await.map_err(store)?;
    let call_id = call.call_id.clone();
    let mut run_controls = super::run_controls::Controls::new(&work, &call_id).ok();
    let (b, p) = proof(io, r, sink, a).await?;
    let attempt = r.begin_shared_input_start(p, work.clone(), call.call_id, input.clone())
        .await.map_err(store)?;
    let issued = guarded(cancel, a.until, || {
        super::context_io::start(
            &b,
            r,
            attempt,
            &work,
            &input,
            a.lease.authority.clone(),
            a.until,
        )
    })
    .await?;
    // Central admission and the original custody already checked current
    // authority. Do not insert another network round trip before the effect
    // boundary; start_turn checks that exact proof synchronously before send.
    let start_stop = match start_while_active(io, r, sink, control, a, cancel, async {
        issued.start_turn(process).await.map_err(store)
    })
    .await
    {
        Ok(stop) => stop,
        Err(error) => {
            match issued.finish_failed_start().await {
                Ok(receipt) => {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        report_work(io, r, sink.command(), &work, &receipt),
                    )
                    .await;
                }
                Err(record_error) => {
                    tracing::error!(?record_error, work_id=%work.id, "Failed to persist original call start failure")
                }
            }
            return Err(error);
        }
    };
    turn.started();
    if let Some(controls) = &mut run_controls {
        controls.activate(process);
    }
    let mut capture = issued.bind_started_turn(process).await.map_err(store)?;
    let mut usage = telemetry.shared_turn(
        &sink.command().original_runtime_command,
        &work,
        &call_id,
        process,
    );
    let approvals_enabled = sink
        .command()
        .tool_policy
        .as_ref()
        .is_some_and(|p| p.mode == "standard");
    let mut approvals = super::approvals::Approvals::default();
    let mut questions: super::questions::Questions = super::questions::Questions::default();
    let mut deferred = std::collections::VecDeque::new();
    let mut last_control_poll = Instant::now() - Duration::from_secs(1);
    let terminal_result: Result<SharedWorkReceipt, NativeError> = async {
      if approvals_enabled && start_stop.is_none() {super::approvals::enable(process)?;}
      if start_stop.is_none() {super::questions::enable(process)?;}
      let mut stop_reason = start_stop;
      let terminal = 'turn: loop {
        macro_rules! room_wait {
            ($future:expr) => { room_wait!(|_: &OwnedSession| true, $future) };
            ($admissible:expr, $future:expr) => {{
                match prepare_while_active(io, r, sink, control, a, cancel,
                    super::cooperative::wait($future, io, &sink.command().original_runtime_command, process, &mut capture, &mut usage, &mut run_controls, &mut deferred, $admissible)).await? {
                    super::cooperative::Waiting::Ready(value) => value,
                    super::cooperative::Waiting::Stopped => continue 'turn,
                    super::cooperative::Waiting::Terminal(receipt) => break 'turn receipt,
                }
            }};
        }

        if last_control_poll.elapsed() >= Duration::from_millis(500) {
            last_control_poll = Instant::now();
            if let Some(controls) = &mut run_controls {
                prepare_while_active(io, r, sink, control, a, cancel, controls.poll(io, &sink.command().original_runtime_command, process)).await?;
            }
        }

        if let Some(error) = stop_reason.take() { return Err(error); }
        if super::stop_capture::requested(cancel) {
            return Err(failure(409, "shared_execution_closed"));
        }
        let already_observed;
        let result = if let Some(event) = deferred.pop_front() {
            already_observed = true;
            Ok(Some(event))
        } else {
            already_observed = false;
            // Pin the actual read. Renewal branches keep polling this same
            // future. A requested stop yields cooperatively for final usage.
            let read = process.next_shared_event_or_tick();
            tokio::pin!(read);
            loop {
                // Anchor renewal to the lease, not to the latest stream event.
                // A ready event must not starve an already-due renewal.
                let wake = a.until.checked_sub(Duration::from_secs(20)).unwrap_or_else(Instant::now);
                let event = tokio::select! {biased;
                    _ = cancel.changed()=>break read.as_mut().await.map_err(|_|failure(409,"shared_execution_event_unknown")),
                    _ = tokio::time::sleep_until(a.until.into())=>break Err(failure(409,"shared_execution_expired")),
                    _ = tokio::time::sleep_until(wake.into())=>{guarded(cancel,a.until,|| maybe_renew(io,r,sink,control,a)).await}
                    event=&mut read=>break event.map_err(|error|{tracing::warn!(?error,"Shared Agent event stream ended before completion");failure(409,"shared_execution_event_unknown")}),
                };
                if let Err(e) = event {
                    if Instant::now() < a.until {
                        // Finish the retained read before cleanup, even when
                        // renewal failed. Preserve any last usage event first.
                        stop_reason = Some(e);
                        break read.as_mut().await.map_err(|_|failure(409,"shared_execution_event_unknown"));
                    }
                    break Err(e);
                }
            }
        };
        let event = match result {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let Some(event)=event else {
            if stop_reason.is_some() || super::stop_capture::requested(cancel) { continue; }
            if process.run_controls_hold_answers() { continue; }
            if questions.busy() {
                // Answers and expiries; the lease keeps renewing meanwhile.
                let ready=room_wait!(questions.collect(io))?;
                for response in ready {
                    let id = response.request_id().clone();
                    let delivered = guarded(cancel,a.until,||async{ super::questions::answer(process,response).await }).await;
                    questions.delivered(&id, delivered.as_ref().is_ok_and(|accepted| *accepted));
                    delivered?;
                }
                room_wait!(questions.publish())?;
            }
            if !approvals_enabled { continue; }
            guarded(cancel,a.until,|| maybe_renew(io,r,sink,control,a)).await?;
            let b=io.bound().await?;
            let receipt=guarded(cancel,a.until,|| approvals.poll(io,&b,&work,&a.lease.authority,a.until,process,&mut capture,&mut usage,history.as_mut())).await?;
            // Question updates that arrived while a decision was being sent.
            let updates=approvals.take_questions();
            if let Some(receipt)=receipt {break receipt;}
            // A callback can arrive while an approval response is being written.
            // It was already observed by the sink, but still needs its tool result.
            for call in approvals.take_room_calls() {
                guarded(cancel,a.until,|| io.check_running(&sink.command().original_runtime_command)).await?;
                match call {
                    super::approvals::RoomCall::History(call) => {
                        if !process.room_history_admissible(&call) { continue; }
                        let reader=history.as_mut().ok_or_else(invalid)?;
                        let content=room_wait!(|process: &OwnedSession| process.room_history_admissible(&call),reader.read_call(&call))
                            .unwrap_or_else(|e|serde_json::json!({"complete":false,"error":e.code}).to_string());
                        guarded(cancel,a.until,||async {process.respond_shared_room_history(&call,content).await.map_err(|_|failure(409,"shared_history_response_failed"))}).await?;
                    }
                    super::approvals::RoomCall::File(call) => {
                        if !process.room_file_admissible(&call) { continue; }
                        let reader=history.as_ref().ok_or_else(invalid)?;
                        let files=files.as_mut().ok_or_else(invalid)?;
                        let content=room_wait!(|process: &OwnedSession| process.room_file_admissible(&call), files.handle(&call.operation,reader));
                        guarded(cancel,a.until,||async {process.respond_shared_room_file(&call,content).await.map_err(|_|failure(409,"shared_file_response_failed"))}).await?;
                    }
                }
            }

            // In arrival order: a question withdrawn in the same batch is never shown.
            for update in super::questions::in_order(updates) {
                match update {
                    super::questions::QuestionUpdate::Withdrawn(id) => questions.withdrawn(&id),
                    super::questions::QuestionUpdate::Asked(request) => {
                        let question_scope = request.clone();
            if let Some(response)=room_wait!(|process: &OwnedSession| process.user_input_waiting(&question_scope), questions.ask(io,&work,request))? {
                            guarded(cancel,a.until,||async{ super::questions::answer(process,response).await }).await?;
                        }
                    }
                }
            }
            continue;
        };
        let approval=event.approval().cloned();
        let question=event.user_input().cloned();
        let withdrawn=event.user_input_resolved().cloned();
        let history_call=event.room_history_call().cloned();
        let file_call=event.room_file_call().cloned();
        if let (Some(history), Some(context)) = (history.as_mut(), event.observation().context_usage()) { history.observe_context(context); }
        let ended = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let recorded = if already_observed { None } else {
            let pending = usage.pending(&event);
        let recorded = match capture.observe(event).await {
            Ok(v) => v,
            Err(e) => {
                usage.abandon();
                let _ = capture.record_unknown().await;
                return Err(store(e));
            }
        };
            // Deferred callbacks have already crossed this exact sink once.
            usage.accept(pending);
        usage.quota(process); recorded
        };
        if ended {
            break recorded.ok_or_else(invalid)?;
        }
        if stop_reason.is_some() { continue; }
        if let Some(call)=file_call && process.room_file_admissible(&call) {
            let reader=history.as_ref().ok_or_else(invalid)?;
            let files=files.as_mut().ok_or_else(invalid)?;
            guarded(cancel,a.until,|| io.check_running(&sink.command().original_runtime_command)).await?;
            let content=room_wait!(|process: &OwnedSession| process.room_file_admissible(&call), files.handle(&call.operation,reader));
            guarded(cancel,a.until,|| io.check_running(&sink.command().original_runtime_command)).await?;
            guarded(cancel,a.until,||async { process.respond_shared_room_file(&call,content).await.map_err(|_|failure(409,"shared_file_response_failed")) }).await?;
        }
        if let Some(call)=history_call && process.room_history_admissible(&call) {
            let reader=history.as_mut().ok_or_else(invalid)?;
            guarded(cancel,a.until,|| io.check_running(&sink.command().original_runtime_command)).await?;
            let content=room_wait!(|process: &OwnedSession| process.room_history_admissible(&call),reader.read_call(&call))
                .unwrap_or_else(|error| serde_json::json!({"complete":false,"error":error.code}).to_string());
            guarded(cancel,a.until,|| io.check_running(&sink.command().original_runtime_command)).await?;
            guarded(cancel,a.until,||async{ process.respond_shared_room_history(&call,content).await.map_err(|_|failure(409,"shared_history_response_failed")) }).await?;
        }
        if let Some(request)=approval {
            let b=io.bound().await?;
            guarded(cancel,a.until,|| approvals.register(io,&b,&work,&a.lease.authority,a.until,process,request)).await?;
        }
        if let Some(id)=withdrawn { questions.withdrawn(&id); }
        if let Some(request)=question {
            // The card goes to the room; the model waits for its answer.
            let question_scope = request.clone();
            if let Some(response)=room_wait!(|process: &OwnedSession| process.user_input_waiting(&question_scope), questions.ask(io,&work,request))? {
                guarded(cancel,a.until,||async{ super::questions::answer(process,response).await }).await?;
            }
        }
      };
      Ok(terminal)
    }.await;
    if let Some(controls) = &mut run_controls {
        controls.finish(process, &terminal_result).await;
    }
    // Cards still waiting when the turn ended close unanswered.
    questions.close();
    let terminal = match terminal_result {
        Ok(receipt) => receipt,
        Err(error) => {
            let receipt =
                super::stop_capture::settle(process, &mut capture, &mut usage, a.until).await;
            if let Some(receipt) = receipt {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    report_work(io, r, sink.command(), &work, &receipt),
                )
                .await;
            } else {
                let _ = capture.record_unknown().await;
            }
            // Final usage first; then show the closed cards, best effort.
            let _ = tokio::time::timeout(Duration::from_secs(3), questions.publish()).await;
            return Err(error);
        }
    };
    if terminal.state == "completed" && terminal.usage_complete
        && let Err(error) = capture.retain_native_session(native_session, process).await
    {
        tracing::warn!(?error, "Completed native context could not be retained");
    }

    if questions.busy() {
        // Cosmetic only: never hold the reply back for long.
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            prepare_while_active(
                io,
                r,
                sink,
                control,
                a,
                cancel,
                Box::pin(questions.publish()),
            ),
        )
        .await;
    }
    // Reply delivery uses the central durable Matrix transaction. Keep this same
    // process/activity alive while it is pending; do not make a successor turn.
    loop {
        guarded(cancel, a.until, || maybe_renew(io, r, sink, control, a)).await?;
        let status = match guarded(cancel, a.until, || {
            report_work(io, r, sink.command(), &work, &terminal)
        })
        .await
        {
            Ok(status) => status,
            // A lost acknowledgement is not a reason to fence a reply that the
            // central transaction may still deliver. Retry only saved evidence,
            // under this same live process; never repeat the model turn.
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "server_unavailable"
                        | "server_request_failed"
                        | "management_outcome_unknown"
                        | "allocation_ticket_expired"
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        if matches!(status.as_deref(), Some("sent" | "blocked")) {
            return Ok(());
        }
        guarded(cancel, a.until, || async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await?;
    }
}
pub(super) async fn recover_process_work(
    io: &Io,
    r: &OwnerResourceRegistry,
    c: &SharedAttemptCommand,
) -> Result<(), NativeError> {
    let b = io.bound().await?;
    let a = b.cleanup(r, &c.original_runtime_command).await?;
    let ids = r
        .shared_attempt_work_ids(a, c.clone())
        .await
        .map_err(store)?;
    b.transport.check_scope(&b.scope, false).await?;
    for id in ids {
        let b = io.bound().await?;
        let a = b.cleanup(r, &c.original_runtime_command).await?;
        let saved = r
            .recover_shared_attempt_work(a, c.clone(), id, None)
            .await
            .map_err(store)?;
        b.transport.check_scope(&b.scope, false).await?;
        if let Some(saved) = saved
            && let Some(receipt) = saved.receipt
        {
            report_work(io, r, c, &saved.work, &receipt).await?;
        }
    }
    Ok(())
}
pub(super) async fn report_process(
    io: &Io,
    r: &OwnerResourceRegistry,
    c: &SharedAttemptCommand,
    receipt: &SharedAttemptProcessReceipt,
) -> Result<(), NativeError> {
    let b = io.bound().await?;
    let a = b.cleanup(r, &c.original_runtime_command).await?;
    let saved = r
        .recover_shared_attempt_process(a, c.clone(), Some(receipt.observation_id.clone()))
        .await
        .map_err(store)?
        .ok_or_else(invalid)?;
    if digest(&saved)? != digest(receipt)? {
        return Err(invalid());
    }
    let started = Instant::now();
    let reply: Timed<DeliveryReply> = b
        .request(
            AttemptAction::Recover {
                id: c.command_id.clone(),
                digest: digest(c)?,
                observation: Some(receipt.observation_id.clone()),
            }
            .into(),
            false,
        )
        .await?;
    let d = reply.value.delivery.ok_or_else(invalid)?;
    if d.execution_allowed || digest(&d.command)? != digest(c)? || d.command_digest != digest(c)? {
        return Err(invalid());
    }
    if let Some(old) = &d.recorded {
        if digest(old)? != digest(receipt)? {
            return Err(invalid());
        }
        return Ok(());
    }
    let until = io::window(
        &d.token,
        d.lease_until_ms,
        reply.observed,
        started,
        &b.scope,
    )?;
    upload_process(&b, &d, until, receipt).await?;
    Ok(())
}
pub(super) async fn report_work(
    io: &Io,
    r: &OwnerResourceRegistry,
    c: &SharedAttemptCommand,
    work: &SharedWorkItem,
    receipt: &SharedWorkReceipt,
) -> Result<Option<String>, NativeError> {
    let b = io.bound().await?;
    let a = b.cleanup(r, &c.original_runtime_command).await?;
    let saved = r
        .recover_shared_attempt_work(
            a,
            c.clone(),
            work.id.clone(),
            Some(receipt.observation_id.clone()),
        )
        .await
        .map_err(store)?
        .and_then(|v| v.receipt)
        .ok_or_else(invalid)?;
    if digest(&saved)? != digest(receipt)? {
        return Err(invalid());
    }
    let wire_receipt =
        super::e2ee::wire_receipt(io, &b, r, &c.original_runtime_command, work, receipt).await?;
    let receipt = &wire_receipt;
    let started = Instant::now();
    let reply: Timed<TicketReply> = b
        .request(
            Action::WorkRecover {
                id: work.id.clone(),
                input: SharedWorkRecoverInput {
                    runtime_command_digest: digest(c)?,
                    work_digest: digest(work)?,
                    observation_id: Some(receipt.observation_id.clone()),
                },
            },
            false,
        )
        .await?;
    let t = reply.value.ticket;
    if t.work_id != work.id || t.work_digest != digest(work)? {
        return Err(invalid());
    }
    if let Some(old) = t.recorded {
        if digest(&old)? != digest(receipt)? {
            return Err(invalid());
        }
        if !work.encrypted && t.reply_state.as_deref() == Some("sent") {
            super::context_io::observe_delivery(&b, r, &c.original_runtime_command, work).await?;
        }
        return Ok(t.reply_state);
    }
    let until = io::window(
        &t.token,
        t.lease_until_ms,
        reply.observed,
        started,
        &b.scope,
    )?;
    let ack: Timed<SharedWorkAcknowledgement> = b
        .upload(
            Action::WorkResult {
                id: work.id.clone(),
                input: Box::new(SharedWorkResultUpload {
                    work_digest: digest(work)?,
                    token: t.token,
                    receipt: receipt.clone(),
                }),
            },
            until,
        )
        .await?;
    if ack.value.observation_id != receipt.observation_id
        || ack.value.receipt_digest != digest(receipt)?
    {
        return Err(invalid());
    }
    if !work.encrypted && ack.value.reply_state.as_deref() == Some("sent") {
        super::context_io::observe_delivery(&b, r, &c.original_runtime_command, work).await?;
    }
    Ok(ack.value.reply_state)
}
