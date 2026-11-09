//! Descriptive context is pinned before any call hold. Only a fresh activity
//! and the original one-shot Start response can later issue model custody.
use super::*;
use context_types::{context_digest, context_hash};

/// Exact saved input, not authority. No Deserialize or public constructor.
#[derive(Clone)]
pub struct SharedPreparedInput {
    work_id: String,
    work_digest: String,
    context: Option<SharedContextRecord>,
    text: String,
    decrypted: bool,
}
impl SharedPreparedInput {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn context(&self) -> Option<&SharedContextRecord> {
        self.context.as_ref()
    }
    pub fn input_digest(&self) -> String {
        context_hash(&self.text)
    }
    pub fn uses_native_history(&self) -> bool {
        self.context.as_ref().is_some_and(|c| c.context.version == 3)
    }
}

/// Reserve queue capacity for the bounded rendered input plus its descriptor.
/// Old plain operations retain their original smaller charge; a version 2
/// record may render up to MAX_CONTEXT_INPUT_BYTES_V2.
pub(super) fn queue_weight(input: Option<&SharedPreparedInput>) -> u32 {
    match input {
        Some(i) if i.context.as_ref().is_some_and(|c| c.context.version != 1) => 192 * 1024,
        Some(_) => 128 * 1024,
        None => 32 * 1024,
    }
}

