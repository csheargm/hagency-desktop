//! Durable native context is descriptive. Every follow-up still needs a new
//! original work, process lease, call admission and original usage capture.
use super::*;
use hagency_runtime::{
    codex::session::{Phase, ResumeThreadId},
    owned::SharedTextProcessSource,
};

pub struct SharedNativeSession {
    work: SharedWorkItem,
    source: SharedTextProcessSource,
    process_digest: String,
    previous: Option<String>,
}
impl SharedNativeSession {
    pub fn is_resuming(&self) -> bool {
        self.previous.is_some()
    }
    pub async fn open_codex(&mut self, process: &mut OwnedSession) -> Result<(), Error> {
        if !self.source.same(&process.shared_text_process_source())
            || !self.source.has_live_activity()
        {
            return Err(Error::LocalAuthority);
        }
        let resume = self
            .previous
            .clone()
            .map(ResumeThreadId::new)
            .transpose()
            .map_err(|_| Error::Conflict)?;
        let (_, resumed) = process
            .open_shared_text_thread_for_room(&self.work.room_id, resume)
            .await
            .map_err(|_| Error::OutcomeUnknown)?;
        if !resumed {
            self.previous = None;
        }
        Ok(())
    }
}

fn scope_digest(
    a: &VerifiedSharedActivity,
    w: &SharedWorkItem,
    settings: &str,
) -> Result<String, Error> {
    let root = a.sink.original();
    let policy = match &a.sink {
        ProcessBinding::Original(_) => root.execution_policy(false, None)?,
        ProcessBinding::Attempt(sink) => root.execution_policy(
            sink.command().web_search,
            sink.command().tool_policy.as_ref().map(|p| p.mode.as_str()),
        )?,
        #[cfg(feature = "shared-budget-execution")]
        ProcessBinding::Budget(sink) => root.execution_policy(
            sink.command().web_search,
            sink.command().tool_policy.as_ref().map(|p| p.mode.as_str()),
        )?,
    };
    // Ordinary room messages share one conversation per requester. An explicit
    // Matrix thread has its own native session. No member can select another
    // requester's context merely by replying in the same room/thread.
    let thread = (w.thread_root != w.event_id).then_some(&w.thread_root);
    Ok(canonical::digest(&json!({
        "version":1,"runtime":root.runtime()?,"profile":a.enrollment.profile.digest,
        "executor":a.enrollment.executor_device_id,"generation":a.enrollment.executor_generation,
        "provider":a.sink.provider(),"resource":root.reserve_command.resource_id,
        "resourceRevision":root.reserve_command.resource_revision,
        "assignment":root.assignment_id,"assignmentGeneration":root.assignment_generation,
        "settings":settings,"policy":policy,"room":w.room_id,"binding":w.room_binding,
        "requester":w.requester_mxid,"thread":thread,"encrypted":w.encrypted,
    }))?)
}

fn previous(db: &Connection, scope: &str) -> Result<Option<String>, Error> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
    );
    let row: Option<Row> = db
        .query_row(
            "SELECT c.native_id,o.payload,o.digest,t.process_digest,w.payload,c.receipt_id
         FROM owner_shared_native_turns t
         JOIN owner_shared_work_inbox w ON w.id=t.work_id
         LEFT JOIN owner_shared_native_checkpoints c ON c.work_id=t.work_id
         LEFT JOIN owner_shared_work_observations o ON o.id=c.receipt_id
         WHERE t.scope_digest=?1 ORDER BY t.opened_revision DESC LIMIT 1",
            [scope],
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
    let Some((Some(native), Some(payload), Some(digest), process, work, Some(id))) = row else {
        return Ok(None);
    };
    let receipt: SharedWorkReceipt = serde_json::from_str(&payload)?;
    let work: SharedWorkItem = serde_json::from_str(&work)?;
    receipt.validate_for(&work, &process)?;
    if canonical::digest(&serde_json::to_value(&receipt)?)? != digest {
        return Err(Error::Conflict);
    }
    let latest: String = db.query_row(
        "SELECT id FROM owner_shared_work_observations WHERE work_id=?1 ORDER BY ledger_revision DESC LIMIT 1",
        [&work.id],|r|r.get(0))?;
    // Completion and whole-tree stop are separate pieces of evidence. A later
    // unknown observation also prevents an older checkpoint being resurrected.
    let stopped: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_shared_process_observations WHERE json_extract(payload,'$.processDigest')=?1 AND json_extract(payload,'$.state')='stopped')
          OR EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE json_extract(payload,'$.processDigest')=?1 AND json_extract(payload,'$.state')='stopped')",
        [&process],|r|r.get(0))?;
    if latest != id || receipt.state != "completed" || !receipt.usage_complete || !stopped {
        return Ok(None);
    }
    ResumeThreadId::new(native.clone()).map_err(|_| Error::Conflict)?;
    Ok(Some(native))
}

