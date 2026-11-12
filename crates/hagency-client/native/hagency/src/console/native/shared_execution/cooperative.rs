//! Keep the original native reader and accounting sink moving during slow
//! Matrix IO. Deferred host callbacks have ALREADY been observed by the sink;
//! their native sequence must never be billed a second time when dispatched.
use super::*;
use hagency_runtime::{
    codex::session::{ControlUpdate, ObservationKind},
    owned::{OwnedSession, SharedTextEvent},
};
use std::{collections::VecDeque, future::Future};

pub(super) enum Waiting<T> {
    Ready(T),
    Stopped,
    Terminal(SharedWorkReceipt),
}
enum Control<T> {
    Ready(T),
    Tick,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn wait<T, F: Future<Output = T>>(
    future: F,
    io: &Io,
    command: &SharedRuntimeCommand,
    process: &mut OwnedSession,
    capture: &mut SharedCallSink,
    usage: &mut crate::console::native::telemetry::TurnTelemetry,
    controls: &mut Option<super::run_controls::Controls>,
    deferred: &mut VecDeque<SharedTextEvent>,
    admissible: impl Fn(&OwnedSession) -> bool,
) -> Result<Waiting<T>, NativeError> {
    tokio::pin!(future);
    loop {
        if let Some(controls) = controls.as_mut() {
            controls.poll(io, command, process).await?;
        }
        if process.run_stop_accepted() || !admissible(process) {
            return Ok(Waiting::Stopped);
        }
        let hold = process.run_controls_hold_answers();
        let next = async {
            tokio::select! { biased;
                _ = tokio::time::sleep(Duration::from_millis(250)) => Control::Tick,
                result = future.as_mut(), if !hold => Control::Ready(result),
            }
        };
        tokio::pin!(next);
        let update = process
            .next_observed_or_control(next.as_mut())
            .await
            .map_err(|_| failure(409, "shared_execution_event_unknown"))?;
        match update {
            ControlUpdate::Control(Control::Ready(value)) => return Ok(Waiting::Ready(value)),
            ControlUpdate::Control(Control::Tick) => {}
            ControlUpdate::Update(update, observation) => {
                let event = process
                    .shared_event(update, *observation)
                    .map_err(|_| failure(409, "shared_execution_event_unknown"))?;
                let ended = matches!(event.observation().kind(), ObservationKind::TurnEnded(_));
                let callback = event.room_file_call().is_some()
                    || event.room_history_call().is_some()
                    || event.approval().is_some()
                    || event.user_input().is_some()
                    || event.user_input_resolved().is_some()
                    || event.observation().context_usage().is_some();
                let pending = usage.pending(&event);
                let later = callback.then(|| event.clone());
                let receipt = match capture.observe(event).await {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        usage.abandon();
                        let _ = capture.record_unknown().await;
                        return Err(store(error));
                    }
                };
                usage.accept(pending);
                usage.quota(process);
                if ended {
                    return Ok(Waiting::Terminal(receipt.ok_or_else(invalid)?));
                }
                if let Some(event) = later {
                    // Only outstanding callbacks/context notices are retained,
                    // never the potentially unbounded model text event stream.
                    if deferred.len() >= 256 {
                        return Err(failure(409, "shared_callback_capacity"));
                    }
                    deferred.push_back(event);
                }
            }
        }
    }
}
