//! Immutable, private-constructor receipts from one admitted Octos session.
//! Numeric projection only; no provider, task, source or billing authority.
use super::{Error, Event, Outcome, Phase, SessionDriver, Usage};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub const MAX_OBSERVATIONS: u64 = 16_384;

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

/// The final answer and outcome observed on this original session. The
/// digest binds the text without exposing it in a serializable receipt.
/// Completion alone is not proof of complete or billable usage.
#[derive(Clone)]
pub struct CompletionReceipt {
    source: ObservationSource,
    sequence: u64,
    outcome: Outcome,
    text_digest: [u8; 32],
}
impl CompletionReceipt {
    pub(super) fn new(source: ObservationSource, sequence: u64, outcome: Outcome, text: &str) -> Self {
        Self { source, sequence, outcome, text_digest: Sha256::digest(text.as_bytes()).into() }
    }
    pub fn source(&self) -> &ObservationSource { &self.source }
    pub fn sequence(&self) -> u64 { self.sequence }
    pub fn outcome(&self) -> Outcome { self.outcome }
    pub fn matches_text(&self, text: &str) -> bool {
        self.text_digest == <[u8; 32]>::from(Sha256::digest(text.as_bytes()))
    }
}

/// Which record the counters come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageCoverage {
    /// The sum of the ended turns' own `token_usage`.
    Turns,
    /// Octos's totals for the whole session, its sub-agents included.
    Session,
}
/// Fresh input apart from cache reads and writes, as Octos normalizes every
/// provider; reasoning is part of output. `None` is unknown, never zero.
#[derive(Clone, PartialEq, Eq)]
pub struct UsageEvidence {
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    coverage: UsageCoverage,
    turns: u32,
}
impl UsageEvidence {
    pub fn input(&self) -> Option<u64> {
        self.input
    }
    pub fn output(&self) -> Option<u64> {
        self.output
    }
    pub fn reasoning(&self) -> Option<u64> {
        self.reasoning
    }
    pub fn cache_read(&self) -> Option<u64> {
        self.cache_read
    }
    pub fn cache_write(&self) -> Option<u64> {
        self.cache_write
    }
    pub fn coverage(&self) -> UsageCoverage {
        self.coverage
    }
    /// Ended turns counted so far.
    pub fn turns(&self) -> u32 {
        self.turns
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ObservationKind {
    Ignored,
    Invalidated,
    /// A turn ended: the session's usage so far.
    Usage(UsageEvidence),
    /// Octos went idle: the dispatch's final usage.
    Idle(UsageEvidence),
}
#[derive(Clone, PartialEq, Eq)]
pub struct Observation {
    source: ObservationSource,
    sequence: u64,
    kind: ObservationKind,
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
}
pub(super) struct State {
    live: Arc<AtomicBool>,
    sequence: u64,
    last: Option<Observation>,
    turns: u32,
    /// The known sums, and whether a terminal carried no usage.
    sums: Usage,
    unknown: bool,
    totals: Option<Usage>,
    invalidated: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            live: Arc::new(AtomicBool::new(true)),
            sequence: 0,
            last: None,
            turns: 0,
            sums: Usage::default(),
            unknown: false,
            totals: None,
            invalidated: false,
        }
    }
}
impl State {
    pub(super) fn last(&self) -> Option<&Observation> { self.last.as_ref() }
    pub(super) fn retire(&self) {
        self.live.store(false, Ordering::Release);
    }
    pub(super) fn turn_ended(&mut self, usage: Option<Usage>) {
        self.turns = self.turns.saturating_add(1);
        match usage {
            None => self.unknown = true,
            Some(usage) => match self.sums.checked_add(usage).filter(bounded) {
                Some(sums) => self.sums = sums,
                None => self.invalidated = true,
            },
        }
    }
    pub(super) fn session_totals(&mut self, totals: Option<Usage>) {
        self.totals = totals;
    }
    fn turn_evidence(&self) -> UsageEvidence {
        let known = |n: u64| (!self.unknown).then_some(n);
        UsageEvidence {
            input: known(self.sums.input),
            output: known(self.sums.output),
            reasoning: known(self.sums.reasoning),
            cache_read: known(self.sums.cache_read),
            cache_write: known(self.sums.cache_write),
            coverage: UsageCoverage::Turns,
            turns: self.turns,
        }
    }
    /// The session's totals when every turn reported complete usage and the
    /// ledger covers those reports. Totals alone do not prove that a missing
    /// turn was included: the ledger may contain earlier calls.
    fn final_evidence(&self) -> UsageEvidence {
        let s = self.sums;
        if self.unknown { return self.turn_evidence(); }
        match self.totals.filter(|t| {
            t.input >= s.input
                && t.output >= s.output
                && t.cache_read >= s.cache_read
                && t.cache_write >= s.cache_write
        }) {
            Some(t) if bounded(&t) => UsageEvidence {
                input: Some(t.input),
                output: Some(t.output),
                // The session totals carry no reasoning breakdown.
                reasoning: None,
                cache_read: Some(t.cache_read),
                cache_write: Some(t.cache_write),
                coverage: UsageCoverage::Session,
                turns: self.turns,
            },
            _ => self.turn_evidence(),
        }
    }
    pub(super) fn accept(&mut self, event: &Event, session: &str) -> Result<(), Error> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= MAX_OBSERVATIONS)
            .ok_or(Error::Capacity)?;
        let kind = match event {
            _ if self.invalidated => ObservationKind::Invalidated,
            Event::TurnEnded { .. } => ObservationKind::Usage(self.turn_evidence()),
            Event::Idle(_) => ObservationKind::Idle(self.final_evidence()),
            _ => ObservationKind::Ignored,
        };
        self.last = Some(Observation {
            source: self.source(session),
            sequence: self.sequence,
            kind,
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
/// Every counter and their sum stay exact in a JSON number.
fn bounded(usage: &Usage) -> bool {
    const MAX: u64 = 9_007_199_254_740_991;
    usage
        .input
        .checked_add(usage.output)
        .and_then(|n| n.checked_add(usage.cache_read))
        .and_then(|n| n.checked_add(usage.cache_write))
        .is_some_and(|n| n <= MAX)
        && usage.reasoning <= MAX
}
impl<R, W, E> SessionDriver<R, W, E> {
    pub fn completion_receipt(&self) -> Option<&CompletionReceipt> {
        (self.phase == Phase::Idle).then_some(self.completion.as_ref()).flatten()
            .filter(|receipt| !receipt.source.is_retired())
    }
    /// Taken once the dispatch's turn is accepted, before any event is read:
    /// the baseline is sequence 0. Late attachment is refused.
    pub fn observation_source(&self) -> Result<ObservationSource, Error> {
        if self.phase != Phase::Running || self.observations.sequence != 0 {
            return Err(Error::State);
        }
        Ok(self
            .observations
            .source(self.session_id.as_deref().ok_or(Error::State)?))
    }
    pub fn matches_observation_source(&self, source: &ObservationSource) -> bool {
        matches!(self.phase, Phase::Running | Phase::Idle)
            && !source.is_retired()
            && self
                .session_id
                .as_deref()
                .is_some_and(|id| &self.observations.source(id) == source)
    }
    /// Clone immediately after each returned event; replay is rejected
    /// downstream.
    pub fn last_observation(&self) -> Option<&Observation> {
        self.observations.last.as_ref()
    }
}
