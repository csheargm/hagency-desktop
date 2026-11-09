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
/// Native plaintext records reserve space for the exact current input.
pub(super) fn queue_weight(input: Option<&SharedPreparedInput>) -> u32 {
    match input {
        Some(i) if i.context.is_some() => 192 * 1024,
        Some(_) => 128 * 1024,
        None => 32 * 1024,
    }
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

/// Call admission always requires locally pinned current input. Historical
/// work without that input remains readable but cannot issue a new model call.
pub(super) fn require_input(
    db: &Connection,
    w: &SharedWorkItem,
    input: Option<&SharedPreparedInput>,
) -> Result<(), Error> {
    let input = input.ok_or(Error::LocalAuthority)?;
    let m = mode(db, &w.id)?;
    if !cfg!(feature = "shared-context")
        || input.work_id != w.id
        || input.work_digest != context_digest(w)?
    {
        return Err(Error::LocalAuthority);
    }
    match &input.context {
        None => {
            if !w.encrypted {
                return Err(Error::LocalAuthority);
            }
            let saved:Option<(String,String,String)>=db.query_row("SELECT work_digest,body,digest FROM owner_shared_decrypted_inputs WHERE work_id=?1",[&w.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if saved
                != Some((
                    input.work_digest.clone(),
                    input.text.clone(),
                    input.input_digest(),
                ))
            {
                return Err(Error::Conflict);
            }
            if m.as_deref() != Some("plain_text_v1")
                || !input.decrypted
                || input.text.is_empty()
                || input.text.len() > 64 * 1024
            {
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

#[cfg(test)]
mod input_boundary_tests {
    use super::*;

    #[test]
    fn missing_input_never_admits_or_pins_a_call_even_for_unmarked_plain_work() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("management-shared-native-context-v3.json")).unwrap();
        let mut work: SharedWorkItem = serde_json::from_value(fixture["work"].clone()).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE owner_shared_input_modes(work_id TEXT PRIMARY KEY, mode TEXT); CREATE TABLE evidence(payload TEXT); INSERT INTO evidence VALUES('unsettled usage is retained');").unwrap();
        for encrypted in [false, true] {
            work.encrypted = encrypted;
            for mode in [None, Some("plain_text_v1"), Some("thread_context_v1")] {
                db.execute("DELETE FROM owner_shared_input_modes", [])
                    .unwrap();
                if let Some(mode) = mode {
                    db.execute(
                        "INSERT INTO owner_shared_input_modes VALUES(?1,?2)",
                        params![work.id, mode],
                    )
                    .unwrap();
                }
                let changes = db.total_changes();
                assert!(matches!(
                    require_input(&db, &work, None),
                    Err(Error::LocalAuthority)
                ));
                assert!(matches!(
                    pin_call(&db, &work, "new-call", None),
                    Err(Error::LocalAuthority)
                ));
                assert!(matches!(
                    require_call(&db, &work, "old-call", None),
                    Err(Error::LocalAuthority)
                ));
                assert_eq!(
                    db.total_changes(),
                    changes,
                    "refusal cannot rewrite historical evidence"
                );
                assert_eq!(
                    db.query_row("SELECT payload FROM evidence", [], |row| row
                        .get::<_, String>(0))
                        .unwrap(),
                    "unsettled usage is retained"
                );
            }
        }
    }

    #[test]
    fn prepared_plaintext_without_v3_cannot_use_the_encrypted_input_path() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("management-shared-native-context-v3.json")).unwrap();
        let work: SharedWorkItem = serde_json::from_value(fixture["work"].clone()).unwrap();
        assert!(!work.encrypted);
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE owner_shared_input_modes(work_id TEXT PRIMARY KEY, mode TEXT)",
        )
        .unwrap();
        db.execute(
            "INSERT INTO owner_shared_input_modes VALUES(?1,'plain_text_v1')",
            [&work.id],
        )
        .unwrap();
        let input = SharedPreparedInput {
            work_id: work.id.clone(),
            work_digest: context_digest(&work).unwrap(),
            context: None,
            text: work.body.clone(),
            decrypted: false,
        };
        assert!(matches!(
            require_input(&db, &work, Some(&input)),
            Err(Error::LocalAuthority)
        ));
    }
}

impl OwnerResourceRegistry {
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
        self.prepare_shared_input_inner(a, w, m, c, None).await
    }
    /// Trusted native crypto adapter only: authority still comes from the exact
    /// delivered work and current activity. Decrypted text never enters a wire type.
    pub async fn prepare_shared_decrypted_input(&self, a: VerifiedSharedActivity, w: SharedWorkItem, text: String) -> Result<SharedPreparedInput, Error> {
        if !w.encrypted || text.is_empty() || text.len()>64*1024 { return Err(Error::LocalAuthority); }
        self.prepare_shared_input_inner(a,w,SharedWorkInputMode{mode:"plain_text_v1".into(),context_digest:None,input_digest:None},None,Some(text)).await
    }
    async fn prepare_shared_input_inner(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        m: SharedWorkInputMode,
        c: Option<SharedContextRecord>,
        decrypted: Option<String>,
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
                if m.mode != "plain_text_v1" || !w.encrypted {
                    return Err(Error::LocalAuthority);
                }
                decrypted.clone().ok_or(Error::LocalAuthority)?
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
                        tx.execute("INSERT INTO owner_shared_decrypted_inputs VALUES(?1,?2,?3,?4,?5)",params![w.id,input.work_digest,prompt,input.text,input.input_digest()])?;
                    }
                }
                if let Some(c) = &input.context {
                    demand_binding(&tx, &w, &c.context)?;
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
