//! Shared Claude uses the same native loop as personal Claude; only authority,
//! Matrix delivery and the original durable usage sink differ.
use super::*;
use crate::console::lifecycle::DrainFailure;
use hagency_agent_local::runtime::claude_turn;
use hagency_runtime::owned::{ManagedProcessState, OwnedClaudeSession};
use tokio::sync::{oneshot, watch};
mod authority;
mod bridge;
mod policy;
mod session_route;
pub(super) use authority::Claimed;
use authority::{Active, Live};
#[cfg(feature = "shared-budget-execution")]
use authority::Authority;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    r: OwnerResourceRegistry,
    claimed: Claimed,
    mut launch: launch::ClaudeLaunch,
    io: Io,
    _observed: u64,
    _started: Instant,
    until: Instant,
    mut cancel: watch::Receiver<bool>,
    sender: oneshot::Sender<Result<SharedExecutionProgress, NativeError>>,
) -> Result<(), DrainFailure> {
    let mut sender = Some(sender);
    let session = guarded(&mut cancel, until, || claimed.prepare_session(&io, &launch, until)).await;
    let mut session = match session {
        Ok(session) => session,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    if let Err(error) = launch.bind_session_scope(&session, claimed.root()) {
        let _ = sender.take().unwrap().send(Err(error));
        return Err(DrainFailure::Unknown);
    }
    let mut process = match launch.spawn(until, session.resume_id()) {
        Ok(p) => p,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    // Initialization cannot consume model input or bind work. A missing native
    // file may be rebuilt once, only after the original process is proven stopped.
    // Even failed initialization is bound below so original cleanup is durable.
    let initialized = guarded(&mut cancel, until, || async {
        io.check_running(claimed.root()).await?;
        launch.check()?;
        // Failed resumed starts are counted per saved session, for this host
        // process only.
        let counted = session.resume_id().map(|saved| format!("shared\n{saved}"));
        if let Err(error) = process.initialize().await {
            use hagency_runtime::claude::{resume_failures, session::Error as Native};
            if !matches!(&error, Native::MissingResume) {
                // A timeout, a refusal, an exit or an unexpected frame says
                // nothing about the saved session: do not retire it, return
                // the failure, so that the next request resumes it. An
                // unsupported installation is never counted: a fresh start
                // meets the same thing.
                let Some(counted) = counted.filter(|_| !matches!(&error, Native::UnsupportedVersion)) else {
                    return Err(initialize_error(error));
                };
                let failures = resume_failures::record(&counted);
                tracing::warn!(
                    code = "claude_resume_start_failed",
                    scope = "shared",
                    cause = %error,
                    failures,
                    "A resumed Claude session failed to start before input"
                );
                // Only a saved session that keeps failing this way, and whose
                // failed process is proven stopped before any prompt byte, is
                // replaced, and only once a fresh process has initialized.
                if failures < resume_failures::LIMIT
                    || process.failed_before_prompt_resume_id() != session.resume_id()
                {
                    return Err(initialize_error(error));
                }
                io.check_running(claimed.root()).await?;
                launch.check()?;
                let mut fresh = launch.spawn(until, None)?;
                if let Err(error) = fresh.initialize().await {
                    // Nothing is retired and the count is kept. The fresh
                    // process is the one whose cleanup is bound below.
                    fresh.stop();
                    process = fresh;
                    return Err(initialize_error(error));
                }
                if session.rebuild_unresumable(&process).is_err() {
                    fresh.stop();
                    process = fresh;
                    return Err(failure(409, "shared_execution_initialize_failed"));
                }
                resume_failures::clear(&counted);
                process = fresh;
                tracing::warn!(
                    code = "claude_saved_session_discarded",
                    scope = "shared",
                    reason = "native_resume_failed_repeatedly",
                    "A saved Claude session was discarded; this conversation continues in a fresh one"
                );
                return Ok(());
            }
            session.rebuild_missing(&process)
                .map_err(|_| failure(409, "shared_execution_initialize_failed"))?;
            if let Some(counted) = &counted {
                resume_failures::clear(counted);
            }
            // The only discard of a saved shared session on a single failure.
            // A notice to the room that the earlier conversation is gone
            // would be raised from here, and from the discard above.
            tracing::warn!(
                code = "claude_saved_session_discarded",
                scope = "shared",
                reason = "native_context_missing",
                "A saved Claude session was discarded; this conversation continues in a fresh one"
            );
            io.check_running(claimed.root()).await?;
            launch.check()?;
            process = launch.spawn(until, None)?;
            process.initialize().await
                .map_err(initialize_error)?;
        } else if let Some(counted) = &counted {
            hagency_runtime::claude::resume_failures::clear(counted);
        }
        Ok(())
    }).await;
    let authority = match process
        .shared_process_source()
        .map_err(|_| invalid())
        .and_then(|source| claimed.bind(source))
    {
        Ok(authority) => Arc::new(authority),
        Err(error) => {
            process.stop();
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    let setup = guarded(&mut cancel, until, || async {
        initialized?;
        io.check_running(authority.root()).await?;
        launch.check()?;
        let observation = process
            .qualify_shared(launch.binding()?)
            .map_err(|_| failure(409, "shared_execution_identity_failed"))?;
        let (lease, at, sent) = authority.qualified(&io, until, observation).await?;
        Active::new(
            io.clone(),
            r.clone(),
            authority.clone(),
            &mut process,
            lease,
            (at, sent),
        )
        .await
    })
    .await;
    let result = match setup {
        Ok(mut active) => {
            let _ = sender
                .take()
                .unwrap()
                .send(Ok(SharedExecutionProgress::Running {
                    command_id: authority.id().into(),
                }));
            work(&mut active, &launch, &mut process, session, &mut cancel).await
        }
        Err(error) => Err(error),
    };
    if let Some(sender) = sender.take() {
        let _ = sender.send(
            result
                .as_ref()
                .map(|_| SharedExecutionProgress::Recorded {
                    command_id: authority.id().into(),
                    observation_id: String::new(),
                    process_state: None,
                })
                .map_err(|e| failure(e.status, &e.code)),
        );
    }
    if result.as_ref().err().is_some_and(super::claude_workspace::scope_revoked) {
        launch.retire_plaintext();
    }
    if let Err(error) = result {
        tracing::warn!(code=%error.code,"Shared Claude process ending after operation failure");
    }
    let observation = process
        .stop_shared_observation()
        .map_err(|_| DrainFailure::Unknown)?;
    let stopped = observation.state() == ManagedProcessState::Stopped;
    // Commit physical stop locally before archival; only publish restart
    // readiness after the private archive has committed successfully.
    authority
        .stopped(&io, &r, observation, || {
            if stopped { launch.seal_stopped() } else { Ok(()) }
        })
        .await
        .map_err(|_| DrainFailure::Unknown)?;
    if stopped {
        Ok(())
    } else {
        Err(DrainFailure::Unknown)
    }
}

fn initialize_error(error: hagency_runtime::claude::session::Error) -> NativeError {
    if matches!(error, hagency_runtime::claude::session::Error::UnsupportedVersion) {
        tracing::warn!("Shared Claude requires Claude Code 2.1.322 or newer");
        failure(409, "shared_claude_version_unsupported")
    } else {
        failure(409, "shared_execution_initialize_failed")
    }
}
async fn work(
    active: &mut Active,
    launch: &launch::ClaudeLaunch,
    process: &mut OwnedClaudeSession,
    mut session: SharedClaudeSession,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), NativeError> {
    let live = active.live.clone();
    let turn_policy = policy::load(&launch.turn_policy_path()?)?;
    if live.authority.is_root() {
        let (b, p, _) = live.proof().await?;
        if live
            .registry
            .shared_bootstrap_requires_successor(p)
            .await
            .map_err(store)?
        {
            super::demand_io::require_pair(&b).await?;
            return Ok(());
        }
    }
    let admission = live.io.hosting_admission()?;
    let (work, mut turn) = loop {
        let Some(turn) = admission.enter_turn() else {
            active
                .wait(cancel, async {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok(())
                })
                .await?;
            continue;
        };
        let found = active
            .wait(cancel, async {
                let (b, p, state) = live.proof().await?;
                let reply: Timed<io::WorkReply> = b
                    .upload(
                        live.authority.work_poll(state.lease.authority.clone()),
                        state.until,
                    )
                    .await?;
                let Some(w) = reply.value.work else {
                    return Ok(None);
                };
                w.validate().map_err(store)?;
                if cfg!(feature = "shared-intake") {
                    #[cfg(feature = "shared-budget-execution")]
                    if live.authority.is_budget() {
                        super::demands::bind_budget_work(
                            &b,
                            &live.registry,
                            live.authority.root(),
                            &w,
                            p,
                            state.until,
                        )
                        .await?;
                    } else {
                        super::demands::bind_work(
                            &b,
                            &live.registry,
                            live.authority.root(),
                            &w,
                            p,
                            state.until,
                        )
                        .await?;
                    }
                    #[cfg(not(feature = "shared-budget-execution"))]
                    super::demands::bind_work(
                        &b,
                        &live.registry,
                        live.authority.root(),
                        &w,
                        p,
                        state.until,
                    )
                    .await?;
                    let (_, fresh, _) = live.proof().await?;
                    live.registry
                        .receive_shared_work(fresh, w.clone())
                        .await
                        .map_err(store)?;
                } else {
                    live.registry
                        .receive_shared_work(p, w.clone())
                        .await
                        .map_err(store)?;
                }
                let ack: Timed<io::AckReply> = b
                    .upload(
                        live.authority.work_ack(SharedWorkInput {
                            authority: state.lease.authority,
                            work_id: w.id.clone(),
                            work_digest: digest(&w)?,
                        }),
                        state.until,
                    )
                    .await?;
                if !ack.value.acknowledged {
                    return Err(invalid());
                }
                Ok(Some(w))
            })
            .await?;
        if let Some(work) = found {
            break (work, turn);
        }
        drop(turn);
        active
            .wait(cancel, async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(())
            })
            .await?;
    };
    let plaintext_input = if !work.encrypted {
        active.wait(cancel, async {
            let (b, p, state) = live.proof().await?;
            #[cfg(feature = "shared-budget-execution")]
            if live.authority.is_budget() {
                return super::budget_context_io::prepare(
                    &live.io,
                    &b,
                    &live.registry,
                    p,
                    &work,
                    state.lease.authority,
                    state.until,
                )
                .await
                .map(Some);
            }
            super::context_io::prepare(
                &live.io,
                &b,
                &live.registry,
                p,
                &work,
                state.lease.authority,
                state.until,
            )
            .await
            .map(Some)
        }).await?
    } else { None };
    active.wait(cancel, async {
        let (_, proof, _) = live.proof().await?;
        session.attach(proof, work.clone(), process).await.map_err(store)
    }).await?;
    launch.bind_admitted_scope(&session)?;
    let mut history = active
        .wait(cancel, async {
            let (b, _, _) = live.proof().await?;
            super::history::Reader::new(&live.io, &b, &work, plaintext_input.as_ref()).await
        })
        .await?;
    if let Some(reader) = &mut history {
        reader.use_native_compaction();
    }
    let input = active
        .wait(cancel, async {
            let (b, _, _) = live.proof().await?;
            if work.encrypted {
                let mut text = super::e2ee::input(&live.io, &b, &work).await?;
                if let Some(reader) = &mut history {
                    text = if session.is_resuming() { reader.followup_prompt(text) } else { reader.prompt(text).await };
                }
                let (_, fresh, _) = live.proof().await?;
                let prepared = live.registry
                    .prepare_shared_decrypted_input(fresh, work.clone(), text).await;
                prepared.map_err(store)
            } else {
                plaintext_input.ok_or_else(|| failure(409, "native_context_protocol_required"))
            }
        })
        .await?;
    let files = active
        .wait(cancel, async {
            let (b, _, _) = live.proof().await?;
            super::files::Files::for_workspace(
                &live.io,
                &b,
                &work,
                &launch.profile.cwd,
                launch.policy.tools(),
                Some(&input),
            )
            .await
        })
        .await?;
    if history.is_some() {
        let tools = if files.is_some() {
            hagency_runtime::room_tools::RoomTools::history_and_files()
        } else {
            hagency_runtime::room_tools::RoomTools::history()
        };
        active
            .wait(cancel, async {
                process
                    .bind_room_tools(tools)
                    .await
                    .map_err(|_| failure(409, "shared_history_tool_unavailable"))
            })
            .await?;
    }
    let (_, p, _) = live.proof().await?;
    let call = live.registry
        .prepare_shared_input_call(p, work.clone(), input.clone())
        .await.map_err(store)?;
    let call_id = call.call_id.clone();
    // Build all fallible host channels before central admission. Once custody
    // is issued, every exit must retain an original accounting receipt.
    let scope = hagency_agent_local::Scope {
        agent: live
            .authority
            .root()
            .reserve_command
            .proposed_agent_id
            .clone(),
        binding: work.assignment_id.clone(),
        room: work.room_id.clone(),
        requester: work.requester_mxid.clone(),
        thread: work.thread_root.clone(),
    };
    let mut matrix = bridge::Matrix::new(
        live.clone(),
        work.clone(),
        scope.clone(),
        &call_id,
        history,
        files,
    )?;
    let stream = matrix
        .controls
        .claim(&scope, &work.id, &call_id)
        .map_err(|_| invalid())?;
    let queue = matrix.queue.clone();
    let (b, p, state) = live.proof().await?;
    let start = live.registry
        .begin_shared_input_start(p, work.clone(), call.call_id, input.clone())
        .await.map_err(store)?;
    let issued = match active
        .wait(cancel, async {
            #[cfg(feature = "shared-budget-execution")]
            if live.authority.is_budget() {
                return super::budget_context_io::start(
                    &b,
                    &live.registry,
                    start,
                    &work,
                    &input,
                    state.lease.authority,
                    state.until,
                )
                .await;
            }
            super::context_io::start(
                &b,
                &live.registry,
                start,
                &work,
                &input,
                state.lease.authority,
                state.until,
            )
            .await
        })
        .await
    {
        Ok(issued) => issued,
        Err(error) => {
            #[cfg(feature = "shared-budget-execution")]
            if let Authority::Budget(_, delivery) = live.authority.as_ref()
                && let Ok(Some(receipt)) = super::budget_work_io::refuse_unissued(
                    &live.io,
                    &live.registry,
                    &delivery.command,
                    &work,
                    Some(&input),
                )
                .await
            {
                let _ = live
                    .authority
                    .report(&live.io, &live.registry, &work, &receipt)
                    .await;
            }
            return Err(error);
        }
    };
    let text = issued.model_input().to_owned();
    let (mut host, mut capture) = bridge::capture(live.clone(), launch.policy, issued, session);
    let progress = capture.progress();
    let (started, mut began) = oneshot::channel();
    let stop = matrix.stop_sender();
    let result = {
        let serve = matrix.serve();
        tokio::pin!(serve);
        let native_polled = std::sync::atomic::AtomicBool::new(false);
        let native = async {
            native_polled.store(true, std::sync::atomic::Ordering::Relaxed);
            let native=claude_turn::run(process,&launch.profile,&scope,&work.id,&text,&queue,Some(started),Some(stream),&mut host,&mut capture);
            tokio::pin!(native);
            let mut start_seen=false;
            loop{tokio::select!{biased;
                value=&mut began,if !start_seen=>{start_seen=true;if value.is_ok(){turn.started();}},
                result=&mut native=>return result.map_err(|_|failure(409,"shared_execution_event_unknown")),
            }}
        };
        tokio::pin!(native);
        let watched = async {
            tokio::select! { biased;
                result = &mut native => Ok(result),
                _ = &mut serve => Err(failure(409, "shared_execution_matrix_unavailable")),
                error = turn_policy.expired(progress) => Err(error),
            }
        };
        match active.wait(cancel, watched).await {
            Ok(result) => result,
            // Cancellation can arrive after central admission but before the
            // native future's first poll. Cleanup must not start that model call.
            Err(error) if !native_polled.load(std::sync::atomic::Ordering::Relaxed) => Err(error),
            Err(error) => {
                // Dropping an OwnedClaudeSession operation kills its child. Keep
                // this exact reader/capture alive while the cooperative interrupt
                // collects the provider's final result and output-token evidence.
                let deadline = active.live.state().until.min(Instant::now() + Duration::from_secs(3));
                let mut interrupt_receipt = None;
                finish_after_stop(&mut native, deadline, || {
                    use hagency_agent_local::run_controls::{Action, SubmitError};
                    match stop.submit(Action::Interrupt) {
                        Ok(submission) => { interrupt_receipt = Some(submission); true }
                        Err(SubmitError::Busy) => true,
                        Err(_) => false, // The original start may still be acknowledging.
                    }
                }).await.unwrap_or(Err(error))
            }
        }
    };
    // Always retain original failure/unknown accounting before physical stop.
    let receipt = capture.finish(&mut host).await;
    matrix.finish(&result);
    if let Ok(outcome) = &result
        && let Some(error) = &outcome.late_failure
    {
        let (kind, detail) = error.diagnostic();
        tracing::warn!(
            code = "shared_claude_answer_kept_after_stream_failure",
            kind,
            detail,
            "Shared Claude stream failed after its final answer; the answer is kept and its usage stays incomplete"
        );
    }
    match receipt {
        Ok(Some(receipt)) => {
            if let Err(error) = live
                .authority
                .report(&live.io, &live.registry, &work, &receipt)
                .await
            {
                tracing::debug!(code=%error.code,"Claude work receipt retained for recovery");
            }
        }
        Ok(None) => {}
        Err(error) => return Err(error),
    }
    result.map(|_| ())
}

async fn finish_after_stop<F: std::future::Future>(
    future: F,
    deadline: Instant,
    mut interrupt: impl FnMut() -> bool,
) -> Option<F::Output> {
    tokio::pin!(future);
    let mut requested = false;
    let mut retry = tokio::time::interval(Duration::from_millis(10));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            result = &mut future => return Some(result),
            _ = tokio::time::sleep_until(deadline.into()) => return None,
            _ = retry.tick(), if !requested => { requested = interrupt(); }
        }
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    #[test]
    fn unsupported_cli_version_has_a_named_host_refusal() {
        assert_eq!(initialize_error(hagency_runtime::claude::session::Error::UnsupportedVersion).code,
            "shared_claude_version_unsupported");
        assert_eq!(initialize_error(hagency_runtime::claude::session::Error::PeerEof).code,
            "shared_execution_initialize_failed");
    }
    #[tokio::test]
    async fn stop_keeps_original_future_until_interrupt_result_is_captured() {
        let (send, receive) = oneshot::channel();
        let mut send = Some(send);
        let mut attempts = 0;
        let answer = finish_after_stop(async { receive.await.unwrap() }, Instant::now() + Duration::from_secs(1), || {
            attempts += 1;
            if attempts < 3 { return false; }
            send.take().unwrap().send(("final answer", 27)).unwrap();
            true
        }).await;
        assert_eq!(answer, Some(("final answer", 27)));
        assert_eq!(attempts, 3, "start acknowledgement was retained until control became active");
    }
    #[tokio::test]
    async fn silent_child_stop_capture_is_bounded_without_inventing_usage() {
        let mut requested = 0;
        let result = finish_after_stop(std::future::pending::<()>(), Instant::now() + Duration::from_millis(20), || {
            requested += 1; true
        }).await;
        assert!(result.is_none());
        assert_eq!(requested, 1);
    }
}
