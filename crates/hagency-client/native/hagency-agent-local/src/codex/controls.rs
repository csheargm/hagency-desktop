//! Cooperative app-server controls. Native response IDs are allocated by this
//! connection; they never come from the Matrix event or the model transcript.
use super::{Error, Result};
use crate::run_controls::{Action, Outcome, Pending};
use hagency_runtime::{
    codex::tree::{Scope, TurnTree},
    deadline::Deadline,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;

type Key = (String, String);
enum Kind {
    Steer { pending: Pending, turn: String },
    Interrupt(Key),
}
struct Request {
    kind: Kind,
    until: Instant,
}
struct Stop {
    original: Option<Pending>,
    until: Instant,
    targets: BTreeSet<Key>,
    accepted: usize,
    // Each refusal keeps its turn, and whether Codex said that turn was over.
    refused: Vec<(Key, bool)>,
    unsupported: usize,
    acknowledged: bool,
}
#[derive(Default)]
pub(super) struct Controls {
    ready: Option<Pending>,
    requests: BTreeMap<u64, Request>,
    // A timed-out steer may still receive its original reply. Consume it on
    // this transport, but never replay it or attach it to a later execution.
    late: BTreeMap<u64, String>,
    stop: Option<Stop>,
}
impl Controls {
    pub fn can_receive(&self) -> bool {
        self.ready.is_none()
    }
    pub fn offer(&mut self, pending: Pending) {
        self.ready = Some(pending);
    }
    pub fn holds_answers(&self) -> bool {
        self.stop.is_some()
            || self
                .ready
                .as_ref()
                .is_some_and(|p| matches!(p.action, Action::Interrupt))
    }
    pub fn accepted_stop(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.acknowledged)
    }
    pub fn settled(&self) -> bool {
        self.requests.is_empty() && self.stop.as_ref().is_none_or(|s| s.original.is_none())
    }
    pub fn can_retain(&self) -> bool {
        self.late.is_empty() && self.requests.is_empty()
    }
    fn unseen(&self, tree: &TurnTree) -> Option<Scope> {
        let stop = self.stop.as_ref()?;
        let unseen = |scope: &Scope| {
            !stop
                .targets
                .contains(&(scope.thread.clone(), scope.turn.clone()))
        };
        tree.active_scopes()
            .find(|scope| scope.root && unseen(scope))
            .or_else(|| tree.active_scopes().find(unseen))
    }
    pub fn has_outgoing(&self, tree: &TurnTree) -> bool {
        self.ready.is_some() || self.unseen(tree).is_some()
    }
    pub fn deadline(&self) -> Deadline {
        self.requests
            .values()
            .map(|r| r.until)
            .chain(
                self.stop
                    .iter()
                    .filter(|s| s.original.is_some())
                    .map(|s| s.until),
            )
            .min()
            .map(Deadline::from)
            .unwrap_or(Deadline::UNBOUNDED)
    }
    /// The host asked for a stop and Codex has not answered it in time. The
    /// turn then ends without a retained turn, so its caller discards this
    /// process; for a turn that is still running, that is the stop.
    pub fn stop_overdue(&self) -> bool {
        let now = Instant::now();
        self.stop
            .as_ref()
            .is_some_and(|s| s.original.is_some() && s.until <= now)
            || self
                .requests
                .values()
                .any(|r| matches!(r.kind, Kind::Interrupt(_)) && r.until <= now)
    }
    pub fn expire(&mut self) {
        let now = Instant::now();
        // Only a steer expires here. An overdue interrupt stays registered:
        // `stop_overdue` ends the turn and nothing reuses this process.
        let expired: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, r)| matches!(r.kind, Kind::Steer { .. }) && r.until <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(Request {
                kind: Kind::Steer { pending, turn },
                ..
            }) = self.requests.remove(&id)
            {
                pending.reply(Outcome::Unknown);
                self.late.insert(id, turn);
            }
        }
    }
    /// Register the exact request before writing. Dropping the run after a
    /// partial write returns Unknown through the original pending action.
    pub fn prepare(
        &mut self,
        tree: &TurnTree,
        thread: &str,
        turn: &str,
        next_id: &mut u64,
    ) -> Result<Option<Value>> {
        if let Some(mut pending) = self.ready.take()
            && !pending.is_closed()
        {
            match &pending.action {
                Action::Interrupt if self.stop.is_some() => pending.reply(Outcome::InProgress),
                Action::Interrupt if tree.is_complete() => pending.reply(Outcome::Unavailable),
                Action::Interrupt => {
                    self.stop = Some(Stop {
                        original: Some(pending),
                        until: Instant::now() + Duration::from_secs(10),
                        targets: BTreeSet::new(),
                        accepted: 0,
                        refused: Vec::new(),
                        unsupported: 0,
                        acknowledged: false,
                    });
                }
                Action::Steer { .. } if self.stop.is_some() || !tree.is_active(thread, turn) => {
                    pending.reply(Outcome::Unavailable)
                }
                Action::Steer { .. }
                    if self
                        .requests
                        .values()
                        .filter(|r| matches!(r.kind, Kind::Steer { .. }))
                        .count()
                        >= 16 =>
                {
                    pending.reply(Outcome::Busy)
                }
                Action::Steer { text } => {
                    let id = allocate(next_id)?;
                    let frame = json!({"id":id,"method":"turn/steer","params":{"threadId":thread,"expectedTurnId":turn,"clientUserMessageId":pending.command_id,"input":[{"type":"text","text":text,"text_elements":[]}]}});
                    pending.begin_write();
                    self.requests.insert(
                        id,
                        Request {
                            kind: Kind::Steer {
                                pending,
                                turn: turn.into(),
                            },
                            until: Instant::now() + Duration::from_secs(10),
                        },
                    );
                    return Ok(Some(frame));
                }
            }
        }
        if let Some(scope) = self.unseen(tree) {
            let id = allocate(next_id)?;
            let key = (scope.thread.clone(), scope.turn.clone());
            let stop = self.stop.as_mut().expect("observed interrupt");
            if let Some(pending) = stop.original.as_mut() {
                pending.begin_write();
            }
            stop.targets.insert(key.clone());
            self.requests.insert(
                id,
                Request {
                    kind: Kind::Interrupt(key),
                    until: Instant::now() + Duration::from_secs(10),
                },
            );
            return Ok(Some(
                json!({"id":id,"method":"turn/interrupt","params":{"threadId":scope.thread,"turnId":scope.turn}}),
            ));
        }
        Ok(None)
    }
    /// True only when this is a response to one of our original control IDs.
    pub fn observe(&mut self, frame: &Value) -> Result<bool> {
        if frame.get("method").is_some() {
            return Ok(false);
        }
        let Some(id) = frame.get("id").and_then(Value::as_u64) else {
            return Ok(false);
        };
        if let Some(turn) = self.late.remove(&id) {
            if outcome(frame)? == Outcome::Accepted && frame["result"]["turnId"] != turn {
                return Err(Error::Protocol("late native steer turn differs"));
            }
            return Ok(true);
        }
        let Some(request) = self.requests.remove(&id) else {
            return Ok(false);
        };
        let outcome = outcome(frame)?;
        match request.kind {
            Kind::Steer { pending, turn } => {
                if outcome == Outcome::Accepted && frame["result"]["turnId"] != turn {
                    return Err(Error::Protocol("native steer turn differs"));
                }
                pending.reply(outcome);
            }
            Kind::Interrupt(key) => {
                let stop = self
                    .stop
                    .as_mut()
                    .ok_or(Error::Protocol("native interrupt lost its scope"))?;
                if !stop.targets.contains(&key) {
                    return Err(Error::Protocol("native interrupt scope differs"));
                }
                match outcome {
                    Outcome::Accepted => stop.accepted += 1,
                    Outcome::Unsupported => stop.unsupported += 1,
                    _ => stop.refused.push((key, already_ended(frame))),
                }
            }
        }
        Ok(true)
    }
    pub fn reconcile(&mut self, tree: &TurnTree) -> Result<()> {
        if self.unseen(tree).is_some()
            || self
                .requests
                .values()
                .any(|r| matches!(r.kind, Kind::Interrupt(_)))
        {
            return Ok(());
        }
        let Some(stop) = &mut self.stop else {
            return Ok(());
        };
        // A sub-agent can finish while the interrupt for it is on its way.
        // A refusal for a turn that has ended by itself is not a failed stop.
        let refused = stop
            .refused
            .iter()
            .filter(|((thread, turn), ended)| !ended && tree.is_active(thread, turn))
            .count();
        if stop.accepted == 0 {
            if refused + stop.unsupported == 0 && !tree.is_complete() {
                return Ok(());
            }
            let outcome = if refused > 0 {
                Outcome::Rejected
            } else if stop.unsupported > 0 {
                Outcome::Unsupported
            } else {
                Outcome::Unavailable
            };
            if let Some(pending) = stop.original.take() {
                pending.reply(outcome);
            }
            self.stop = None;
        } else if refused + stop.unsupported > 0 {
            if let Some(pending) = stop.original.take() {
                pending.reply(Outcome::Unknown);
            }
            return Err(Error::Protocol("native interrupt only partly accepted"));
        } else {
            // This acknowledges the targets observed so far, not completion.
            // Keep the intent: a newly observed child is interrupted too.
            if let Some(pending) = stop.original.take() {
                pending.reply(Outcome::Accepted);
            }
            stop.acknowledged = true;
        }
        Ok(())
    }
}
fn allocate(next: &mut u64) -> Result<u64> {
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or(Error::Protocol("native request ID exhausted"))?;
    Ok(id)
}
/// Codex 0.167 answers an interrupt for a turn that is already over with this
/// error (code -32600). The turn's own completion is then the outcome.
fn already_ended(frame: &Value) -> bool {
    frame["error"]["message"] == "no active turn to interrupt"
}
fn outcome(frame: &Value) -> Result<Outcome> {
    match (frame.get("result"), frame.get("error")) {
        (Some(result), None) if result.is_object() => Ok(Outcome::Accepted),
        (None, Some(error)) if error.is_object() => Ok(if error["code"] == -32601 {
            Outcome::Unsupported
        } else {
            Outcome::Rejected
        }),
        _ => Err(Error::Protocol("invalid native control response")),
    }
}