impl OwnerResourceRegistry {
    /// Mark this exact new work as the newest context opening before any native
    /// resume or model input. A crash afterwards cannot reuse the older state.
    pub async fn prepare_shared_native_session(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        process: &mut OwnedSession,
    ) -> Result<SharedNativeSession, Error> {
        a.work(&w)?;
        let source = process.shared_text_process_source();
        if !Arc::ptr_eq(&self.0, a.sink.registry())
            || !a.sink.source().same(&source)
            || a.sink.original().runtime()? != "codex"
            || process.phase() != Phase::Ready
        {
            return Err(Error::LocalAuthority);
        }
        process
            .retain_shared_context()
            .map_err(|_| Error::LocalAuthority)?;
        let settings = process
            .shared_session_profile_digest()
            .map_err(|_| Error::LocalAuthority)?;
        let scope = scope_digest(&a, &w, &settings)?;
        let process_digest = a.sink.process_digest()?;
        let mut session = SharedNativeSession {
            work: w.clone(),
            source,
            process_digest: process_digest.clone(),
            previous: None,
        };
        session.previous = self
            .0
            .store
            .call(32 * 1024, move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                work::live(&tx, &a)?;
                if !work::exact_work(&tx, a.sink.child(), &w)? {
                    return Err(Error::LocalAuthority);
                }
                // Plaintext context V1/V2 pins a server-rendered transcript. Keep
                // those immutable input bytes until that protocol supports deltas.
                let prior = if w.encrypted || !cfg!(feature = "shared-context") {
                    previous(&tx, &scope)?
                } else {
                    None
                };
                bounded_row(
                    &tx,
                    "owner_shared_native_turns",
                    "work_id",
                    &w.id,
                    1_000_000,
                )?;
                let revision = revision(&tx)?;
                tx.execute(
                    "INSERT INTO owner_shared_native_turns VALUES(?1,?2,?3,?4)",
                    params![w.id, scope, process_digest, revision],
                )?;
                a.check()?;
                tx.commit()?;
                Ok(prior)
            })
            .await?;
        Ok(session)
    }
}

impl SharedCallSink {
    /// Only this original sink and completed native process can produce a
    /// checkpoint. Arbitrary thread IDs, text, counters and receipts cannot.
    pub async fn retain_native_session(
        &self,
        session: SharedNativeSession,
        process: &OwnedSession,
    ) -> Result<(), Error> {
        self.custody.check_capture(process)?;
        if !self.clean
            || self.terminal != Some(TurnResult::Completed)
            || process.phase() != Phase::Ended
            || !matches!(&self.source,Source::Codex(source) if process.matches_completed_observation_source(source))
            || !session.source.same(self.custody.sink.source())
            || session.work.id != self.custody.work.id
            || session.process_digest != self.custody.process_digest()?
        {
            return Err(Error::LocalAuthority);
        }
        let native = process
            .shared_thread_id()
            .ok_or(Error::LocalAuthority)?
            .to_owned();
        ResumeThreadId::new(native.clone()).map_err(|_| Error::LocalAuthority)?;
        let work = self.custody.work.clone();
        let process = session.process_digest;
        self.custody.call.scope.registry.store.call(64*1024,move |db| {
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let payload:String=tx.query_row("SELECT payload FROM owner_shared_work_observations WHERE work_id=?1 ORDER BY ledger_revision DESC LIMIT 1",[&work.id],|r|r.get(0))?;
            let receipt:SharedWorkReceipt=serde_json::from_str(&payload)?;
            receipt.validate_for(&work,&process)?;
            if receipt.state!="completed" || !receipt.usage_complete { return Ok(()); }
            let opened:String=tx.query_row("SELECT process_digest FROM owner_shared_native_turns WHERE work_id=?1",[&work.id],|r|r.get(0))?;
            if opened!=process { return Err(Error::Conflict); }
            tx.execute("INSERT INTO owner_shared_native_checkpoints VALUES(?1,?2,?3)",params![work.id,native,receipt.observation_id])?;
            tx.commit()?;
            Ok(())
        }).await
    }
}
