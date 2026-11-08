use super::*;
use hagency_runtime::{
    codex::session::{ObservationKind, ObservationSource, TurnResult, UsageEvidence},
    owned::{OwnedSession, SharedTextEvent},
};
/// Bounded, original-process capture. Numerical usage is never accepted without
/// an exact runtime source and contiguous accepted event sequence.
pub struct SharedCallSink {
    custody: SharedCallCustody,
    source: ObservationSource,
    sequence: u64,
    clean: bool,
    terminal: Option<TurnResult>,
    usage: Option<UsageEvidence>,
    output: String,
    output_overflow: bool,
    last_text_item: Option<String>,
}
impl SharedCallCustody {
    pub fn bind_turn(self, process: &OwnedSession) -> Result<SharedCallSink, Error> {
        let source = self.capture_source(process)?;
        Ok(self.capture(source))
    }
    /// Keep a durable call outcome even if the start acknowledgement could not
    /// be attached to an observation source. This grants no retry authority.
    pub async fn bind_started_turn(self, process: &OwnedSession) -> Result<SharedCallSink, Error> {
        match self.capture_source(process) {
            Ok(source) => Ok(self.capture(source)),
            Err(error) => {
                self.finish_failed_start().await?;
                Err(error)
            }
        }
    }
    fn capture_source(&self, process: &OwnedSession) -> Result<ObservationSource, Error> {
        self.check_capture(process)?;
        let source = process
            .observation_source()
            .map_err(|_| Error::LocalAuthority)?;
        let expected=self.context_input_digest().unwrap_or_else(||self.work.body_digest.clone());
        if !self
            .sink
            .source()
            .matches_input_digest(&expected)
            || !self.sink.source().same(&process.shared_text_process_source())
            || !process.matches_observation_source(&source)
            || source.is_retired()
        {
            return Err(Error::LocalAuthority);
        }
        Ok(source)
    }
    fn capture(self, source: ObservationSource) -> SharedCallSink {
        SharedCallSink {
            custody: self,
            source,
            sequence: 0,
            clean: true,
            terminal: None,
            usage: None,
            output: String::new(),
            output_overflow: false,
            last_text_item: None,
        }
    }
}
impl SharedCallSink {
    fn append_text(&mut self, text: &str) {
        if self.output_overflow { return; }
        let remaining=MAX_SHARED_REPLY.saturating_sub(self.output.len());
        let mut end=text.len().min(remaining);
        while !text.is_char_boundary(end) {end-=1;}
        self.output.push_str(&text[..end]);
        self.output_overflow=end<text.len();
    }
    pub fn work(&self) -> &SharedWorkItem {
        &self.custody.work
    }
    pub async fn observe(
        &mut self,
        event: SharedTextEvent,
    ) -> Result<Option<SharedWorkReceipt>, Error> {
        if !self.custody.sink.source().same(event.source())
            || event.observation().source() != &self.source
        {
            return Err(Error::LocalAuthority);
        }
        let o = event.observation();
        if o.sequence() != self.sequence.saturating_add(1) || self.terminal.is_some() {
            self.clean = false;
            return Err(Error::Conflict);
        }
        self.sequence = o.sequence();
        if let Some(text) = event.delta() {
            // Provider commentary and the final answer are distinct message
            // items. Keep their Markdown blocks separated without altering
            // incremental chunks belonging to the same item.
            if !text.is_empty() {
                if !self.output.is_empty() && self.last_text_item.as_deref()!=event.text_item_id() {
                    self.append_text("\n\n");
                }
                self.last_text_item=event.text_item_id().map(str::to_owned);
                self.append_text(text);
            }
        }
        let write = match o.kind() {
            ObservationKind::Usage(e) => {
                self.usage = Some(e.clone());
                true
            }
            ObservationKind::TurnEnded(result) => {
                self.terminal = Some(*result);
                if let Some(usage) = o.terminal_usage() {
                    self.usage = Some(usage.clone());
                }
                true
            }
            ObservationKind::Tool(_) if event.is_authorized_tool() => false,
            ObservationKind::Invalidated | ObservationKind::Tool(_) => {
                self.clean = false;
                true
            }
            ObservationKind::Ignored => false,
        };
        if write {
            self.persist(false).await.map(Some)
        } else {
            Ok(None)
        }
    }
    /// No completion/usage is invented when authority, IO or task custody ends.
    /// Previously captured positive usage remains durably chargeable after logout.
    pub async fn record_unknown(&mut self) -> Result<SharedWorkReceipt, Error> {
        self.clean = false;
        self.persist(true).await
    }
    async fn persist(&self, forced_unknown: bool) -> Result<SharedWorkReceipt, Error> {
        let c = &self.custody;
        let child = c.call.scope.child.clone();
        let call = c.call.scope.target.clone();
        let binding = c.call.scope.binding.clone();
        let issued = c.call.scope.issued_revision;
        let registry = c.call.scope.registry.clone();
        let terminal = self.terminal.is_some() || forced_unknown;
        let complete = self.clean && !self.source.is_retired() && self.terminal.is_some();
        let state = if forced_unknown || !self.clean {
            "unknown"
        } else {
            match self.terminal {
                Some(TurnResult::Completed) => "completed",
                Some(TurnResult::Failed | TurnResult::Interrupted) => {
                    "failed"
                }
                _ => "unknown",
            }
        };
        let id = format!(
            "swo_{}",
            canonical::digest(
                &json!({"workId":c.work.id,"callId":call,"sequence":self.sequence,"forcedUnknown":forced_unknown,"state":state})
            )?
        );
        let usage = ManagedUsageObservation::shared_capture(
            format!("{id}_usage"),
            self.usage.as_ref(),
            terminal,
            complete,
            false,
        );
        let body = (state == "completed").then(|| bounded_reply(&self.output,self.output_overflow));
        let work = c.work.clone();
        let process = c.process_digest()?;
        let state = state.to_owned();
        registry
            .store
            .call(64 * 1024, move |db| {
                if original_call_changed(&db.db, &child, &binding, issued, &call)?
                {
                    return Err(Error::LocalAuthority);
                }
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                if let Some(old) = tx
                    .query_row(
                        "SELECT payload FROM owner_shared_work_observations WHERE id=?1",
                        [&id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    return Ok(serde_json::from_str(&old)?);
                }
                let recorded = observations::usage(&tx, &child, &call, usage)?;
                let h = calls::history(&tx, &child, &call)?;
                let unrepresentable: bool = tx.query_row(
                    "SELECT unrepresentable_usage FROM owner_managed_calls WHERE id=?1",
                    [&call],
                    |r| r.get(0),
                )?;
                let retained = (!unrepresentable).then_some(h.known_consumed_tokens);
                let revision = revision(&tx)?;
                let mut receipt = SharedWorkReceipt {
                    version: 1,
                    observation_id: id.clone(),
                    work_id: work.id.clone(),
                    work_digest: canonical::digest(&serde_json::to_value(&work)?)?,
                    execution_id: work.execution_id.clone(),
                    call_id: call,
                    runtime_command_digest: work.runtime_command_digest.clone(),
                    process_digest: process.clone(),
                    ledger_revision: revision,
                    state,
                    known_consumed_tokens: recorded.known_consumed_tokens,
                    retained_tokens: retained,
                    usage_complete: recorded.usage_complete,
                    body_digest: body.as_deref().map(text_digest),
                    body,
                };
                // Escaping can make JSON larger than its UTF-8 body. Preserve
                // a labelled prefix and real terminal usage rather than lose
                // an otherwise completed answer.
                while serde_json::to_vec(&receipt)?.len() > 48 * 1024 {
                    let body=receipt.body.as_mut().ok_or(Error::Capacity)?;
                    let mut end=body.len()/2;
                    while !body.is_char_boundary(end) {end-=1;}
                    if end==0 {return Err(Error::Capacity)}
                    *body=bounded_reply(&body[..end],true);
                    receipt.body_digest=Some(text_digest(body));
                }
                receipt.validate_for(&work, &process)?;
                let payload = canonical::encode(&serde_json::to_value(&receipt)?)?;
                let digest = canonical::digest(&serde_json::to_value(&receipt)?)?;
                bounded_row(&tx, "owner_shared_work_observations", "id", &id, 1_000_000)?;
                tx.execute(
                    "INSERT INTO owner_shared_work_observations VALUES(?1,?2,?3,?4,?5)",
                    params![id, work.id, payload, digest, revision],
                )?;
                tx.commit()?;
                Ok(receipt)
            })
            .await
    }
}
fn text_digest(v: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(v.as_bytes()))
}
fn original_call_changed(
    db: &Connection,
    child: &str,
    binding: &str,
    issued: u64,
    call: &str,
) -> Result<bool, Error> {
    let stored:Option<(String,u64)>=db.query_row("SELECT p.assignment_digest,p.issue_revision FROM owner_managed_call_pins p JOIN owner_managed_calls c ON c.id=p.call_id WHERE p.call_id=?1 AND c.child_id=?2",params![call,child],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    Ok(stored.is_none_or(|(b, i)| b != binding || i != issued))
}

const TRUNCATED_REPLY: &str = "\n\n[答复超过单条消息长度限制，以上内容已截断。]";
fn bounded_reply(text:&str,truncated:bool)->String {
    if !truncated {return text.to_owned()}
    let mut end=text.len().min(MAX_SHARED_REPLY-TRUNCATED_REPLY.len());
    while !text.is_char_boundary(end) {end-=1;}
    text[..end].to_owned()+TRUNCATED_REPLY
}
