//! One native root turn and its descendants. All input must come from the
//! retained app-server transport. IDs and counters are observations, not Matrix
//! identities, owner permissions, payment evidence, or process-cleanup proof.
use super::session::{Error, UsageBreakdown};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_TURNS: usize = 4096;
const MAX_RESPONSES: usize = 16_384;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResponseTotals {
    pub input: u64,
    pub output: u64,
    pub cached_input: u64,
    pub cache_write_input: u64,
    pub reasoning_output: u64,
}
impl ResponseTotals {
    pub(super) fn decode(value: &Value) -> Option<Self> {
        let value = UsageBreakdown::from_native(value)?;
        let result = Self {
            input: value.input_tokens()?,
            output: value.output_tokens()?,
            cached_input: value.cached_input_tokens()?,
            cache_write_input: value.cache_write_input_tokens()?,
            reasoning_output: value.reasoning_output_tokens()?,
        };
        (result.input.checked_add(result.output)? == value.total_tokens()?
            && result.cached_input.checked_add(result.cache_write_input)? <= result.input
            && result.reasoning_output <= result.output)
            .then_some(result)
    }
    pub(super) fn add(self, other: Self) -> Option<Self> {
        let add = |a: u64, b: u64| a.checked_add(b).filter(|n| *n <= 9_007_199_254_740_991);
        Some(Self {
            input: add(self.input, other.input)?,
            output: add(self.output, other.output)?,
            cached_input: add(self.cached_input, other.cached_input)?,
            cache_write_input: add(self.cache_write_input, other.cache_write_input)?,
            reasoning_output: add(self.reasoning_output, other.reasoning_output)?,
        })
    }
}

#[derive(Clone)]
struct Turn {
    started: u64,
    completed: bool,
    receipt: bool,
    outcome: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub thread: String,
    pub turn: String,
    pub root: bool,
}

