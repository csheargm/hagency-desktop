//! Capture only the original isolated Octos process and its admitted input.
use super::*;
use hagency_runtime::{
    octos::{Outcome, session as native},
    owned::OwnedOctosSession,
};

impl SharedCallCustody {
    fn check_octos_capture(&self, process: &OwnedOctosSession) -> Result<(), Error> {
        if self.sink.original().runtime()? != "octos"
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

    /// The caller supplies only a protocol turn ID; the input comes from the
    /// immutable admitted work. Any failed write remains outcome-unknown.
    pub async fn start_octos_turn(
        &self,
        process: &mut OwnedOctosSession,
        turn_id: &str,
    ) -> Result<(), Error> {
        self.check()?;
        self.check_octos_capture(process)?;
        process
            .start_turn(turn_id, self.model_input())
            .await
            .map_err(|_| Error::OutcomeUnknown)?;
        let expected = self
            .context_input_digest()
            .unwrap_or_else(|| self.work.body_digest.clone());
        if !process.shared_call_matches(self.sink.source(), &expected) {
            return Err(Error::OutcomeUnknown);
        }
        Ok(())
    }

    fn octos_source(&self, process: &OwnedOctosSession) -> Result<Source, Error> {
        self.check_octos_capture(process)?;
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
        Ok(Source::Octos(source))
    }

    pub async fn bind_started_octos_turn(
        self,
        process: &OwnedOctosSession,
    ) -> Result<SharedCallSink, Error> {
        match self.octos_source(process) {
            Ok(source) => Ok(self.capture(source)),
            Err(error) => {
                self.finish_failed_start().await?;
                Err(error)
            }
        }
    }
}

impl SharedCallSink {
    /// Capture every native observation in order, including background turns.
    /// A missing counter remains unknown; an invalidated stream cannot settle.
    pub async fn observe_octos(
        &mut self,
        event: &native::Observation,
    ) -> Result<Option<SharedWorkReceipt>, Error> {
        if !matches!(&self.source, Source::Octos(source) if event.source() == source) {
            return Err(Error::LocalAuthority);
        }
        if self.source.is_retired()
            || self.sequence.checked_add(1) != Some(event.sequence())
            || self.terminal.is_some()
            || self.octos_idle
        {
            self.clean = false;
            return Err(Error::Conflict);
        }
        self.sequence = event.sequence();
        match event.kind() {
            native::ObservationKind::Ignored => return Ok(None),
            native::ObservationKind::Invalidated => self.usage_unaccounted = true,
            native::ObservationKind::Usage(evidence) => {
                self.usage = Some(Usage::Octos(evidence.clone()))
            }
            native::ObservationKind::Idle(evidence) => {
                self.usage = Some(Usage::Octos(evidence.clone()));
                self.octos_idle = true;
            }
        }
        self.persist(false).await.map(Some)
    }

    /// Only the original process's idle receipt proves this answer. It cannot
    /// be completed with caller-provided text or a different process's event.
    pub async fn finish_octos(
        &mut self,
        process: &OwnedOctosSession,
        text: &str,
    ) -> Result<SharedWorkReceipt, Error> {
        let expected = self
            .custody
            .context_input_digest()
            .unwrap_or_else(|| self.custody.work.body_digest.clone());
        let receipt = process.completion_receipt().ok_or(Error::LocalAuthority)?;
        if !matches!(&self.source, Source::Octos(source) if process.matches_observation_source(source) && receipt.source() == source)
            || !process.shared_call_matches(self.custody.sink.source(), &expected)
            || !(self.octos_idle || self.usage_unaccounted)
            || receipt.sequence() != self.sequence
            || !receipt.matches_text(text)
            || self.terminal.is_some()
        {
            return Err(Error::LocalAuthority);
        }
        self.terminal = Some(match receipt.outcome() {
            Outcome::Completed => TurnResult::Completed,
            Outcome::Interrupted => TurnResult::Interrupted,
            Outcome::Errored | Outcome::RateLimited => TurnResult::Failed,
        });
        self.append_text(text);
        self.persist(false).await
    }
}
