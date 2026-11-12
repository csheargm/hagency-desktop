//! Host-to-runtime controls for one admitted execution. This is NOT a Matrix
//! authentication API: the host verifies current sender/controller authority
//! and durably deduplicates the source event before submitting an action.
use crate::{
    Scope,
    codex::{Error, Result},
};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

pub enum Action {
    Steer { text: String },
    Interrupt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Native acknowledgement, not terminal completion or usage evidence.
    Accepted,
    Rejected,
    Unsupported,
    /// An interrupt for this execution is already being processed.
    InProgress,
    /// The runtime's bounded control queue is occupied; no write began.
    Busy,
    /// No native write began; the original execution is no longer available.
    Unavailable,
    /// Some bytes may have left. Never retry this action automatically.
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    Inactive,
    Busy,
    Invalid,
}
pub struct Submission {
    pub command_id: String,
    pub receipt: oneshot::Receiver<Outcome>,
}
struct Binding {
    scope: Scope,
    dispatch: String,
    execution: String,
    prefix: String,
    state: Mutex<State>,
}
enum Phase {
    New,
    Claimed,
    Running,
    Retired,
}
struct State {
    phase: Phase,
    sequence: u64,
    receivers: Option<(mpsc::Receiver<Pending>, mpsc::Receiver<Pending>)>,
}
#[derive(Clone)]
pub struct Sender {
    binding: Arc<Binding>,
    steer: mpsc::Sender<Pending>,
    interrupt: mpsc::Sender<Pending>,
}
#[derive(Clone)]
pub struct Inbox(Arc<Binding>);
pub struct Stream {
    binding: Arc<Binding>,
    steer: mpsc::Receiver<Pending>,
    interrupt: mpsc::Receiver<Pending>,
    steer_open: bool,
    interrupt_open: bool,
}
pub(crate) struct Pending {
    pub action: Action,
    pub command_id: String,
    response: Option<oneshot::Sender<Outcome>>,
    started: bool,
}
impl Pending {
    pub fn is_closed(&self) -> bool {
        self.response
            .as_ref()
            .is_none_or(oneshot::Sender::is_closed)
    }
    pub fn begin_write(&mut self) {
        self.started = true;
    }
    pub fn reply(mut self, outcome: Outcome) {
        if let Some(response) = self.response.take() {
            let _ = response.send(outcome);
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(response) = self.response.take() {
            let _ = response.send(if self.started {
                Outcome::Unknown
            } else {
                Outcome::Unavailable
            });
        }
    }
}
pub fn channel(scope: &Scope, dispatch: &str, execution: &str) -> Result<(Sender, Inbox)> {
    scope.validate()?;
    crate::key(dispatch)?;
    crate::key(execution)?;
    use sha2::{Digest, Sha256};
    let prefix = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(scope, dispatch, execution))?)
    );
    let (steer, steer_rx) = mpsc::channel(16);
    let (interrupt, interrupt_rx) = mpsc::channel(1);
    let binding = Arc::new(Binding {
        scope: scope.clone(),
        dispatch: dispatch.into(),
        execution: execution.into(),
        prefix,
        state: Mutex::new(State {
            phase: Phase::New,
            sequence: 0,
            receivers: Some((steer_rx, interrupt_rx)),
        }),
    });
    Ok((
        Sender {
            binding: binding.clone(),
            steer,
            interrupt,
        },
        Inbox(binding),
    ))
}
impl Sender {
    pub fn is_active(&self) -> bool {
        matches!(
            self.binding
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase,
            Phase::Running
        )
    }
    /// Each successful submission denotes a NEW action. Persist/deduplicate
    /// the originating Matrix event before calling this method, not after it.
    pub fn submit(&self, action: Action) -> std::result::Result<Submission, SubmitError> {
        if matches!(&action, Action::Steer { text } if text.is_empty() || text.len() > 65_536) {
            return Err(SubmitError::Invalid);
        }
        let mut state = self.binding.state.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(state.phase, Phase::Running) {
            return Err(SubmitError::Inactive);
        }
        let sequence = state.sequence.checked_add(1).ok_or(SubmitError::Busy)?;
        let command_id = format!("{}-{sequence}", self.binding.prefix);
        let sender = if matches!(action, Action::Interrupt) {
            &self.interrupt
        } else {
            &self.steer
        };
        let (response, receipt) = oneshot::channel();
        sender
            .try_send(Pending {
                action,
                command_id: command_id.clone(),
                response: Some(response),
                started: false,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SubmitError::Busy,
                mpsc::error::TrySendError::Closed(_) => SubmitError::Inactive,
            })?;
        state.sequence = sequence;
        Ok(Submission {
            command_id,
            receipt,
        })
    }
}
impl Inbox {
    pub(crate) fn validate(&self, scope: &Scope, dispatch: &str, execution: &str) -> Result<()> {
        if &self.0.scope != scope || self.0.dispatch != dispatch || self.0.execution != execution {
            return Err(Error::Protocol("run control execution differs"));
        }
        Ok(())
    }
    /// Claim before admission so a reused channel cannot start another model
    /// turn. Submission remains inactive until native start is acknowledged.
    pub fn claim(&self, scope: &Scope, dispatch: &str, execution: &str) -> Result<Stream> {
        self.validate(scope, dispatch, execution)?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(state.phase, Phase::New) {
            return Err(Error::Protocol("run controls already used"));
        }
        let (steer, interrupt) = state
            .receivers
            .take()
            .ok_or(Error::Protocol("run controls missing"))?;
        state.phase = Phase::Claimed;
        Ok(Stream {
            binding: self.0.clone(),
            steer,
            interrupt,
            steer_open: true,
            interrupt_open: true,
        })
    }
}
impl Stream {
    pub fn activate(&mut self) {
        self.binding
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = Phase::Running;
    }
    pub(crate) fn try_next(&mut self) -> Option<Pending> {
        loop {
            let pending = self
                .interrupt
                .try_recv()
                .ok()
                .or_else(|| self.steer.try_recv().ok())?;
            if !pending.is_closed() {
                return Some(pending);
            }
        }
    }
    pub(crate) async fn next(&mut self) -> Pending {
        loop {
            let pending = tokio::select! { biased;
                value = self.interrupt.recv(), if self.interrupt_open => {
                    if value.is_none() { self.interrupt_open = false; } value
                }
                value = self.steer.recv(), if self.steer_open => {
                    if value.is_none() { self.steer_open = false; } value
                }
                else => std::future::pending().await,
            };
            if let Some(pending) = pending
                && !pending.is_closed()
            {
                return pending;
            }
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.binding
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = Phase::Retired;
        self.steer.close();
        self.interrupt.close();
        // Receivers drop their unsent actions; in-flight actions retain their
        // own Unknown result until the native adapter confirms an outcome.
    }
}
pub(crate) async fn next(stream: &mut Option<Stream>) -> Pending {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:test".into(),
            requester: "@owner:test".into(),
            thread: "root".into(),
        }
    }
    #[tokio::test]
    async fn control_channel_prioritizes_stop_and_retires_the_original_execution() {
        let scope = scope();
        let (sender, inbox) = channel(&scope, "dispatch", "call").unwrap();
        assert!(matches!(
            sender.submit(Action::Interrupt),
            Err(SubmitError::Inactive)
        ));
        assert!(inbox.claim(&scope, "dispatch", "later").is_err());
        let mut stream = inbox.claim(&scope, "dispatch", "call").unwrap();
        assert!(matches!(
            sender.submit(Action::Interrupt),
            Err(SubmitError::Inactive)
        ));
        stream.activate();
        let mut waiting = Vec::new();
        for _ in 0..16 {
            waiting.push(
                sender
                    .submit(Action::Steer {
                        text: "more".into(),
                    })
                    .unwrap(),
            );
        }
        assert!(matches!(
            sender.submit(Action::Steer {
                text: "full".into()
            }),
            Err(SubmitError::Busy)
        ));
        let stop = sender.submit(Action::Interrupt).unwrap();
        let pending = stream.next().await;
        assert!(matches!(pending.action, Action::Interrupt));
        assert_eq!(pending.command_id, stop.command_id);
        pending.reply(Outcome::Accepted);
        assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
        drop(stream);
        for action in waiting {
            assert_eq!(action.receipt.await.unwrap(), Outcome::Unavailable);
        }
        assert!(inbox.claim(&scope, "dispatch", "call").is_err());
        assert!(matches!(
            sender.submit(Action::Interrupt),
            Err(SubmitError::Inactive)
        ));
    }
    #[tokio::test]
    async fn control_channel_never_reports_a_lost_write_as_rejected_or_replays_it() {
        let scope = scope();
        let (sender, inbox) = channel(&scope, "dispatch", "call").unwrap();
        let mut stream = inbox.claim(&scope, "dispatch", "call").unwrap();
        stream.activate();
        let submitted = sender.submit(Action::Interrupt).unwrap();
        let mut pending = stream.next().await;
        pending.begin_write();
        drop(pending);
        assert_eq!(submitted.receipt.await.unwrap(), Outcome::Unknown);
        let withdrawn = sender
            .submit(Action::Steer {
                text: "withdrawn".into(),
            })
            .unwrap();
        drop(withdrawn);
        let next = sender
            .submit(Action::Steer {
                text: "live".into(),
            })
            .unwrap();
        let pending = stream.next().await;
        assert_eq!(pending.command_id, next.command_id);
        drop(pending);
        assert_eq!(next.receipt.await.unwrap(), Outcome::Unavailable);
    }
}
