//! Durable owner-local inbox. Matrix/server delivery ACK never means model completion.
//! This module has no network or server/model credentials. Its host must verify
//! the current server user/device lease before handing it a wire envelope.
use crate::{Error, Ledger, Result, Scope, call_digest, hash, json, key, verify};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

// Capacity is serialized inbox payload plus a conservative fixed allowance for
// immutable digest (64), execution ID (<=80), and reply digest (64). SQLite page,
// index and WAL overhead are not a byte-exact file-size or secure-erasure promise.
const PROOF_BYTES: usize = 208;
const ACTIVE_RECORDS: &str = "state NOT IN ('replied','unknown') AND NOT (state IN ('rejected','failed') AND json_extract(envelope,'$.body')='')";
const RETAINED_BYTES: &str = "SELECT coalesce(sum(length(CAST(envelope AS BLOB))+coalesce(length(CAST(reply AS BLOB)),0)+208),0) FROM local_inbox";
fn no_provider_usage() -> crate::Usage {
    crate::Usage {
        input: 0,
        output: 0,
        cached_input: 0,
        cache_write_input: None,
        reasoning_output: 0,
        accounting_version: "no-provider-call-v1".into(),
    }
}
fn retained_bytes(db: &rusqlite::Connection) -> Result<u64> {
    Ok(db.query_row(RETAINED_BYTES, [], |r| r.get(0))?)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dispatch {
    pub id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub event_id: String,
    pub room_id: String,
    pub requester_mxid: String,
    pub thread_root: String,
    pub body: String,
    pub state: String,
    pub binding_generation: i64,
    pub dispatch_epoch: Option<i64>,
    pub dispatch_device_id: Option<String>,
    pub execution_id: Option<String>,
    pub outcome: Option<String>,
}
impl Dispatch {
    pub fn scope(&self) -> Scope {
        Scope {
            agent: self.agent_id.clone(),
            binding: self.binding_id.clone(),
            room: self.room_id.clone(),
            requester: self.requester_mxid.clone(),
            thread: self.thread_root.clone(),
        }
    }
    fn validate(&self) -> Result<()> {
        self.scope().validate()?;
        for value in [&self.id, &self.event_id] {
            key(value)?;
        }
        // An empty body marks an encrypted owner-direct request: the server
        // routes only ciphertext and the executor's Agent device reads it.
        if !self.event_id.starts_with('$')
            || !(self.thread_root.starts_with('$') || self.thread_root == self.room_id)
            || self.binding_generation <= 0
            || self.body.len() > 65536
        {
            return Err(Error::Invalid("dispatch identity/content"));
        }
        if self.dispatch_epoch.is_none_or(|v| v <= 0)
            || self
                .dispatch_device_id
                .as_deref()
                .is_none_or(|v| key(v).is_err())
        {
            return Err(Error::Invalid("missing device delivery lease"));
        }
        Ok(())
    }
    fn digest(&self) -> Result<String> {
        hash(&(
            &self.id,
            &self.binding_id,
            &self.agent_id,
            &self.event_id,
            &self.room_id,
            &self.requester_mxid,
            &self.thread_root,
            &self.body,
            self.binding_generation,
        ))
    }
}
/// Metadata-only server evidence. No model cost or message body crosses this interface.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryExecution {
    pub dispatch_id: String,
    pub execution_id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub event_id: String,
    pub room_id: String,
    pub requester_mxid: String,
    pub thread_root: String,
    pub binding_generation: i64,
    pub dispatch_epoch: i64,
    pub dispatch_device_id: String,
    pub immutable_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerStart {
    pub dispatch: Dispatch,
    pub newly_started: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Received,
    Acknowledged,
    Prepared,
    Running,
    ReplyReady,
    Replied,
    Rejected,
    Failed,
    Unknown,
}
impl State {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "received" => Self::Received,
            "acknowledged" => Self::Acknowledged,
            "prepared" => Self::Prepared,
            "running" => Self::Running,
            "reply_ready" => Self::ReplyReady,
            "replied" => Self::Replied,
            "rejected" => Self::Rejected,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            _ => return Err(Error::Invalid("inbox state")),
        })
    }
}
#[derive(Clone, Debug)]
pub struct Record {
    pub dispatch: Dispatch,
    pub state: State,
    pub execution_id: Option<String>,
    pub reply: Option<String>,
}
/// Created only after a durable insert/identity check. The host may now send ACK.
#[derive(Debug)]
pub struct Receipt {
    pub dispatch_id: String,
    pub already_durable: bool,
}
/// Stable across HTTP start retry and process restart, generated before sending start.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub dispatch: Dispatch,
    pub execution_id: String,
}
/// Single-process start authorization. Not Clone/Serialize/Deserialize.
/// The host consumes this once to enter the provider; retry must re-query state.
#[derive(Debug)]
pub struct RunPermit {
    prepared: Prepared,
}
impl RunPermit {
    pub fn into_prepared(self) -> Prepared {
        self.prepared
    }
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub max_records: usize,
    pub max_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: 10000,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxCapacity {
    pub active_records: u64,
    pub replied_receipts: u64,
    /// Serialized retained payload plus bounded proof allowance, not DB file size.
    pub charged_bytes: u64,
}
#[derive(Debug, Clone, Copy)]
pub enum Finish {
    Rejected,
    Failed,
    Unknown,
}

/// Compact only a terminal record with an exact, retained settlement proof.
/// This neither deletes started witnesses nor changes calls/accounts/holds.
fn compact_known_terminals(db: &rusqlite::Connection) -> Result<()> {
    let mut stmt = db.prepare("SELECT id,envelope,execution_id,state FROM local_inbox WHERE state IN ('rejected','failed','unknown') AND json_extract(envelope,'$.body')!=''")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for (id, encoded, execution, terminal) in rows {
        let Some(execution) = execution else {
            continue;
        };
        let mut d: Dispatch = serde_json::from_str(&encoded)?;
        let call: Option<(String, String, u64, String, Option<String>, String)> = db
            .query_row(
                "SELECT scope,state,reserved,snapshots,usage,digest FROM calls WHERE id=?",
                [&execution],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((scope, state, reserved, snapshots, usage, digest)) = call else {
            // A terminal attempt without any recorded provider charge is zero.
            // Keep its immutable identity witness, release only the bulky body.
            d.body.clear();
            db.execute(
                "UPDATE local_inbox SET envelope=? WHERE id=?",
                (json(&d)?, &id),
            )?;
            continue;
        };
        if state != "settled" || scope != json(&d.scope())? {
            continue;
        }
        let Some(usage) = usage else {
            continue;
        };
        let usage: crate::Usage = serde_json::from_str(&usage)?;
        let total = usage.total()?;
        let snapshots: Vec<crate::Snapshot> = serde_json::from_str(&snapshots)?;
        let zero_proof = terminal == "rejected"
            && reserved == 0
            && snapshots.is_empty()
            && total == 0
            && usage.accounting_version == "no-provider-call-v1"
            && digest == hash(&("no-provider-call", &id, &execution, &d.scope()))?;
        let settled_proof = snapshots.len() == 3
            && digest == call_digest(&d.scope(), &id, reserved)?
            && snapshots
                .iter()
                .map(|s| s.scope.as_str())
                .collect::<Vec<_>>()
                == d.scope()
                    .layers()?
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
        if !zero_proof && !settled_proof {
            continue;
        }
        let mut charged = true;
        for snapshot in &snapshots {
            let spent: Option<u64> = db
                .query_row(
                    "SELECT spent FROM accounts WHERE scope=? AND window=?",
                    (&snapshot.scope, &snapshot.window),
                    |r| r.get(0),
                )
                .optional()?;
            charged &= spent.is_some_and(|n| n >= total);
        }
        if !charged {
            continue;
        }
        d.body.clear();
        db.execute(
            "UPDATE local_inbox SET envelope=? WHERE id=?",
            (json(&d)?, &id),
        )?;
    }
    Ok(())
}

impl Ledger {
    /// Persist before network ACK; duplicate identity/body changes are a conflict.
    pub fn receive_dispatch(
        &mut self,
        owner: &str,
        event: &Dispatch,
        limits: Limits,
        now: i64,
    ) -> Result<Receipt> {
        self.owner(owner)?;
        event.validate()?;
        verify(&self.db, &event.scope())?;
        if !matches!(event.state.as_str(), "offered" | "acknowledged")
            || event.execution_id.is_some()
            || event.outcome.is_some()
            || now < 0
        {
            return Err(Error::Invalid("dispatch is not offerable"));
        }
        if limits.max_records == 0 || limits.max_bytes == 0 {
            return Err(Error::Invalid("inbox capacity"));
        }
        let digest = event.digest()?;
        let encoded = json(event)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE inbox_limits SET max_records=?,max_bytes=? WHERE singleton=1",
            (limits.max_records as u64, limits.max_bytes as u64),
        )?;
        let existing: Option<(String, String, String)> = tx
            .query_row(
                "SELECT immutable_digest,state,envelope FROM local_inbox WHERE id=?",
                [&event.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((old, state, old_envelope)) = existing {
            if old != digest {
                return Err(Error::Conflict);
            }
            // A different live lease can redeliver only an invocation not yet prepared.
            if matches!(state.as_str(), "received" | "acknowledged") {
                let prior: Dispatch = serde_json::from_str(&old_envelope)?;
                if event.dispatch_epoch < prior.dispatch_epoch {
                    tx.commit()?;
                    return Ok(Receipt {
                        dispatch_id: event.id.clone(),
                        already_durable: true,
                    });
                }
                if event.dispatch_epoch == prior.dispatch_epoch
                    && event.dispatch_device_id != prior.dispatch_device_id
                {
                    return Err(Error::Conflict);
                }
                if encoded.len() > old_envelope.len() {
                    let bytes = retained_bytes(&tx)?;
                    if bytes.saturating_add((encoded.len() - old_envelope.len()) as u64)
                        > limits.max_bytes as u64
                    {
                        return Err(Error::Invalid("inbox full"));
                    }
                }
                tx.execute(
                    "UPDATE local_inbox SET envelope=? WHERE id=?",
                    (&encoded, &event.id),
                )?;
            }
            tx.commit()?;
            return Ok(Receipt {
                dispatch_id: event.id.clone(),
                already_durable: true,
            });
        }
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_inbox WHERE binding=? AND event_id=?)",
            (&event.binding_id, &event.event_id),
            |r| r.get(0),
        )?;
        if duplicate {
            return Err(Error::Conflict);
        }
        // Previously retained terminal rows are compacted only with settled
        // cost proof; unknown charges and unsent outputs remain fully retained.
        compact_known_terminals(&tx)?;
        let count: u64 = tx.query_row(
            &format!("SELECT count(*) FROM local_inbox WHERE {ACTIVE_RECORDS}"),
            [],
            |r| r.get(0),
        )?;
        let bytes = retained_bytes(&tx)?;
        if count >= limits.max_records as u64
            || bytes.saturating_add(encoded.len().saturating_add(PROOF_BYTES) as u64)
                > limits.max_bytes as u64
        {
            return Err(Error::Invalid("inbox full"));
        }
        tx.execute(
            "INSERT INTO local_inbox VALUES (?,?,?,?,?,'received',NULL,NULL,NULL,?)",
            (
                &event.id,
                &event.binding_id,
                &event.event_id,
                digest,
                encoded,
                now,
            ),
        )?;
        tx.commit()?;
        Ok(Receipt {
            dispatch_id: event.id.clone(),
            already_durable: false,
        })
    }
    /// Invoke after server ACK succeeds; a lost ACK response may be retried safely.
    pub fn acknowledge_dispatch(&mut self, owner: &str, id: &str) -> Result<()> {
        self.owner(owner)?;
        self.inbox_record(owner, id)?;
        self.db.execute(
            "UPDATE local_inbox SET state='acknowledged' WHERE id=? AND state='received'",
            [id],
        )?;
        Ok(())
    }
    pub fn inbox_record(&self, owner: &str, id: &str) -> Result<Record> {
        self.owner(owner)?;
        key(id)?;
        let (event, state, execution_id, reply): (String, String, Option<String>, Option<String>) =
            self.db.query_row(
                "SELECT envelope,state,execution_id,reply FROM local_inbox WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let dispatch: Dispatch = serde_json::from_str(&event)?;
        verify(&self.db, &dispatch.scope())?;
        Ok(Record {
            dispatch,
            state: State::parse(&state)?,
            execution_id,
            reply,
        })
    }
    /// Persist stable execution identity BEFORE posting server start. Never prepares
    /// running, uncertain or completed work for a second provider invocation.
    pub fn prepare_execution(&mut self, owner: &str, id: &str) -> Result<Prepared> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (encoded, state, execution_id): (String, String, Option<String>) = tx.query_row(
            "SELECT envelope,state,execution_id FROM local_inbox WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let event: Dispatch = serde_json::from_str(&encoded)?;
        verify(&tx, &event.scope())?;
        let execution_id = match state.as_str() {
            "prepared" => execution_id.ok_or(Error::Conflict)?,
            "acknowledged" => {
                let mut random = [0u8; 32];
                getrandom::fill(&mut random).map_err(|_| Error::Invalid("execution randomness"))?;
                let execution = format!(
                    "exec_{}",
                    random
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                tx.execute(
                    "UPDATE local_inbox SET state='prepared',execution_id=? WHERE id=?",
                    (&execution, id),
                )?;
                execution
            }
            _ => return Err(Error::Conflict),
        };
        tx.commit()?;
        Ok(Prepared {
            dispatch: event,
            execution_id,
        })
    }
    /// Only newlyStarted=true, exact scope/lease and a local prepared state mint
    /// a one-use permit. A duplicate server start never starts a second model.
    pub fn confirm_execution_start(
        &mut self,
        owner: &str,
        response: &ServerStart,
    ) -> Result<Option<RunPermit>> {
        self.owner(owner)?;
        response.dispatch.validate()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (encoded, digest, state, execution_id): (String, String, String, Option<String>) = tx
            .query_row(
            "SELECT envelope,immutable_digest,state,execution_id FROM local_inbox WHERE id=?",
            [&response.dispatch.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let stored: Dispatch = serde_json::from_str(&encoded)?;
        if digest != response.dispatch.digest()?
            || response.dispatch.state != "running"
            || response.dispatch.outcome.is_some()
            || response.dispatch.execution_id != execution_id
            || execution_id.is_none()
            || stored.dispatch_epoch != response.dispatch.dispatch_epoch
            || stored.dispatch_device_id != response.dispatch.dispatch_device_id
        {
            return Err(Error::Conflict);
        }
        verify(&tx, &stored.scope())?;
        let permit = if state == "prepared" && response.newly_started {
            tx.execute(
                "UPDATE local_inbox SET state='running' WHERE id=?",
                [&stored.id],
            )?;
            Some(RunPermit {
                prepared: Prepared {
                    dispatch: stored,
                    execution_id: execution_id.unwrap(),
                },
            })
        } else if state == "prepared" {
            tx.execute(
                "UPDATE local_inbox SET state='unknown' WHERE id=?",
                [&stored.id],
            )?;
            None
        } else {
            None
        };
        tx.commit()?;
        Ok(permit)
    }
    /// Record model output before sending server reply. Retry the same reply only.
    pub fn persist_execution_reply(
        &mut self,
        owner: &str,
        id: &str,
        execution: &str,
        body: &str,
    ) -> Result<()> {
        self.owner(owner)?;
        if body.is_empty() || body.len() > 65536 {
            return Err(Error::Invalid("reply size"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, actual, old): (String, Option<String>, Option<String>) = tx.query_row(
            "SELECT state,execution_id,reply_digest FROM local_inbox WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if actual.as_deref() != Some(execution) {
            return Err(Error::Unauthorized);
        }
        let digest = hash(&body)?;
        if matches!(state.as_str(), "reply_ready" | "replied") {
            return if old.as_deref() == Some(&digest) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        if state != "running" {
            return Err(Error::Conflict);
        }
        let bytes = retained_bytes(&tx)?;
        let maximum: u64 = tx.query_row(
            "SELECT max_bytes FROM inbox_limits WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if bytes.saturating_add(body.len() as u64) > maximum {
            return Err(Error::Invalid("inbox reply capacity exhausted"));
        }
        tx.execute(
            "UPDATE local_inbox SET state='reply_ready',reply=?,reply_digest=? WHERE id=?",
            (body, digest, id),
        )?;
        tx.commit()?;
        Ok(())
    }
    /// An owner-policy notice may be delivered only when this exact execution
    /// has never reserved a provider call. The zero-call proof and immutable
    /// reply commit together; existing model charges are never rewritten.
    pub fn persist_no_provider_reply(
        &mut self,
        owner: &str,
        id: &str,
        execution: &str,
        body: &str,
    ) -> Result<()> {
        self.owner(owner)?;
        if body.is_empty() || body.len() > 65536 {
            return Err(Error::Invalid("reply size"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (encoded, state, actual, old): (String, String, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT envelope,state,execution_id,reply_digest FROM local_inbox WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        if actual.as_deref() != Some(execution) {
            return Err(Error::Unauthorized);
        }
        let dispatch: Dispatch = serde_json::from_str(&encoded)?;
        if dispatch.id != id {
            return Err(Error::Conflict);
        }
        let scope = json(&dispatch.scope())?;
        let proof_digest = hash(&("no-provider-call", id, execution, &dispatch.scope()))?;
        let usage = json(&no_provider_usage())?;
        let digest = hash(&body)?;
        type Proof = (String, String, u64, String, String, Option<String>, String);
        let existing: Option<Proof> = tx
            .query_row(
                "SELECT binding,scope,reserved,snapshots,state,usage,digest FROM calls WHERE id=?",
                [execution],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        if matches!(state.as_str(), "reply_ready" | "replied") {
            let expected = (
                dispatch.binding_id,
                scope,
                0,
                "[]".into(),
                "settled".into(),
                Some(usage),
                proof_digest,
            );
            return if old.as_deref() == Some(&digest) && existing.as_ref() == Some(&expected) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        if state != "running" || existing.is_some() {
            return Err(Error::Conflict);
        }
        let maximum: u64 = tx.query_row(
            "SELECT max_bytes FROM inbox_limits WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if retained_bytes(&tx)?.saturating_add(body.len() as u64) > maximum {
            return Err(Error::Invalid("inbox reply capacity exhausted"));
        }
        tx.execute(
            "INSERT INTO calls VALUES (?,?,?,?,0,'[]','settled',?)",
            (execution, &dispatch.binding_id, scope, proof_digest, usage),
        )?;
        tx.execute(
            "UPDATE local_inbox SET state='reply_ready',reply=?,reply_digest=? WHERE id=?",
            (body, digest, id),
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Only a trusted host's scoped `sent` receipt with a Matrix event ID
    /// permits this terminal transition. Outbox acceptance remains reply_ready.
    pub fn confirm_matrix_delivery(
        &mut self,
        owner: &str,
        id: &str,
        execution: &str,
    ) -> Result<()> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (encoded, state, actual): (String, String, Option<String>) = tx.query_row(
            "SELECT envelope,state,execution_id FROM local_inbox WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut dispatch: Dispatch = serde_json::from_str(&encoded)?;
        verify(&tx, &dispatch.scope())?;
        if actual.as_deref() != Some(execution)
            || !matches!(state.as_str(), "reply_ready" | "replied")
        {
            return Err(Error::Conflict);
        }
        // Only a proved Matrix-sent result can lose its large text payloads.
        // The original immutable_digest is never recomputed from this compact
        // envelope. Incoming full-body redelivery is still checked against it.
        dispatch.body.clear();
        tx.execute("UPDATE local_inbox SET state='replied',envelope=?,reply=NULL WHERE id=? AND state IN ('reply_ready','replied')",
            (json(&dispatch)?,id))?;
        tx.commit()?;
        Ok(())
    }
    pub fn finish_local_execution(
        &mut self,
        owner: &str,
        id: &str,
        execution: &str,
        outcome: Finish,
    ) -> Result<()> {
        self.owner(owner)?;
        let target = match outcome {
            Finish::Rejected => "rejected",
            Finish::Failed => "failed",
            Finish::Unknown => "unknown",
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, actual): (String, Option<String>) = tx.query_row(
            "SELECT state,execution_id FROM local_inbox WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if actual.as_deref() != Some(execution) {
            return Err(Error::Unauthorized);
        }
        if state == target {
            compact_known_terminals(&tx)?;
            tx.commit()?;
            return Ok(());
        }
        if !matches!(state.as_str(), "prepared" | "running") {
            return Err(Error::Conflict);
        }
        if target == "rejected" {
            let encoded: String =
                tx.query_row("SELECT envelope FROM local_inbox WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
            let dispatch: Dispatch = serde_json::from_str(&encoded)?;
            let usage = no_provider_usage();
            tx.execute(
                "INSERT INTO calls VALUES (?,?,?,?,0,'[]','settled',?) ON CONFLICT(id) DO NOTHING",
                (
                    execution,
                    &dispatch.binding_id,
                    json(&dispatch.scope())?,
                    hash(&("no-provider-call", id, execution, &dispatch.scope()))?,
                    json(&usage)?,
                ),
            )?;
        }
        tx.execute("UPDATE local_inbox SET state=? WHERE id=?", (target, id))?;
        compact_known_terminals(&tx)?;
        tx.commit()?;
        Ok(())
    }
    /// One exclusive execution-device restart, before polling new work. Uncertain
    /// starts cannot be returned to the queue; known durable reply retries survive.
    pub fn recover_local_executions(&mut self, owner: &str) -> Result<usize> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE local_inbox SET state='unknown' WHERE state IN ('prepared','running')",
            [],
        )?;
        crate::recover_pending_calls(&tx, owner, None)?;
        compact_known_terminals(&tx)?;
        tx.commit()?;
        Ok(changed)
    }
    pub fn has_known_reply(&self, owner: &str, agent: &str, binding: &str) -> Result<bool> {
        self.owner(owner)?;
        key(agent)?;
        key(binding)?;
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM local_inbox i JOIN bindings b ON b.id=i.binding WHERE b.agent=? AND i.binding=? AND i.state='reply_ready' AND i.execution_id IS NOT NULL AND length(i.reply)>0)",(agent,binding),|r|r.get(0))?)
    }
    /// Retain execution identity and validate every available accounting receipt.
    /// An absent receipt remains unmeasured, without blocking later requests or
    /// creating a zero-usage settlement. Mismatched execution custody still fails.
    pub fn covers_execution_history(
        &self,
        owner: &str,
        agent: &str,
        history: &[HistoryExecution],
    ) -> Result<bool> {
        self.covers_execution_history_page(owner, agent, history, true)
    }
    pub fn covers_execution_history_page(
        &self,
        owner: &str,
        agent: &str,
        history: &[HistoryExecution],
        verify_balances: bool,
    ) -> Result<bool> {
        self.owner(owner)?;
        let tx = self.db.unchecked_transaction()?;
        key(agent)?;
        for h in history {
            if h.agent_id != agent {
                return Ok(false);
            }
            let row: Option<(String,String,Option<String>,String)> = tx.query_row(
                "SELECT envelope,state,execution_id,immutable_digest FROM local_inbox WHERE id=?", [&h.dispatch_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            let Some((encoded, state, execution, immutable)) = row else {
                // Server custody permanently fences started executions. A missing
                // local historical receipt remains unmeasured, never a replay.
                continue;
            };
            let d: Dispatch = serde_json::from_str(&encoded)?;
            if immutable != h.immutable_digest
                || execution.as_deref() != Some(&h.execution_id)
                || d.binding_id != h.binding_id
                || d.agent_id != h.agent_id
                || d.event_id != h.event_id
                || d.room_id != h.room_id
                || d.requester_mxid != h.requester_mxid
                || d.thread_root != h.thread_root
                || d.binding_generation != h.binding_generation
                || d.dispatch_epoch != Some(h.dispatch_epoch)
                || d.dispatch_device_id.as_deref() != Some(&h.dispatch_device_id)
            {
                return Ok(false);
            }
            let call: Option<(String, String, u64, String, Option<String>, String)> = tx
                .query_row(
                    "SELECT scope,state,reserved,snapshots,usage,digest FROM calls WHERE id=?",
                    [&h.execution_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((scope, charge, reserved, snapshots, usage, digest)) = call else {
                // Execution custody above prevents replay. Missing accounting
                // does not become a fabricated zero-usage settlement.
                continue;
            };
            if scope != json(&d.scope())? {
                return Ok(false);
            }
            let snapshots: Vec<crate::Snapshot> = serde_json::from_str(&snapshots)?;
            if matches!(state.as_str(), "rejected" | "reply_ready" | "replied")
                && charge == "settled"
                && reserved == 0
                && snapshots.is_empty()
                && digest
                    == hash(&(
                        "no-provider-call",
                        &h.dispatch_id,
                        &h.execution_id,
                        &d.scope(),
                    ))?
                && usage.as_deref().is_some_and(|u| {
                    serde_json::from_str::<crate::Usage>(u).is_ok_and(|u| {
                        u.accounting_version == "no-provider-call-v1"
                            && u.total().is_ok_and(|v| v == 0)
                    })
                })
            {
                continue;
            }
            if digest != call_digest(&d.scope(), &h.dispatch_id, reserved)?
                || snapshots
                    .iter()
                    .map(|s| s.scope.as_str())
                    .collect::<Vec<_>>()
                    != d.scope()
                        .layers()?
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
            {
                return Ok(false);
            }
            if snapshots.len() != 3
                || (matches!(state.as_str(), "reply_ready" | "replied" | "failed")
                    && charge != "settled")
            {
                return Ok(false);
            }
            if charge == "settled" && usage.is_none() {
                return Ok(false);
            }
        }
        if !verify_balances {
            return Ok(true);
        }
        // Independently reconstruct balances from every retained call. Neither a
        // copied inbox nor a reset accounts table can pass this invariant.
        let mut expected = std::collections::BTreeMap::<(String, String), (u64, u64)>::new();
        let mut stmt = tx.prepare("SELECT reserved,snapshots,state,usage FROM calls")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (reserved, snapshots, state, usage) = row?;
            let spent = if state == "settled" {
                serde_json::from_str::<crate::Usage>(&usage.ok_or(Error::Conflict)?)?.total()?
            } else {
                0
            };
            let held = if state == "settled" { 0 } else { reserved };
            for snapshot in serde_json::from_str::<Vec<crate::Snapshot>>(&snapshots)? {
                let value = expected
                    .entry((snapshot.scope, snapshot.window))
                    .or_default();
                value.0 = value.0.checked_add(spent).ok_or(Error::Conflict)?;
                value.1 = value.1.checked_add(held).ok_or(Error::Conflict)?;
            }
        }
        for ((scope, window), (spent, held)) in expected {
            let actual: Option<(u64, u64)> = tx
                .query_row(
                    "SELECT spent,held FROM accounts WHERE scope=? AND window=?",
                    (&scope, &window),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if actual != Some((spent, held)) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Exclusive restart of ONE Agent after acquiring its server device lease.
    /// Other Agents owned by the same user may be executing concurrently.
    pub fn recover_agent_executions(&mut self, owner: &str, agent: &str) -> Result<usize> {
        self.owner(owner)?;
        key(agent)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE local_inbox SET state='unknown' WHERE state IN ('prepared','running') AND binding IN (SELECT id FROM bindings WHERE agent=?)", [agent])?;
        let bindings = tx
            .prepare("SELECT id FROM bindings WHERE agent=?")?
            .query_map([agent], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for binding in bindings {
            crate::recover_pending_calls(&tx, owner, Some(&binding))?;
        }
        compact_known_terminals(&tx)?;
        tx.commit()?;
        Ok(changed)
    }
    /// Conservatively preflight a bounded reply payload before invoking a model.
    /// This is a snapshot; the host must serialize this context and handle capacity
    /// changes/rejections without regenerating the model response.
    pub fn inbox_available_bytes(&self, owner: &str) -> Result<u64> {
        self.owner(owner)?;
        let maximum: u64 = self.db.query_row(
            "SELECT max_bytes FROM inbox_limits WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let used = retained_bytes(&self.db)?;
        Ok(maximum.saturating_sub(used))
    }
    /// Replied records remain permanent deduplication evidence. Only active
    /// records consume the count limit; compact receipts still consume bytes.
    /// Proven settled rejected/failed records are also compact terminal receipts.
    pub fn inbox_capacity(&self, owner: &str) -> Result<InboxCapacity> {
        self.owner(owner)?;
        let (active_records,replied_receipts):(u64,u64)=self.db.query_row(
            &format!("SELECT coalesce(sum({ACTIVE_RECORDS}),0),coalesce(sum(state='replied'),0) FROM local_inbox"),[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(InboxCapacity {
            active_records,
            replied_receipts,
            charged_bytes: retained_bytes(&self.db)?,
        })
    }
    pub fn inbox_by_state(&self, owner: &str, state: State, limit: usize) -> Result<Vec<Record>> {
        self.owner(owner)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("inbox query limit"));
        }
        let state = match state {
            State::Received => "received",
            State::Acknowledged => "acknowledged",
            State::Prepared => "prepared",
            State::Running => "running",
            State::ReplyReady => "reply_ready",
            State::Replied => "replied",
            State::Rejected => "rejected",
            State::Failed => "failed",
            State::Unknown => "unknown",
        };
        let mut stmt = self
            .db
            .prepare("SELECT id FROM local_inbox WHERE state=? ORDER BY received_at,id LIMIT ?")?;
        let ids = stmt
            .query_map((state, limit), |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.inbox_record(owner, &id))
            .collect()
    }
    /// Bounded work selection for one independently leased Agent; another
    /// Agent's backlog must not starve its reply reconciliation or new work.
    pub fn inbox_for_agent(
        &self,
        owner: &str,
        agent: &str,
        state: State,
        limit: usize,
    ) -> Result<Vec<Record>> {
        self.owner(owner)?;
        key(agent)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("inbox query limit"));
        }
        let encoded = serde_json::to_string(&state)?;
        let state = encoded.trim_matches('"');
        let mut stmt=self.db.prepare("SELECT i.id FROM local_inbox i JOIN bindings b ON i.binding=b.id WHERE b.agent=? AND i.state=? ORDER BY i.received_at,i.id LIMIT ?")?;
        let ids = stmt
            .query_map((agent, state, limit), |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.inbox_record(owner, &id))
            .collect()
    }
    pub fn inbox_for_binding(
        &self,
        owner: &str,
        agent: &str,
        binding: &str,
        state: State,
        limit: usize,
    ) -> Result<Vec<Record>> {
        self.owner(owner)?;
        key(agent)?;
        key(binding)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("inbox query limit"));
        }
        let encoded = serde_json::to_string(&state)?;
        let state = encoded.trim_matches('"');
        let mut stmt=self.db.prepare("SELECT i.id FROM local_inbox i JOIN bindings b ON i.binding=b.id WHERE b.agent=? AND i.binding=? AND i.state=? ORDER BY i.received_at,i.id LIMIT ?")?;
        let ids = stmt
            .query_map((agent, binding, state, limit), |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.inbox_record(owner, &id))
            .collect()
    }
    /// Circular recovery scan. A stuck historical delivery cannot pin the front
    /// of this binding or consume an unbounded retry pass before new polling.
    pub fn inbox_for_binding_after(
        &self,
        owner: &str,
        agent: &str,
        binding: &str,
        state: State,
        cursor: &str,
    ) -> Result<Vec<Record>> {
        self.owner(owner)?;
        key(agent)?;
        key(binding)?;
        let encoded = serde_json::to_string(&state)?;
        let state = encoded.trim_matches('"');
        let mut stmt=self.db.prepare("SELECT i.id FROM local_inbox i JOIN bindings b ON i.binding=b.id WHERE b.agent=? AND i.binding=? AND i.state=? ORDER BY (i.id<=?),i.id LIMIT 1")?;
        let ids = stmt
            .query_map((agent, binding, state, cursor), |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.inbox_record(owner, &id))
            .collect()
    }
    pub fn inbox_pending(&self, owner: &str, limit: usize) -> Result<Vec<Record>> {
        self.owner(owner)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("inbox query limit"));
        }
        let mut stmt=self.db.prepare("SELECT id FROM local_inbox WHERE state IN ('received','acknowledged','prepared','reply_ready','unknown') ORDER BY received_at,id LIMIT ?")?;
        let ids = stmt
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.inbox_record(owner, &id))
            .collect()
    }
}
#[cfg(test)]
mod tests;
