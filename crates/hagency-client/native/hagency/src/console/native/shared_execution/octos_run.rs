//! One isolated Octos process, one original admitted call, and its Matrix bridge.
use super::*;
use super::claude_run::{authority::{Active, Authority, Claimed}, bridge::Matrix};
use crate::console::lifecycle::DrainFailure;
use hagency_agent_local::runtime::octos_turn;
use hagency_runtime::owned::{ManagedProcessState, OwnedOctosSession};
use tokio::sync::{oneshot, watch};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    registry: OwnerResourceRegistry,
    claimed: Claimed,
    mut launch: launch::OctosLaunch,
    io: Io,
    until: Instant,
    mut cancel: watch::Receiver<bool>,
    sender: oneshot::Sender<Result<SharedExecutionProgress, NativeError>>,
) -> Result<(), DrainFailure> {
    let mut sender = Some(sender);
    let session = guarded(&mut cancel, until, || prepare_session(&claimed, &io, &launch, until)).await;
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    if let Err(error) = launch.bind_scope(claimed.root(), session.storage_scope()) {
        let _ = sender.take().unwrap().send(Err(error));
        return Err(DrainFailure::Unknown);
    }
    let mut process = match launch.spawn(until) {
        Ok(process) => process,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    let initialized = guarded(&mut cancel, until, || async {
        io.check_running(claimed.root()).await?;
        launch.check()?;
        process.hello().await.map_err(|_| failure(409, "shared_execution_initialize_failed"))
    }).await;
    let authority = match process.shared_process_source().map_err(|_| invalid()).and_then(|source| claimed.bind(source)) {
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
        let observation = launch.qualify(&mut process)?;
        let (lease, at, sent) = authority.qualified(&io, until, observation).await?;
        Active::new_octos(io.clone(), registry.clone(), authority.clone(), &mut process, lease, (at, sent)).await
    }).await;
    let result = match setup {
        Ok(mut active) => {
            let _ = sender.take().unwrap().send(Ok(SharedExecutionProgress::Running { command_id: authority.id().into() }));
            work(&mut active, &launch, &mut process, session, &mut cancel).await
        }
        Err(error) => Err(error),
    };
    if let Some(sender) = sender.take() {
        let _ = sender.send(result.as_ref().map(|_| SharedExecutionProgress::Recorded {
            command_id: authority.id().into(), observation_id: String::new(), process_state: None,
        }).map_err(|e| failure(e.status, &e.code)));
    }
    if let Err(error) = &result {
        tracing::warn!(code=%error.code, "Shared Octos process ending after operation failure");
        if super::claude_workspace::scope_revoked(error) { launch.retire_plaintext(); }
    }
    let observation = process.stop_shared_observation().map_err(|_| DrainFailure::Unknown)?;
    let stopped = observation.state() == ManagedProcessState::Stopped;
    authority.stopped(&io, &registry, observation, || {
        if stopped { launch.seal_stopped() } else { Ok(()) }
    }).await.map_err(|_| DrainFailure::Unknown)?;
    if stopped { Ok(()) } else { Err(DrainFailure::Unknown) }
}

async fn prepare_session(claimed: &Claimed, io: &Io, launch: &launch::OctosLaunch, until: Instant) -> Result<SharedOctosSession, NativeError> {
    let route = match claimed {
        Claimed::Root(_) => None,
        Claimed::Attempt(c) => {
            let d = &c.1;
            super::claude_run::session_route::route(io, &d.command.original_runtime_command, &d.command.command_id,
                &d.command_digest, &d.token, until).await
        }
        #[cfg(feature = "shared-budget-execution")]
        Claimed::Budget(c) => {
            let d = &c.1;
            super::claude_run::session_route::route(io, &d.command.original_runtime_command, &d.command.command_id,
                &d.command_digest, &d.token, until).await
        }
    };
    let settings = launch.session_settings_digest()?;
    match claimed {
        Claimed::Root(c) => c.0.prepare_octos_session(route, settings),
        Claimed::Attempt(c) => c.0.prepare_octos_session(route, settings),
        #[cfg(feature = "shared-budget-execution")]
        Claimed::Budget(c) => c.0.prepare_octos_session(route, settings),
    }.map_err(store)
}

fn turn_id() -> Result<String, NativeError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure(503, "native_random_unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &text[..8], &text[8..12], &text[12..16], &text[16..20], &text[20..]))
}

