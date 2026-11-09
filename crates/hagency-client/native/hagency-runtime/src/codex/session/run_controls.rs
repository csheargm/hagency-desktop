//! Host controls on the original native stream. Every write has a correlated
//! native RPC; model events keep flowing while its acknowledgement is pending.
use super::*;

#[derive(Clone, Debug)]
pub enum RunAction {
    Steer { text: String },
    Interrupt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Pending,
    Accepted,
    Rejected,
    Unknown,
    Unavailable,
    InProgress,
    Busy,
}
#[derive(Clone, Debug)]
pub struct RunReceipt {
    pub command_id: String,
    pub outcome: RunOutcome,
}
struct Request {
    command: String,
    scope: TurnScope,
    steer: bool,
    deadline: Instant,
    timed_out: bool,
}
struct Stop {
    command: String,
    targets: BTreeMap<(String, String), Option<bool>>,
    reported: bool,
}
#[derive(Default)]
pub(super) struct RunControls {
    pending: BTreeMap<RequestId, Request>,
    seen: BTreeSet<String>,
    receipts: VecDeque<RunReceipt>,
    stop: Option<Stop>,
    terminal: Option<super::super::ObservationKind>,
}
impl RunControls {
    pub fn retire(&mut self) {
        self.pending.clear();
        self.stop = None;
        self.terminal = None;
    }
    pub fn holds_terminal(&self) -> bool {
        self.terminal.is_some()
    }
    pub fn waiting(&self) -> bool {
        self.pending.values().any(|p| !p.timed_out)
    }
    pub fn holding_answers(&self) -> bool {
        self.stop.is_some()
    }
    pub fn accepted_stop(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|s| s.targets.values().any(|v| *v == Some(true)))
    }
    fn expire(&mut self) {
        for pending in self.pending.values_mut() {
            if pending.steer && !pending.timed_out && Instant::now() >= pending.deadline {
                pending.timed_out = true;
                self.receipts.push_back(RunReceipt {
                    command_id: pending.command.clone(),
                    outcome: RunOutcome::Unknown,
                });
            }
        }
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    pub(super) async fn ready_control_terminal(&mut self) -> Result<Option<Update>, Error> {
        self.run_controls.expire();
        if self.run_controls.waiting() {
            return Ok(None);
        }
        if let Some(terminal) = self.run_controls.terminal.take() {
            self.drain_terminal().await?;
            self.observation_kind = terminal;
            if !self.wire.has_live_shared_activity() {
                self.wire.close();
            }
            return Ok(Some(Update::TurnEnded));
        }
        Ok(None)
    }
    pub fn run_control_receipts(&mut self) -> Vec<RunReceipt> {
        self.run_controls.expire();
        self.run_controls.receipts.drain(..).collect()
    }
    pub fn run_controls_hold_answers(&self) -> bool {
        self.run_controls.holding_answers()
    }
    pub fn run_stop_accepted(&self) -> bool {
        self.run_controls.accepted_stop()
    }
    pub async fn submit_run_control(
        &mut self,
        command: String,
        action: RunAction,
    ) -> Result<RunOutcome, Error> {
        if self.phase() != Phase::Running || !crate::codex::text(&command, 1024) {
            return Ok(RunOutcome::Unavailable);
        }
        if self.run_controls.seen.contains(&command) {
            return Ok(RunOutcome::InProgress);
        }
        let interrupt = matches!(action, RunAction::Interrupt);
        if self.run_controls.pending.len() >= if interrupt { 24 } else { 16 }
            || (!interrupt && self.run_controls.receipts.len() >= 16)
        {
            return Ok(RunOutcome::Busy);
        }
        if self.run_controls.stop.is_some() {
            return Ok(RunOutcome::InProgress);
        }
        let operation = Operation {
            session: self,
            finished: false,
        };
        let result = operation
            .session
            .submit_run_control_inner(command, action)
            .await;
        operation.finish(result)
    }
    async fn submit_run_control_inner(
        &mut self,
        command: String,
        action: RunAction,
    ) -> Result<RunOutcome, Error> {
        let active: Vec<_> = self
            .tree
            .as_ref()
            .ok_or(Error::State)?
            .graph
            .active_scopes()
            .collect();
        match action {
            RunAction::Steer { text } => {
                let Some(root) = active.iter().find(|scope| scope.root) else {
                    return Ok(RunOutcome::Unavailable);
                };
                if text.trim().is_empty() || text.len() > 65_536 {
                    return Err(Error::Settings);
                }
                self.run_controls.seen.insert(command.clone());
                self.write_run_control(
                    command,
                    TurnScope::new(root.thread.clone(), root.turn.clone())
                        .map_err(|_| Error::Scope)?,
                    Some(text),
                )
                .await?;
            }
            RunAction::Interrupt => {
                if active.is_empty() {
                    return Ok(RunOutcome::Unavailable);
                }
                self.run_controls.seen.insert(command.clone());
                self.run_controls.stop = Some(Stop {
                    command,
                    targets: BTreeMap::new(),
                    reported: false,
                });
                self.pump_run_stops().await?;
            }
        }
        // Pending denotes the host write; Accepted is only a native reply.
        Ok(RunOutcome::Pending)
    }
    async fn write_run_control(
        &mut self,
        command: String,
        scope: TurnScope,
        text: Option<String>,
    ) -> Result<(), Error> {
        let steer = text.is_some();
        let input = text.map(|text| (command.clone(), text));
        let deadline = Instant::now() + Duration::from_secs(10);
        let sent = self
            .wire
            .send(transport::Command::RunControl {
                scope: scope.clone(),
                input,
            })
            .await
            .map_err(Error::Transport)?;
        let id = sent.request_id.ok_or(Error::Scope)?;
        self.run_controls.pending.insert(
            id,
            Request {
                command,
                scope,
                steer,
                deadline,
                timed_out: false,
            },
        );
        Ok(())
    }
    pub(super) async fn pump_run_stops(&mut self) -> Result<(), Error> {
        self.run_controls.expire();
        let Some(stop) = &self.run_controls.stop else {
            return Ok(());
        };
        let command = stop.command.clone();
        let missing: Vec<_> = self
            .tree
            .as_ref()
            .ok_or(Error::State)?
            .graph
            .active_scopes()
            .filter(|s| {
                !stop
                    .targets
                    .contains_key(&(s.thread.clone(), s.turn.clone()))
            })
            .collect();
        // Stop intent survives the root ACK, including children announced later.
        for scope in missing {
            if self.run_controls.pending.len() >= 24 {
                break;
            }
            self.run_controls
                .stop
                .as_mut()
                .ok_or(Error::State)?
                .targets
                .insert((scope.thread.clone(), scope.turn.clone()), None);
            let scope = TurnScope::new(scope.thread, scope.turn).map_err(|_| Error::Scope)?;
            self.write_run_control(command.clone(), scope, None).await?;
        }
        Ok(())
    }
    pub(super) async fn controlled_update(&mut self, event: Event) -> Result<Update, Error> {
        self.run_controls.expire();
        if let Event::Response { id, method, result } = event {
            let pending = self.run_controls.pending.remove(&id).ok_or(Error::Scope)?;
            let accepted = match (&result, pending.steer) {
                (Ok(value), true)
                    if method == "turn/steer" && value["turnId"] == pending.scope.turn_id() =>
                {
                    true
                }
                (Ok(value), false)
                    if method == "turn/interrupt"
                        && value.as_object().is_some_and(|o| o.is_empty()) =>
                {
                    true
                }
                (Err(_), true) if method == "turn/steer" => false,
                (Err(_), false) if method == "turn/interrupt" => false,
                _ => return Err(Error::Scope),
            };
            if pending.steer {
                if !pending.timed_out {
                    self.run_controls.receipts.push_back(RunReceipt {
                        command_id: pending.command,
                        outcome: if accepted {
                            RunOutcome::Accepted
                        } else {
                            RunOutcome::Rejected
                        },
                    });
                }
            } else {
                let stop = self.run_controls.stop.as_mut().ok_or(Error::Scope)?;
                if stop.command != pending.command {
                    return Err(Error::Scope);
                }
                *stop
                    .targets
                    .get_mut(&(
                        pending.scope.thread_id().into(),
                        pending.scope.turn_id().into(),
                    ))
                    .ok_or(Error::Scope)? = Some(accepted);
                if stop.targets.values().all(Option::is_some) {
                    let any = stop.targets.values().any(|value| *value == Some(true));
                    let all = stop.targets.values().all(|value| *value == Some(true));
                    if any && !all {
                        return Err(Error::Scope);
                    }
                    if !stop.reported {
                        self.run_controls.receipts.push_back(RunReceipt {
                            command_id: stop.command.clone(),
                            outcome: if all {
                                RunOutcome::Accepted
                            } else {
                                RunOutcome::Rejected
                            },
                        });
                        stop.reported = true;
                    }
                    if !any {
                        self.run_controls.stop = None;
                    }
                }
            }
            if let Some(terminal) = self.ready_control_terminal().await? {
                return Ok(terminal);
            }
            self.observation_kind = super::super::ObservationKind::Ignored;
            return Ok(Update::Notice);
        }
        // Once stop is accepted, never grant a freshly arriving callback.
        if self
            .run_controls
            .stop
            .as_ref()
            .is_some_and(|s| s.targets.values().any(|v| *v == Some(true)))
            && let Event::ServerRequest { id, .. } = event
        {
            self.wire
                .send(transport::Command::DeclineServerRequest { id })
                .await
                .map_err(Error::Transport)?;
            self.observation_kind = super::super::ObservationKind::Ignored;
            return Ok(Update::Notice);
        }
        let update = self.accept_update_inner(event).await?;
        if matches!(update, Update::TurnEnded) && self.run_controls.waiting() {
            self.run_controls.terminal = Some(std::mem::replace(
                &mut self.observation_kind,
                super::super::ObservationKind::Ignored,
            ));
            Ok(Update::Progress)
        } else {
            Ok(update)
        }
    }
}
