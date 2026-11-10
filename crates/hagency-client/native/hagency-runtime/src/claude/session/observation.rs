//! Immutable, private-constructor receipts from one admitted native stream.
use super::{Error, Message, Phase, SessionDriver, UsageEvidence};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone)]
pub struct ObservationSource {
    live: Arc<AtomicBool>,
    session: String,
}
impl PartialEq for ObservationSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.live, &other.live) && self.session == other.session
    }
}
impl Eq for ObservationSource {}
impl ObservationSource {
    pub fn is_retired(&self) -> bool {
        !self.live.load(Ordering::Acquire)
    }
}

/// Original answer completion, independent of the subsequent accounting
/// query. Private construction requires the native result of this stream and
/// either native idle with no live descendants (`completion_receipt`) or a
/// stream that has since failed or closed (`closed_answer_receipt`). Neither
/// carries usage. Retirement does not erase this already observed evidence.
pub struct CompletionReceipt {
    source: ObservationSource,
    sequence: u64,
    text_digest: [u8; 32],
    is_error: bool,
    interrupted: bool,
}
impl CompletionReceipt {
    pub fn source(&self) -> &ObservationSource {
        &self.source
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn is_error(&self) -> bool {
        self.is_error
    }
    pub fn interrupted(&self) -> bool {
        self.interrupted
    }
    pub fn matches_text(&self, text: &str) -> bool {
        use sha2::{Digest, Sha256};
        self.text_digest == <[u8; 32]>::from(Sha256::digest(text.as_bytes()))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ObservationKind {
    Ignored,
    Invalidated,
    Usage(UsageEvidence),
    Result {
        usage: UsageEvidence,
        is_error: bool,
    },
}
#[derive(Clone, PartialEq, Eq)]
pub struct Observation {
    source: ObservationSource,
    sequence: u64,
    kind: ObservationKind,
    result_error: Option<bool>,
    accounting_snapshot: bool,
    continues_answer: bool,
}
/// The turn's answer when a steer ran as a cycle of its own: the non-empty
/// result texts in order, separated by a blank line. The completion receipt
/// verifies exactly this string.
pub fn join_answer(answer: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !answer.is_empty() {
        answer.push_str("\n\n");
    }
    answer.push_str(text);
}
impl Observation {
    pub fn source(&self) -> &ObservationSource {
        &self.source
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn kind(&self) -> &ObservationKind {
        &self.kind
    }
    /// Native result identity survives a rejected numeric projection.
    pub fn result_error(&self) -> Option<bool> {
        self.result_error
    }
    pub fn is_usage_snapshot(&self) -> bool {
        self.accounting_snapshot
    }
    /// This result closed a steer's own cycle. Join its text to the answer
    /// read so far with `join_answer`; any other result replaces the answer.
    pub fn continues_answer(&self) -> bool {
        self.continues_answer
    }
}
pub(super) struct State {
    live: Arc<AtomicBool>,
    sequence: u64,
    last: Option<Observation>,
    usage: super::usage::Tracker,
}
impl Default for State {
    fn default() -> Self {
        Self {
            live: Arc::new(AtomicBool::new(true)),
            sequence: 0,
            last: None,
            usage: Default::default(),
        }
    }
}
impl State {
    pub(super) fn completed(
        &self,
        session: &str,
        text_digest: [u8; 32],
        is_error: bool,
        interrupted: bool,
    ) -> CompletionReceipt {
        CompletionReceipt {
            source: self.source(session),
            sequence: self.sequence,
            text_digest,
            is_error,
            interrupted,
        }
    }
    pub(super) fn retire(&self) {
        self.live.store(false, Ordering::Release);
    }
    pub(super) fn accept(
        &mut self,
        message: &Message,
        session: &str,
        accounting_snapshot: bool,
        continues_answer: bool,
    ) -> Result<(), Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let kind = match message {
            Message::Event { kind, payload, .. } => self.usage.project(*kind, payload),
            _ => ObservationKind::Ignored,
        };
        self.last = Some(Observation {
            source: self.source(session),
            sequence: self.sequence,
            kind,
            accounting_snapshot,
            continues_answer,
            result_error: match message {
                Message::Event {
                    kind: super::EventKind::Result,
                    payload,
                    ..
                } => Some(payload["is_error"] == true),
                _ => None,
            },
        });
        Ok(())
    }
    fn source(&self, session: &str) -> ObservationSource {
        ObservationSource {
            live: self.live.clone(),
            session: session.into(),
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.retire();
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    /// The system/init receipt is the baseline (sequence1), not usage. Late
    /// attachment is refused even if the caller used only ordinary reads.
    pub fn observation_source(&self) -> Result<ObservationSource, Error> {
        if self.phase != Phase::Running || self.observations.sequence != 1 {
            return Err(Error::State);
        }
        Ok(self
            .observations
            .source(self.session_id.as_deref().ok_or(Error::State)?))
    }
    pub fn matches_observation_source(&self, source: &ObservationSource) -> bool {
        matches!(self.phase, Phase::Running | Phase::ResultObserved)
            && !source.is_retired()
            && self
                .session_id
                .as_deref()
                .is_some_and(|id| &self.observations.source(id) == source)
    }
    /// Clone immediately after each returned Message on any read/control path.
    /// Control/flush returns leave this unchanged; replay is rejected downstream.
    pub fn last_observation(&self) -> Option<&Observation> {
        self.observations.last.as_ref()
    }
}
