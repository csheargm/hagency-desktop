//! Shared Claude uses the same native loop as personal Claude; only authority,
//! Matrix delivery and the original durable usage sink differ.
use super::*;
use crate::console::lifecycle::DrainFailure;
use hagency_agent_local::runtime::claude_turn;
use hagency_runtime::owned::{ManagedProcessState, OwnedClaudeSession};
use tokio::sync::{oneshot, watch};
mod authority;
mod bridge;
mod session_route;
pub(super) use authority::Claimed;
use authority::{Active, Authority, Live};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    r: OwnerResourceRegistry,
    claimed: Claimed,
    launch: launch::ClaudeLaunch,
    io: Io,
    _observed: u64,
    _started: Instant,
    until: Instant,
    mut cancel: watch::Receiver<bool>,
    sender: oneshot::Sender<Result<SharedExecutionProgress, NativeError>>,
) -> Result<(), DrainFailure> {
    let mut sender = Some(sender);
    let session = guarded(&mut cancel, until, || claimed.prepare_session(&io, &launch, until)).await;
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
    let mut process = match launch.spawn(until, session.resume_id()) {
        Ok(p) => p,
        Err(error) => {
            let _ = sender.take().unwrap().send(Err(error));
            return Err(DrainFailure::Unknown);
        }
    };
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
        io.check_running(authority.root()).await?;
        launch.check()?;
        process
            .initialize()
            .await
            .map_err(|_| failure(409, "shared_execution_initialize_failed"))?;
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
    if let Err(error) = result {
        tracing::warn!(code=%error.code,"Shared Claude process ending after operation failure");
    }
    let observation = process
        .stop_shared_observation()
        .map_err(|_| DrainFailure::Unknown)?;
    let stopped = observation.state() == ManagedProcessState::Stopped;
    authority
        .stopped(&io, &r, observation)
        .await
        .map_err(|_| DrainFailure::Unknown)?;
    if stopped {
        Ok(())
    } else {
        Err(DrainFailure::Unknown)
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
                if w.encrypted && !cfg!(feature = "shared-context") {
                    return Err(failure(409, "shared_encrypted_context_required"));
                }
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
    active.wait(cancel, async {
        let (_, proof, _) = live.proof().await?;
        session.attach(proof, work.clone(), process).await.map_err(store)
    }).await?;
    let mut history = active
        .wait(cancel, async {
            let (b, _, _) = live.proof().await?;
            super::history::Reader::new(&live.io, &b, &work).await
        })
        .await?;
    if let Some(reader) = &mut history {
        reader.use_native_compaction();
    }
    let input = active
        .wait(cancel, async {
            let (b, p, state) = live.proof().await?;
            if cfg!(feature = "shared-context") && work.encrypted {
                let mut text = super::e2ee::input(&live.io, &b, &work).await?;
                if let Some(reader) = &mut history {
                    text = if session.is_resuming() { reader.followup_prompt(text) } else { reader.prompt(text).await };
                }
                let (_, fresh, _) = live.proof().await?;
                let prepared = if history.is_some() {
                    live.registry
                        .prepare_shared_room_input(fresh, work.clone(), text)
                        .await
                } else {
                    live.registry
                        .prepare_shared_decrypted_input(fresh, work.clone(), text)
                        .await
                };
                Ok(Some(prepared.map_err(store)?))
            } else if cfg!(feature = "shared-context") {
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
            } else {
                Ok(None)
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
    let call = if let Some(input) = &input {
        live.registry
            .prepare_shared_input_call(p, work.clone(), input.clone())
            .await
    } else {
        live.registry.prepare_shared_call(p, work.clone()).await
    }
    .map_err(store)?;
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
    let start = if let Some(input) = &input {
        live.registry
            .begin_shared_input_start(p, work.clone(), call.call_id, input.clone())
            .await
    } else {
        live.registry
            .begin_shared_start(p, work.clone(), call.call_id)
            .await
    }
    .map_err(store)?;
    let issued = match active
        .wait(cancel, async {
            #[cfg(feature = "shared-budget-execution")]
            if live.authority.is_budget() {
                return super::budget_context_io::start(
                    &b,
                    &live.registry,
                    start,
                    &work,
                    input.as_ref(),
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
                input.as_ref(),
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
                    input.as_ref(),
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
    let (started, mut began) = oneshot::channel();
    let result=active.wait(cancel,async{
        let native=claude_turn::run(process,&launch.profile,&scope,&work.id,&text,&queue,Some(started),Some(stream),&mut host,&mut capture);
        tokio::pin!(native);
        let serve=matrix.serve();tokio::pin!(serve);
        let mut start_seen=false;
        loop{tokio::select!{biased;
            value=&mut began,if !start_seen=>{start_seen=true;if value.is_ok(){turn.started();}},
            result=&mut native=>return result.map_err(|_|failure(409,"shared_execution_event_unknown")),
            _=&mut serve=>return Err(failure(409,"shared_execution_matrix_unavailable")),
        }}
    }).await;
    // Always retain original failure/unknown accounting before physical stop.
    let receipt = capture.finish(&mut host).await;
    matrix.finish(&result);
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