async fn work(
    active: &mut Active,
    launch: &launch::OctosLaunch,
    process: &mut OwnedOctosSession,
    mut session: SharedOctosSession,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), NativeError> {
    let live = active.live.clone();
    if live.authority.is_root() {
        let (b, proof, _) = live.proof().await?;
        if live.registry.shared_bootstrap_requires_successor(proof).await.map_err(store)? {
            super::demand_io::require_pair(&b).await?;
            return Ok(());
        }
    }
    let admission = live.io.hosting_admission()?;
    let (work, mut turn) = loop {
        let Some(turn) = admission.enter_turn() else {
            active.wait(cancel, async { tokio::time::sleep(Duration::from_millis(250)).await; Ok(()) }).await?;
            continue;
        };
        let found = active.wait(cancel, async {
            let (b, proof, state) = live.proof().await?;
            let reply: Timed<io::WorkReply> = b.upload(live.authority.work_poll(state.lease.authority.clone()), state.until).await?;
            let Some(w) = reply.value.work else { return Ok(None); };
            w.validate().map_err(store)?;
            if cfg!(feature = "shared-intake") {
                #[cfg(feature = "shared-budget-execution")]
                if live.authority.is_budget() {
                    super::demands::bind_budget_work(&b, &live.registry, live.authority.root(), &w, proof, state.until).await?;
                } else {
                    super::demands::bind_work(&b, &live.registry, live.authority.root(), &w, proof, state.until).await?;
                }
                #[cfg(not(feature = "shared-budget-execution"))]
                super::demands::bind_work(&b, &live.registry, live.authority.root(), &w, proof, state.until).await?;
                let (_, fresh, _) = live.proof().await?;
                live.registry.receive_shared_work(fresh, w.clone()).await.map_err(store)?;
            } else {
                live.registry.receive_shared_work(proof, w.clone()).await.map_err(store)?;
            }
            let ack: Timed<io::AckReply> = b.upload(live.authority.work_ack(SharedWorkInput {
                authority: state.lease.authority, work_id: w.id.clone(), work_digest: digest(&w)?,
            }), state.until).await?;
            if !ack.value.acknowledged { return Err(invalid()); }
            Ok(Some(w))
        }).await?;
        if let Some(work) = found { break (work, turn); }
        drop(turn);
        active.wait(cancel, async { tokio::time::sleep(Duration::from_millis(500)).await; Ok(()) }).await?;
    };
    let plaintext_input = if !work.encrypted {
        active.wait(cancel, async {
            let (b, proof, state) = live.proof().await?;
            #[cfg(feature = "shared-budget-execution")]
            if live.authority.is_budget() {
                return super::budget_context_io::prepare(&live.io, &b, &live.registry, proof, &work, state.lease.authority, state.until).await.map(Some);
            }
            super::context_io::prepare(&live.io, &b, &live.registry, proof, &work, state.lease.authority, state.until).await.map(Some)
        }).await?
    } else { None };
    active.wait(cancel, async {
        let (_, proof, _) = live.proof().await?;
        session.attach(proof, work.clone(), process).await.map_err(store)
    }).await?;
    launch.bind_admitted_scope(&session)?;
    let mut history = active.wait(cancel, async {
        let (b, _, _) = live.proof().await?;
        super::history::Reader::new(&live.io, &b, &work, plaintext_input.as_ref()).await
    }).await?;
    let input = active.wait(cancel, async {
        let (b, _, _) = live.proof().await?;
        if work.encrypted {
            let mut text = super::e2ee::input(&live.io, &b, &work).await?;
            if let Some(reader) = &mut history { text = reader.prompt(text).await; }
            let (_, fresh, _) = live.proof().await?;
            live.registry.prepare_shared_decrypted_input(fresh, work.clone(), text).await.map_err(store)
        } else {
            plaintext_input.ok_or_else(|| failure(409, "native_context_protocol_required"))
        }
    }).await?;
    let workspace = launch.workspace()?;
    let files = active.wait(cancel, async {
        let (b, _, _) = live.proof().await?;
        super::files::Files::for_workspace(&live.io, &b, &work, &workspace, launch.policy.tools(), Some(&input)).await
    }).await?;
    let room_tools = history.as_ref().map(|_| if files.is_some() {
        hagency_runtime::room_tools::RoomTools::history_and_files()
    } else { hagency_runtime::room_tools::RoomTools::history() });
    let (_, proof, _) = live.proof().await?;
    let call = live.registry.prepare_shared_input_call(proof, work.clone(), input.clone()).await.map_err(store)?;
    let call_id = call.call_id.clone();
    let scope = hagency_agent_local::Scope {
        agent: live.authority.root().reserve_command.proposed_agent_id.clone(), binding: work.assignment_id.clone(),
        room: work.room_id.clone(), requester: work.requester_mxid.clone(), thread: work.thread_root.clone(),
    };
    let (message_bound, _, _) = live.proof().await?;
    let message_root = super::e2ee::message_root(&live.io, &message_bound, &work.assignment_id)?;
    let mut matrix = Matrix::new(live.clone(), work.clone(), scope.clone(), &call_id, &message_root, history, files)?;
    let stream = matrix.controls.claim(&scope, &work.id, &call_id).map_err(|_| invalid())?;
    let queue = matrix.queue.clone();
    let workspace = workspace.to_str().ok_or_else(invalid)?;
    let session = format!("{}:local:{}", launch.profile_id(), live.authority.id());
    let id = turn_id()?;
    let (b, proof, state) = live.proof().await?;
    let start = live.registry.begin_shared_input_start(proof, work.clone(), call.call_id, input.clone()).await.map_err(store)?;
    let issued = match active.wait(cancel, async {
        #[cfg(feature = "shared-budget-execution")]
        if live.authority.is_budget() {
            return super::budget_context_io::start(&b, &live.registry, start, &work, &input, state.lease.authority, state.until).await;
        }
        super::context_io::start(&b, &live.registry, start, &work, &input, state.lease.authority, state.until).await
    }).await {
        Ok(issued) => issued,
        Err(error) => {
            #[cfg(feature = "shared-budget-execution")]
            if let Authority::Budget(_, delivery) = live.authority.as_ref()
                && let Ok(Some(receipt)) = super::budget_work_io::refuse_unissued(&live.io, &live.registry, &delivery.command, &work, Some(&input)).await
            {
                let _ = live.authority.report(&live.io, &live.registry, &work, &receipt).await;
            }
            return Err(error);
        }
    };
    let text = issued.model_input().to_owned();
    let (mut host, mut capture) = super::octos_capture::capture(live.clone(), launch.policy, issued);
    let (started, mut began) = oneshot::channel();
    let stop = matrix.stop_sender();
    let result = {
        let serve = matrix.serve();
        tokio::pin!(serve);
        let native_polled = std::sync::atomic::AtomicBool::new(false);
        let native = async {
            native_polled.store(true, std::sync::atomic::Ordering::Relaxed);
            let native = octos_turn::run(process, launch.profile_id(), &session, workspace, &id,
                &scope, &work.id, &text, &queue, Some(started), Some(stream), room_tools, &mut host, &mut capture);
            tokio::pin!(native);
            let mut start_seen = false;
            loop { tokio::select! { biased;
                value=&mut began, if !start_seen => { start_seen=true; if value.is_ok() { turn.started(); } },
                result=&mut native => return result.map_err(|_| failure(409, "shared_execution_event_unknown")),
            }}
        };
        tokio::pin!(native);
        let watched = async { tokio::select! { biased;
            result=&mut native => Ok(result),
            _=&mut serve => Err(failure(409, "shared_execution_matrix_unavailable")),
            _=tokio::time::sleep(Duration::from_secs(3600)) => Err(failure(409, "shared_octos_duration_reached")),
        }};
        match active.wait(cancel, watched).await {
            Ok(result) => result,
            Err(error) if !native_polled.load(std::sync::atomic::Ordering::Relaxed) => Err(error),
            Err(error) => {
                let deadline = active.live.state().until.min(Instant::now() + Duration::from_secs(3));
                let mut interrupt_receipt = None;
                super::claude_run::finish_after_stop(&mut native, deadline, || {
                    use hagency_agent_local::run_controls::{Action, SubmitError};
                    match stop.submit(Action::Interrupt) {
                        Ok(submission) => { interrupt_receipt = Some(submission); true }
                        Err(SubmitError::Busy) => true,
                        Err(_) => false,
                    }
                }).await.unwrap_or(Err(error))
            }
        }
    };
    let receipt = capture.finish(&mut host).await;
    if !matches!(tokio::time::timeout(Duration::from_secs(30),
        active.wait(cancel, matrix.flush_messages())).await, Ok(Ok(())))
    {
        tracing::warn!(code = "shared_message_delivery_pending", "Native messages remain in encrypted local custody");
    }
    matrix.finish(&result);
    match receipt {
        Ok(Some(receipt)) => {
            if let Err(error) = live.authority.report(&live.io, &live.registry, &work, &receipt).await {
                tracing::debug!(code=%error.code, "Octos work receipt retained for recovery");
            }
        }
        Ok(None) => {}
        Err(error) => return Err(error),
    }
    result.map(|_| ())
}