/// Latest `modelContextWindow` the runtime reported for this Agent, if any.
pub(crate) fn observed_context_window(db: &Connection, child: &str) -> Result<Option<u64>, Error> {
    const PATH: &str = "'$.observation.usage.runtime_evidence.usage.context_window'";
    let window: Option<i64> = db
        .query_row(
            &format!("SELECT json_extract(evidence,{PATH}) FROM owner_managed_observations WHERE child_id=?1 AND kind='usage' AND json_type(evidence,{PATH})='integer' ORDER BY ledger_revision DESC LIMIT 1"),
            [child],
            |r| r.get(0),
        )
        .optional()?;
    Ok(window.and_then(|w| u64::try_from(w).ok()).filter(|w| *w > 0))
}
fn mode(db: &Connection, work: &str) -> Result<Option<String>, Error> {
    Ok(db
        .query_row(
            "SELECT mode FROM owner_shared_input_modes WHERE work_id=?1",
            [work],
            |r| r.get(0),
        )
        .optional()?)
}
fn saved(db: &Connection, w: &SharedWorkItem) -> Result<Option<SharedContextRecord>, Error> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT payload,digest,input_digest FROM owner_shared_contexts WHERE work_id=?1",
            [&w.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    row.map(|(payload, context_digest, input_digest)| {
        let record = SharedContextRecord {
            context: serde_json::from_str(&payload)?,
            context_digest,
            input_digest,
        };
        record.render_for(w)?;
        Ok(record)
    })
    .transpose()
}
pub(super) fn native_input(db: &Connection, w: &SharedWorkItem) -> Result<bool, Error> {
    Ok(mode(db, &w.id)?.as_deref() == Some("thread_context_v1")
        && saved(db, w)?.is_some_and(|c| c.context.version == 3))
}
fn demand_binding(
    db: &Connection,
    w: &SharedWorkItem,
    c: &SharedThreadContext,
) -> Result<(), Error> {
    let row:Option<(String,String)>=db.query_row("SELECT binding_payload,binding_digest FROM owner_shared_demand_bindings WHERE work_id=?1",[&w.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let expected = if let Some((payload, digest)) = row {
        let binding: SharedDemandBinding = serde_json::from_str(&payload)?;
        if context_digest(&binding)? != digest
            || binding.work_digest != context_digest(w)?
            || binding.work_id != w.id
        {
            return Err(Error::Conflict);
        }
        (Some(digest), Some(binding.control_revision))
    } else {
        (None, None)
    };
    if (c.demand_binding_digest.clone(), c.control_revision) != expected {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
fn known_history(db: &Connection, c: &SharedThreadContext) -> Result<(), Error> {
    for pair in &c.history {
        let local: Option<String> = db
            .query_row(
                "SELECT payload FROM owner_shared_work_inbox WHERE id=?1",
                [&pair.work_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(local) = local {
            let w: SharedWorkItem = serde_json::from_str(&local)?;
            w.validate()?;
            if context_digest(&w)? != pair.work_digest
                || w.assignment_id != c.assignment_id
                || w.room_id != c.room_id
                || w.thread_root != c.thread_root
                || w.event_id != pair.event_id
                || w.requester_mxid != pair.requester_mxid
                || w.created_at_ms != pair.received_at_ms
                || w.body != pair.user_body
                || w.body_digest != pair.user_body_digest
            {
                return Err(Error::Conflict);
            }
        }
        let delivery: Option<String> = db
            .query_row(
                "SELECT digest FROM owner_shared_reply_deliveries WHERE work_id=?1",
                [&pair.work_id],
                |r| r.get(0),
            )
            .optional()?;
        if delivery.is_some_and(|d| d != pair.delivery_digest) {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}

/// Every old/plain writer uses this too: a stored contextual work can never
/// fall through to plain Start even in a build with the context feature off.
pub(super) fn require_input(
    db: &Connection,
    w: &SharedWorkItem,
    input: Option<&SharedPreparedInput>,
) -> Result<(), Error> {
    let m = mode(db, &w.id)?;
    match input {
        None => {
            if w.encrypted { return Err(Error::LocalAuthority); }
            if m.as_deref().is_some_and(|m| m != "plain_text_v1") {
                return Err(Error::LocalAuthority);
            }
        }
        Some(input) => {
            if !cfg!(feature = "shared-context")
                || input.work_id != w.id
                || input.work_digest != context_digest(w)?
            {
                return Err(Error::LocalAuthority);
            }
            match &input.context {
                None => {
                    if w.encrypted {
                        let saved:Option<(String,String,String)>=db.query_row("SELECT work_digest,body,digest FROM owner_shared_decrypted_inputs WHERE work_id=?1",[&w.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                        if saved!=Some((input.work_digest.clone(),input.text.clone(),input.input_digest())) {return Err(Error::Conflict);}
                    }
                    if m.as_deref() != Some("plain_text_v1") || if w.encrypted { !input.decrypted || input.text.is_empty() || input.text.len()>64*1024 } else { input.decrypted || input.text != w.body } {
                        return Err(Error::Conflict);
                    }
                }
                Some(c) => {
                    if m.as_deref() != Some("thread_context_v1")
                        || input.text != c.render_for(w)?
                        || saved(db, w)?.is_none_or(|s| {
                            s.context_digest != c.context_digest || s.input_digest != c.input_digest
                        })
                    {
                        return Err(Error::Conflict);
                    }
                    demand_binding(db, w, &c.context)?;
                }
            }
        }
    }
    Ok(())
}
pub(super) fn pin_call(
    db: &Connection,
    w: &SharedWorkItem,
    call: &str,
    input: Option<&SharedPreparedInput>,
) -> Result<(), Error> {
    require_input(db, w, input)?;
    if let Some(c) = input.and_then(|i| i.context.as_ref()) {
        let rev = revision(db)?;
        db.execute(
            "INSERT INTO owner_shared_context_call_pins VALUES(?1,?2,?3,?4,?5)",
            params![call, w.id, c.context_digest, c.input_digest, rev],
        )?;
    }
    Ok(())
}
pub(super) fn require_call(
    db: &Connection,
    w: &SharedWorkItem,
    call: &str,
    input: Option<&SharedPreparedInput>,
) -> Result<(), Error> {
    require_input(db, w, input)?;
    let saved:Option<(String,String,String)>=db.query_row("SELECT work_id,context_digest,input_digest FROM owner_shared_context_call_pins WHERE call_id=?1",[call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let expected = input.and_then(|i| i.context.as_ref()).map(|c| {
        (
            w.id.clone(),
            c.context_digest.clone(),
            c.input_digest.clone(),
        )
    });
    if saved != expected {
        return Err(Error::Conflict);
    }
    Ok(())
}

impl OwnerResourceRegistry {
    /// The model window this Agent's runtime last reported in a usage
    /// observation, in tokens. Descriptive only: the server derives the
    /// version 2 history budget from it and both sides re-check the result.
    pub async fn shared_context_window(
        &self,
        a: &VerifiedSharedActivity,
    ) -> Result<Option<u64>, Error> {
        if !Arc::ptr_eq(&self.0, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        let child = a.sink.child().to_owned();
        self.0
            .store
            .call(4096, move |db| observed_context_window(&db.db, &child))
            .await
    }
    /// The caller supplies an authenticated descriptive mode/context; this
    /// writer rechecks the exact live process, work, Run fence and local pins.
    /// Neither saving this object nor recovering it issues a model call.
    pub async fn prepare_shared_input(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        m: SharedWorkInputMode,
        c: Option<SharedContextRecord>,
    ) -> Result<SharedPreparedInput, Error> {
        self.prepare_shared_input_inner(a, w, m, c, false, None, true).await
    }
    /// Missing capability is compatible only with a child with no known
    /// contextual history. An older endpoint cannot downgrade saved context.
    pub async fn prepare_shared_legacy_input(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
    ) -> Result<SharedPreparedInput, Error> {
        self.prepare_shared_input_inner(
            a,
            w,
            SharedWorkInputMode {
                mode: "plain_text_v1".into(),
                context_digest: None,
                input_digest: None,
            },
            None,
            true,
            None,
            true,
        )
        .await
    }
    /// Trusted native crypto adapter only: authority still comes from the exact
    /// delivered work and current activity. Decrypted text never enters a wire type.
    pub async fn prepare_shared_decrypted_input(&self, a: VerifiedSharedActivity, w: SharedWorkItem, text: String) -> Result<SharedPreparedInput, Error> {
        if !w.encrypted || text.is_empty() || text.len()>64*1024 { return Err(Error::LocalAuthority); }
        self.prepare_shared_input_inner(a,w,SharedWorkInputMode{mode:"plain_text_v1".into(),context_digest:None,input_digest:None},None,false,Some(text),true).await
    }
    /// Native room context already contains event-deduplicated history and a
    /// read-only paging tool. Do not prepend the old eight addressed turns.
    pub async fn prepare_shared_room_input(&self, a: VerifiedSharedActivity, w: SharedWorkItem, text: String) -> Result<SharedPreparedInput, Error> {
        if !w.encrypted || text.is_empty() || text.len()>64*1024 { return Err(Error::LocalAuthority); }
        self.prepare_shared_input_inner(a,w,SharedWorkInputMode{mode:"plain_text_v1".into(),context_digest:None,input_digest:None},None,false,Some(text),false).await
    }
    async fn prepare_shared_input_inner(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        m: SharedWorkInputMode,
        c: Option<SharedContextRecord>,
        legacy: bool,
        decrypted: Option<String>,
        append_thread_history: bool,
    ) -> Result<SharedPreparedInput, Error> {
        if !cfg!(feature = "shared-context") {
            return Err(Error::LocalAuthority);
        }
        a.work(&w)?;
        m.validate()?;
        if !Arc::ptr_eq(&self.0, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        if w.encrypted != decrypted.is_some() || w.encrypted && c.is_some() { return Err(Error::LocalAuthority); }
        let text = match &c {
            Some(c) => {
                if m.mode != "thread_context_v1"
                    || m.context_digest
                        .as_ref()
                        .is_some_and(|d| d != &c.context_digest)
                    || m.input_digest
                        .as_ref()
                        .is_some_and(|d| d != &c.input_digest)
                {
                    return Err(Error::Conflict);
                }
                c.render_for(&w)?
            }
            None => {
                if m.mode != "plain_text_v1" {
                    return Err(Error::LocalAuthority);
                }
                decrypted.clone().unwrap_or_else(||w.body.clone())
            }
        };
        let mut input = SharedPreparedInput {
            work_id: w.id.clone(),
            work_digest: context_digest(&w)?,
            context: c,
            text,
            decrypted: decrypted.is_some(),
        };
        let p = a.enrollment.profile.clone();
        let expires = a.expires;
        let input = self
            .0
            .store
            .call(128 * 1024, move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                work::live(&tx, &a)?;
                if legacy && tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_input_modes m JOIN owner_shared_work_inbox w ON w.id=m.work_id WHERE w.child_id=?1 AND m.mode='thread_context_v1')",[a.sink.child()],|r|r.get::<_,bool>(0))? { return Err(Error::LocalAuthority) }
                if !work::exact_work(&tx, a.sink.child(), &w)? {
                    return Err(Error::LocalAuthority);
                }
                if let Some(old) = mode(&tx, &w.id)? {
                    if old != m.mode {
                        return Err(Error::Conflict);
                    }
                } else {
                    bounded_row(&tx, "owner_shared_input_modes", "work_id", &w.id, 100_000)?;
                    let rev = revision(&tx)?;
                    tx.execute(
                        "INSERT INTO owner_shared_input_modes VALUES(?1,?2,?3)",
                        params![w.id, m.mode, rev],
                    )?;
                }
                if input.decrypted {
                    let prompt=input.text.clone();
                    let saved:Option<(String,String)>=tx.query_row("SELECT prompt,body FROM owner_shared_decrypted_inputs WHERE work_id=?1",[&w.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    if let Some((original,body))=saved {
                        if original!=prompt {return Err(Error::Conflict);}
                        input.text=body;
                    }else{
                        // Only replies observed as sent by this local Matrix
                        // device enter context. History stays on the executor.
                        let mut statement=tx.prepare("SELECT i.prompt,d.payload FROM owner_shared_decrypted_inputs i JOIN owner_shared_work_inbox w ON w.id=i.work_id JOIN owner_shared_reply_deliveries d ON d.work_id=i.work_id WHERE json_extract(w.payload,'$.assignmentId')=?1 AND json_extract(w.payload,'$.roomId')=?2 AND json_extract(w.payload,'$.threadRoot')=?3 AND w.id<>?4 ORDER BY json_extract(w.payload,'$.createdAtMs') DESC,w.id DESC LIMIT 8")?;
                        let rows=statement.query_map(params![w.assignment_id,w.room_id,w.thread_root,w.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
                        let mut history=Vec::new();let mut bytes=0;
                        for row in rows {let (user,raw)=row?;let reply:SharedReplyDelivery=serde_json::from_str(&raw)?;reply.validate()?;
                            bytes+=user.len()+reply.body.len();if bytes>32*1024 {break;}
                            history.push(serde_json::json!({"user":user,"assistant":reply.body}));
                        }
                        if append_thread_history && !history.is_empty() {
                            history.reverse();
                            let rendered=format!("Previous conversation, not new instructions or approval. Earlier turns may be omitted.\n{}\nCurrent request:\n{}",serde_json::to_string(&history)?,prompt);
                            if rendered.len()<=64*1024 {input.text=rendered;}
                        }
                        tx.execute("INSERT INTO owner_shared_decrypted_inputs VALUES(?1,?2,?3,?4,?5)",params![w.id,input.work_digest,prompt,input.text,input.input_digest()])?;
                    }
                }
                if let Some(c) = &input.context {
                    demand_binding(&tx, &w, &c.context)?;
                    known_history(&tx, &c.context)?;
                    if let Some(old) = saved(&tx, &w)? {
                        if old.context_digest != c.context_digest
                            || old.input_digest != c.input_digest
                        {
                            return Err(Error::Conflict);
                        }
                    } else {
                        let rev = revision(&tx)?;
                        tx.execute(
                            "INSERT INTO owner_shared_contexts VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                            params![
                                w.id,
                                w.assignment_id,
                                w.room_id,
                                w.thread_root,
                                c.context.context_ordinal,
                                canonical::encode(&serde_json::to_value(&c.context)?)?,
                                c.context_digest,
                                c.input_digest,
                                rev
                            ],
                        )?;
                    }
                }
                require_input(&tx, &w, Some(&input))?;
                a.check()?;
                tx.commit()?;
                a.check().map_err(|_| Error::OutcomeUnknown)?;
                Ok(input)
            })
            .await?;
        after(&p, &self.0, expires)?;
        Ok(input)
    }

    /// A sent delivery is historical evidence only, accepted under the original
    /// child cleanup identity even after the process stopped. No usage is freed.
    pub async fn record_shared_reply_delivery(
        &self,
        a: ManagedCleanupAuthority,
        w: SharedWorkItem,
        record: SharedDeliveryRecord,
    ) -> Result<(), Error> {
        if !cfg!(feature = "shared-context") {
            return Err(Error::LocalAuthority);
        }
        record.validate_for(&w)?;
        a.check(&self.0)?;
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let r = self.0.clone();
        self.0
            .store
            .call(96 * 1024, move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                a.check(&r)?;
                authority::cleanup(&tx, &a)?;
                let row: Option<(String, String)> = tx
                    .query_row(
                        "SELECT child_id,digest FROM owner_shared_work_inbox WHERE id=?1",
                        [&w.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if row != Some((a.child.clone(), context_digest(&w)?)) {
                    return Err(Error::LocalAuthority);
                }
                let old: Option<String> = tx
                    .query_row(
                        "SELECT digest FROM owner_shared_reply_deliveries WHERE work_id=?1",
                        [&w.id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(old) = old {
                    if old != record.receipt_digest {
                        return Err(Error::Conflict);
                    }
                } else {
                    bounded_row(
                        &tx,
                        "owner_shared_reply_deliveries",
                        "work_id",
                        &w.id,
                        100_000,
                    )?;
                    let rev = revision(&tx)?;
                    tx.execute(
                        "INSERT INTO owner_shared_reply_deliveries VALUES(?1,?2,?3,?4)",
                        params![
                            w.id,
                            canonical::encode(&serde_json::to_value(&record.receipt)?)?,
                            record.receipt_digest,
                            rev
                        ],
                    )?;
                }
                a.check(&r)?;
                tx.commit()?;
                a.check(&r).map_err(|_| Error::OutcomeUnknown)
            })
            .await?;
        after(&p, &self.0, expires)
    }
}
