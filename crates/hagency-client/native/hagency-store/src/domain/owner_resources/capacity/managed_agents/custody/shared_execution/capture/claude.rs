use super::*;
use hagency_runtime::{claude::session as native, owned::OwnedClaudeSession};

impl SharedCallCustody {
    /// The model input comes from the original admitted work, not the caller.
    /// `prompt` records that exact input on the retained shared process before
    /// writing bytes, so a later source cannot attach to a different request.
    pub async fn start_claude_turn(&self, process: &mut OwnedClaudeSession) -> Result<(), Error> {
        self.check()?;
        self.check_claude_capture(process)?;
        process
            .prompt(self.model_input())
            .await
            .map_err(|_| Error::OutcomeUnknown)?;
        let expected = self
            .context_input_digest()
            .unwrap_or_else(|| self.work.body_digest.clone());
        if !self.sink.source().matches_input_digest(&expected) {
            return Err(Error::OutcomeUnknown);
        }
        Ok(())
    }
    fn check_claude_capture(&self, process: &OwnedClaudeSession) -> Result<(), Error> {
        if self.sink.original().runtime()? != "claude"
            || self.call.scope.registry.closed.load(Ordering::Acquire)
            || !self.sink.source().has_live_activity()
            || !self.sink.source().same(
                &process
                    .shared_process_source()
                    .map_err(|_| Error::LocalAuthority)?,
            )
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    fn claude_source(&self, process: &OwnedClaudeSession) -> Result<Source, Error> {
        self.check_claude_capture(process)?;
        let source = process
            .observation_source()
            .map_err(|_| Error::LocalAuthority)?;
        let expected = self
            .context_input_digest()
            .unwrap_or_else(|| self.work.body_digest.clone());
        if source.is_retired()
            || !process.matches_observation_source(&source)
            || !process.shared_call_matches(self.sink.source(), &expected)
        {
            return Err(Error::LocalAuthority);
        }
        Ok(Source::Claude(source))
    }
    pub async fn bind_started_claude_turn(
        self,
        process: &OwnedClaudeSession,
    ) -> Result<SharedCallSink, Error> {
        match self.claude_source(process) {
            Ok(source) => Ok(self.capture(source)),
            Err(error) => {
                self.finish_failed_start().await?;
                Err(error)
            }
        }
    }
}

impl SharedCallSink {
    /// Preserve a completed native answer when a later metadata query fails.
    /// This receipt cannot be constructed from a result alone or transferred
    /// to another process/turn. Unknown usage never establishes a checkpoint.
    pub async fn finish_claude_unaccounted(
        &mut self,
        receipt: &native::CompletionReceipt,
        text: &str,
    ) -> Result<SharedWorkReceipt, Error> {
        if !matches!(&self.source, Source::Claude(source) if source == receipt.source())
            || !receipt.matches_text(text)
            || receipt.sequence() > self.sequence
            || !self.clean
            || self.claude_result != Some(receipt.is_error())
            || self.terminal.is_some()
        {
            return Err(Error::LocalAuthority);
        }
        self.claude_unaccounted = true;
        self.terminal = Some(if receipt.interrupted() {
            TurnResult::Interrupted
        } else if receipt.is_error() {
            TurnResult::Failed
        } else {
            TurnResult::Completed
        });
        self.append_text(text);
        self.persist(false).await
    }
    /// Feed every original native observation before deferring host callbacks.
    /// A Claude result can precede background work; it never closes this sink.
    pub async fn observe_claude(
        &mut self,
        event: &native::Observation,
    ) -> Result<Option<SharedWorkReceipt>, Error> {
        if !matches!(&self.source, Source::Claude(source) if event.source() == source) {
            return Err(Error::LocalAuthority);
        }
        if self.source.is_retired()
            || self.sequence.checked_add(1) != Some(event.sequence())
            || self.terminal.is_some()
        {
            self.clean = false;
            return Err(Error::Conflict);
        }
        self.sequence = event.sequence();
        if let Some(is_error) = event.result_error()
            && !event.is_usage_snapshot()
        {
            self.claude_result = Some(is_error);
        }
        match event.kind() {
            native::ObservationKind::Ignored => return Ok(None),
            // Numeric evidence can be unusable while result/idle remains
            // authentic. Keep the answer and any earlier known lower bound.
            native::ObservationKind::Invalidated => self.claude_unaccounted = true,
            native::ObservationKind::Usage(evidence) => {
                self.usage = Some(Usage::Claude(evidence.clone()))
            }
            native::ObservationKind::Result { usage, .. } => {
                self.usage = Some(Usage::Claude(usage.clone()));
            }
        }
        self.persist(false).await.map(Some)
    }
    /// Finalization requires the original live process to have reached native
    /// idle and completed any required background-usage query. Caller-provided
    /// numeric counters, a result frame, or process exit cannot prove this.
    pub async fn finish_claude(
        &mut self,
        process: &OwnedClaudeSession,
        text: &str,
    ) -> Result<SharedWorkReceipt, Error> {
        let expected = self
            .custody
            .context_input_digest()
            .unwrap_or_else(|| self.custody.work.body_digest.clone());
        if !matches!(&self.source, Source::Claude(source) if process.matches_observation_source(source))
            || !process.shared_call_matches(self.custody.sink.source(), &expected)
            || process.phase() != native::Phase::ResultObserved
            || process.needs_usage_snapshot()
            || self.claude_result.is_none()
            || self.terminal.is_some()
        {
            return Err(Error::LocalAuthority);
        }
        self.terminal = Some(
            if process
                .interrupt_receipt()
                .is_some_and(|receipt| receipt.accepted())
            {
                TurnResult::Interrupted
            } else if self.claude_result == Some(true) {
                TurnResult::Failed
            } else {
                TurnResult::Completed
            },
        );
        self.append_text(text);
        self.persist(false).await
    }
}