#[derive(Clone)]
pub struct TurnTree {
    root_thread: String,
    root_turn: String,
    sequence: u64,
    turns: BTreeMap<(String, String), Turn>,
    expected: BTreeSet<String>,
    tool_starts: BTreeMap<(String, String), u64>,
    responses: BTreeMap<String, (String, String, Option<ResponseTotals>)>,
    invalid_usage: bool,
    last_response: Option<ResponseTotals>,
}
impl TurnTree {
    pub(crate) fn completed_native_turns(&self) -> Option<(String, BTreeMap<String,BTreeSet<String>>)> {
        if !self.is_complete() { return None; }
        let mut threads:BTreeMap<String,BTreeSet<String>>=BTreeMap::new();
        for (thread,turn) in self.turns.keys() { threads.entry(thread.clone()).or_default().insert(turn.clone()); }
        Some((self.root_turn.clone(),threads))
    }
    pub(crate) fn invalidate_native_usage(&mut self) { self.invalid_usage=true; }
    /// Alternate native transport: exact persisted response records, retaining
    /// their original thread/turn/response IDs and the same deduplication rules.
    pub(super) fn observe_rollout_receipt(&mut self, receipt: super::rollout_usage::Receipt) -> Result<(),Error> {
        if receipt.root_turn!=self.root_turn { return Err(Error::Scope); }
        let observed=self.turns.get_mut(&(receipt.thread.clone(),receipt.turn.clone())).ok_or(Error::Scope)?;
        if !observed.completed { return Err(Error::State); }
        let value=ResponseTotals::decode(&receipt.usage);
        self.invalid_usage|=value.is_none();
        let entry=(receipt.thread,receipt.turn,value);
        if let Some(old)=self.responses.get(&receipt.response) { self.invalid_usage|=old!=&entry; }
        else {
            if self.responses.len()>=MAX_RESPONSES { return Err(Error::Capacity); }
            self.responses.insert(receipt.response,entry);
            self.last_response=value;
        }
        observed.receipt=true;
        Ok(())
    }
    pub fn new(thread: &str, turn: &str) -> Result<Self, Error> {
        valid(thread)?;
        valid(turn)?;
        Ok(Self {
            root_thread: thread.into(),
            root_turn: turn.into(),
            sequence: 0,
            turns: BTreeMap::from([(
                (thread.into(), turn.into()),
                Turn {
                    started: 0,
                    completed: false,
                    receipt: false,
                    outcome: None,
                },
            )]),
            expected: BTreeSet::new(),
            tool_starts: BTreeMap::new(),
            responses: BTreeMap::new(),
            invalid_usage: false,
            last_response: None,
        })
    }
    pub fn has_descendants(&self) -> bool {
        !self.tool_starts.is_empty()
            || !self.expected.is_empty()
            || self
                .turns
                .keys()
                .any(|(thread, _)| thread != &self.root_thread)
    }
    pub fn has_response_receipts(&self) -> bool {
        !self.responses.is_empty() || self.invalid_usage
    }
    pub fn is_active(&self, thread: &str, turn: &str) -> bool {
        self.turns
            .get(&(thread.into(), turn.into()))
            .is_some_and(|turn| !turn.completed)
    }
    /// Only positively observed, unfinished turns belong to a host control.
    /// A future child is discovered by its original rootTurnId, never by a
    /// room-provided thread or turn identity.
    pub fn active_scopes(&self) -> impl Iterator<Item = Scope> + '_ {
        self.turns
            .iter()
            .filter(|(_, turn)| !turn.completed)
            .map(|((thread, turn), _)| Scope {
                thread: thread.clone(),
                turn: turn.clone(),
                root: thread == &self.root_thread,
            })
    }
    pub fn was_interrupted(&self) -> bool {
        self.turns
            .values()
            .any(|turn| turn.outcome.as_deref() == Some("interrupted"))
    }
    /// Context evidence is accepted only for an observed active native turn.
    pub fn context_usage(&self, params: &Value) -> Option<super::session::ContextUsage> {
        let thread = params["threadId"].as_str()?;
        let turn = params["turnId"].as_str()?;
        if !self.is_active(thread, turn) { return None; }
        super::session::ContextUsage::project(params, thread == self.root_thread)
    }
    pub fn contains_thread(&self, thread: &str) -> bool {
        self.turns.keys().any(|(id, _)| id == thread) || self.expected.contains(thread)
    }
    pub fn awaiting_collaboration(&self) -> bool {
        !self.tool_starts.is_empty()
    }
    /// Positive per-response evidence received so far. This does not establish
    /// that every active turn has ended or supplied its receipt.
    pub fn measured_responses(&self) -> Option<(ResponseTotals, ResponseTotals)> {
        if self.invalid_usage || self.responses.is_empty() {
            return None;
        }
        let total = self
            .responses
            .values()
            .try_fold(ResponseTotals::default(), |sum, (_, _, value)| {
                sum.add((*value)?)
            })?;
        Some((total, self.last_response?))
    }
    pub fn is_complete(&self) -> bool {
        self.tool_starts.is_empty()
            && self.expected.is_empty()
            && self.turns.values().all(|turn| turn.completed)
    }
    /// Exact response counters; cumulative child history is never added because
    /// a fork can inherit the parent's prior context and counters.
    pub fn usage(&self) -> Option<ResponseTotals> {
        if !self.is_complete()
            || self.invalid_usage
            || self.turns.values().any(|turn| !turn.receipt)
        {
            return None;
        }
        self.measured_responses().map(|(total, _)| total)
    }
    /// Unscoped telemetry (including status before a child's turn/started) is
    /// ignored. A callback requires a current scoped turn, including after the
    /// root has answered while a child remains active.
    pub fn observe(
        &mut self,
        method: &str,
        params: &Value,
        callback: bool,
    ) -> Result<Option<Scope>, Error> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= super::session::MAX_EVENTS as u64)
            .ok_or(Error::Capacity)?;
        // Raw items duplicate canonical items and include inter-turn native
        // notifications. They never carry usage or callback authority.
        if !callback
            && (method.starts_with("codex/event/")
                || matches!(method, "rawResponseItem/completed" | "thread/started"))
        {
            return Ok(None);
        }
        let Some(thread) = params.get("threadId").and_then(Value::as_str) else {
            return if callback || requires_turn(method) {
                Err(Error::Scope)
            } else {
                Ok(None)
            };
        };
        valid(thread)?;
        if !callback && method == "thread/status/changed" {
            return Ok(None);
        }
        let turn = if matches!(method, "turn/started" | "turn/completed") {
            params["turn"]["id"].as_str()
        } else {
            params.get("turnId").and_then(Value::as_str)
        };
        let Some(turn) = turn else {
            return if callback || requires_turn(method) {
                Err(Error::Scope)
            } else {
                Ok(None)
            };
        };
        valid(turn)?;
        let key = (thread.to_owned(), turn.to_owned());
        let root = thread == self.root_thread;
        if method == "turn/started" {
            let root_id = params["turn"]["rootTurnId"].as_str();
            if !(root && turn == self.root_turn && root_id.is_none())
                && root_id != Some(&self.root_turn)
            {
                return Err(Error::Scope);
            }
            if !self.turns.contains_key(&key) {
                if self.turns.len() >= MAX_TURNS {
                    return Err(Error::Capacity);
                }
                self.turns.insert(
                    key.clone(),
                    Turn {
                        started: self.sequence,
                        completed: false,
                        receipt: false,
                        outcome: None,
                    },
                );
            }
            self.expected.remove(thread);
        }
        // Older-turn resume telemetry neither grants authority nor supplies a
        // baseline for this call. Every other scoped frame must be in this tree.
        if method == "thread/tokenUsage/updated" && !self.turns.contains_key(&key) && root {
            return Ok(None);
        }
        let observed = self.turns.get_mut(&key).ok_or(Error::Scope)?;
        if observed.completed
            && (callback
                || !matches!(
                    method,
                    "turn/completed" | "rawResponse/completed" | "thread/tokenUsage/updated"
                ))
        {
            return Err(Error::Scope);
        }
        if method == "turn/completed" {
            if params["turn"]
                .get("rootTurnId")
                .is_some_and(|id| !id.is_null() && id != &self.root_turn)
            {
                return Err(Error::Scope);
            }
            if !matches!(
                params["turn"]["status"].as_str(),
                Some("completed" | "failed" | "interrupted")
            ) {
                return Err(Error::Malformed);
            }
            let status = params["turn"]["status"].as_str().ok_or(Error::Malformed)?;
            if observed
                .outcome
                .as_deref()
                .is_some_and(|previous| previous != status)
            {
                return Err(Error::Malformed);
            }
            observed.outcome = Some(status.into());
            observed.completed = true;
        }
        if method == "rawResponse/completed" {
            observed.receipt = true;
            let response = params["responseId"].as_str().ok_or(Error::Malformed)?;
            valid(response)?;
            let value = ResponseTotals::decode(&params["usage"]);
            self.invalid_usage |= value.is_none();
            let receipt = (thread.to_owned(), turn.to_owned(), value);
            if let Some(previous) = self.responses.get(response) {
                self.invalid_usage |= previous != &receipt;
            } else {
                if self.responses.len() >= MAX_RESPONSES {
                    return Err(Error::Capacity);
                }
                self.responses.insert(response.into(), receipt);
                self.last_response = value;
            }
        }
        if matches!(method, "item/started" | "item/completed")
            && params["item"]["type"] == "collabAgentToolCall"
        {
            self.collaboration(thread, method == "item/completed", &params["item"])?;
        }
        Ok(Some(Scope {
            thread: thread.into(),
            turn: turn.into(),
            root,
        }))
    }
    fn collaboration(&mut self, thread: &str, completed: bool, item: &Value) -> Result<(), Error> {
        let id = item["id"].as_str().ok_or(Error::Malformed)?;
        valid(id)?;
        let key = (thread.to_owned(), id.to_owned());
        if !completed {
            if self.tool_starts.len() >= MAX_RESPONSES {
                return Err(Error::Capacity);
            }
            self.tool_starts.entry(key).or_insert(self.sequence);
            return Ok(());
        }
        let started = self.tool_starts.remove(&key).unwrap_or(0);
        if !matches!(
            item["tool"].as_str(),
            Some("spawnAgent" | "resumeAgent" | "sendInput" | "followupTask")
        ) || item["status"] != "completed"
        {
            return Ok(());
        }
        let receivers = item["receiverThreadIds"]
            .as_array()
            .ok_or(Error::Malformed)?;
        if receivers.len() > MAX_TURNS {
            return Err(Error::Capacity);
        }
        for receiver in receivers {
            let receiver = receiver.as_str().ok_or(Error::Malformed)?;
            valid(receiver)?;
            let status = item["agentsStates"][receiver]["status"].as_str();
            // A fast child can finish before its listener is attached. Its
            // completed status still owes this call its turn and response
            // events. Merely resuming an idle agent does not start a new turn.
            if !matches!(status, Some("pendingInit" | "running"))
                && !(status == Some("completed") && item["tool"] != "resumeAgent")
            {
                continue;
            }
            if !self.turns.iter().any(|((id, _), turn)| {
                id == receiver && (turn.started > started || !turn.completed)
            }) {
                self.expected.insert(receiver.into());
                if self.expected.len() > MAX_TURNS {
                    return Err(Error::Capacity);
                }
            }
        }
        Ok(())
    }
}
fn valid(id: &str) -> Result<(), Error> {
    if super::text(id, 1024) {
        Ok(())
    } else {
        Err(Error::Malformed)
    }
}
fn requires_turn(method: &str) -> bool {
    matches!(
        method,
        "thread/tokenUsage/updated"
            | "item/started"
            | "item/completed"
            | "turn/started"
            | "turn/completed"
            | "rawResponse/completed"
    )
}
