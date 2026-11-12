//! Native-only Start/Resume handoff. The prepared handle cannot be deserialized,
//! and the automatic worker only reads/submits existing immutable start intents.
use super::*;
use ::hagency::native_owner::{
    CurrentSharedExecutor, PreparedSharedSuccessor, SharedSuccessorReview, SharedSuccessorResume,
    SharedExecutionProgress, SharedSuccessorAssessment, SharedSuccessorBudgetReview,
    SharedSuccessorDescription,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SharedSuccessorRequest {
    pub assignment_id: String,
}
pub(crate) struct PreparedSuccessorStart {
    owner: Weak<Inner>,
    custody: SuccessorCustody,
}
enum SuccessorCustody {
    Local {executor: Arc<CurrentSharedExecutor>, prepared: Box<PreparedSharedSuccessor>},
    Remote {controller: Arc<::hagency::daemon::bridge::ControllerClient>, prepared: ::hagency::daemon::bridge::PreparedSharedSuccessor},
}
impl PreparedSuccessorStart {
    pub(crate) fn expired(&self) -> bool {
        match &self.custody { SuccessorCustody::Local {prepared,..} => prepared.expired(), SuccessorCustody::Remote {prepared,..} => prepared.expired() }
    }
    pub(crate) fn restrict_to_host(
        mut self,
        gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, String> {
        self.custody = match self.custody {
            SuccessorCustody::Local {executor,prepared} => SuccessorCustody::Local {executor, prepared:Box::new((*prepared).restrict_to_host(gate).map_err(|e|e.code)?)},
            SuccessorCustody::Remote {controller,prepared} => SuccessorCustody::Remote {controller, prepared:prepared.restrict_to_host(gate).map_err(|e|e.code)?},
        };
        Ok(self)
    }
    pub(crate) fn review(&self) -> &SharedSuccessorReview {
        match &self.custody {SuccessorCustody::Local {prepared,..}=>prepared.review(), SuccessorCustody::Remote {prepared,..}=>prepared.review()}
    }
}
pub(crate) enum SuccessorAssessment {
    Ready(PreparedSuccessorStart),
    Blocked(SharedSuccessorBudgetReview),
}
#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum SharedSuccessorResumeResult {
    NoSavedIntent,
    Resumed { progress: SharedExecutionProgress },
}
impl Service {
    fn successor_package(&self) -> ::hagency::native_owner::ManagementPackage {
        ::hagency::native_owner::ManagementPackage {
            digest:blake3::hash(b"hagency-desktop-native-shared-worker-v1").to_hex().to_string(),
            instance:1, account_epoch:self.0.epoch+1, permissions:vec![],
        }
    }
    pub(crate) async fn list_shared_successors(
        &self,
        after: Option<String>,
    ) -> Result<::hagency::native_owner::SharedSuccessorPage, String> {
        self.check()?;
        if let Some(remote)=self.remote_client().await? {
            let result=remote.list_shared_successors(self.successor_package(),after).await.map_err(|e|e.code);
            self.check()?;return result;
        }
        let executor = self.shared_successor_executor().await?;
        let result = executor.list_successors(after).await.map_err(|e| e.code);
        self.check()?;
        result
    }
    pub(crate) async fn assess_shared_successor(
        &self,
        request: SharedSuccessorRequest,
        until: std::time::Instant,
    ) -> Result<SuccessorAssessment, String> {
        self.check()?;
        if let Some(remote)=self.remote_client().await? {
            let result=remote.assess_shared_successor(self.successor_package(),request.assignment_id,until).await.map_err(|e|e.code)?;
            self.check()?;
            return Ok(match result {
                ::hagency::daemon::bridge::SuccessorAssessment::Ready(prepared)=>SuccessorAssessment::Ready(PreparedSuccessorStart {owner:Arc::downgrade(&self.0),custody:SuccessorCustody::Remote {controller:remote,prepared}}),
                ::hagency::daemon::bridge::SuccessorAssessment::Blocked(review)=>SuccessorAssessment::Blocked(review),
            });
        }
        let executor = self.shared_successor_executor().await?;
        let prepared = executor
            .assess_successor(request.assignment_id)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(match prepared {
            SharedSuccessorAssessment::Ready(prepared) => {
                SuccessorAssessment::Ready(PreparedSuccessorStart {
                    owner: Arc::downgrade(&self.0),
                    custody: SuccessorCustody::Local {executor,prepared},
                })
            }
            SharedSuccessorAssessment::Blocked(review) => SuccessorAssessment::Blocked(review),
        })
    }
    pub(crate) async fn start_shared_successor(
        &self,
        pending: PreparedSuccessorStart,
    ) -> Result<SharedExecutionProgress, String> {
        self.check()?;
        if pending
            .owner
            .upgrade()
            .is_none_or(|owner| !Arc::ptr_eq(&owner, &self.0))
        {
            return Err("shared_successor_preparation_changed".into());
        }
        let result = match pending.custody {
            SuccessorCustody::Local {executor,prepared}=>executor.start_successor(*prepared).await,
            SuccessorCustody::Remote {controller,prepared}=>controller.start_shared_successor(self.successor_package(),prepared).await,
        }.map_err(|e|e.code);
        self.check()?;
        result
    }
    pub(crate) async fn shared_successor_description(
        &self,
        request: SharedSuccessorRequest,
    ) -> Result<SharedSuccessorDescription, String> {
        self.check()?;
        if let Some(remote)=self.remote_client().await? {
            let result=remote.shared_successor_description(self.successor_package(),request.assignment_id).await.map_err(|e|e.code);
            self.check()?;return result;
        }
        let executor = self.shared_successor_executor().await?;
        let description = executor
            .successor_description(request.assignment_id)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(description)
    }
    pub(crate) async fn resume_shared_successor(
        &self,
        request: SharedSuccessorRequest,
        until: std::time::Instant,
    ) -> Result<SharedSuccessorResumeResult, String> {
        self.check()?;
        if let Some(remote)=self.remote_client().await? {
            let result=remote.resume_shared_successor(self.successor_package(),request.assignment_id,until).await.map_err(|e|e.code)?;
            self.check()?;
            return Ok(match result {Some(progress)=>SharedSuccessorResumeResult::Resumed{progress},None=>SharedSuccessorResumeResult::NoSavedIntent});
        }
        let executor = self.shared_successor_executor().await?;
        let result = executor
            .resume_successor(request.assignment_id)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(match result {
            Some(progress) => SharedSuccessorResumeResult::Resumed { progress },
            None => SharedSuccessorResumeResult::NoSavedIntent,
        })
    }
}

trait SavedSuccessorIo {
    fn resume(
        &self,
        after: Option<String>,
    ) -> impl std::future::Future<Output = Result<Option<SharedSuccessorResume>, NativeError>> + Send;
    fn poll(
        &self,
        after: Option<String>,
    ) -> impl std::future::Future<Output = Result<SharedExecutionProgress, NativeError>> + Send;
}
impl SavedSuccessorIo for CurrentSharedExecutor {
    async fn resume(
        &self,
        after: Option<String>,
    ) -> Result<Option<SharedSuccessorResume>, NativeError> {
        self.resume_next_successor(after).await
    }
    async fn poll(&self, after: Option<String>) -> Result<SharedExecutionProgress, NativeError> {
        self.poll_successor(after).await
    }
}
#[derive(Default)]
pub(super) struct Polling {
    journal: Option<String>,
    attempt: Option<String>,
}
impl Polling {
    pub(super) async fn poll(&mut self, executor: &CurrentSharedExecutor, live: impl Fn() -> bool) {
        self.existing(executor, live).await
    }
    async fn existing(&mut self, executor: &impl SavedSuccessorIo, live: impl Fn() -> bool) {
        if !live() {
            return;
        }
        match executor.resume(self.journal.clone()).await {
            Ok(Some(result)) => self.journal = Some(result.intent_id),
            Ok(None) => self.journal = None,
            Err(_) => {}
        }
        if !live() {
            return;
        }
        let result = executor.poll(self.attempt.clone()).await;
        super::shared_worker::advance(&mut self.attempt, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct Existing {
        requests: Mutex<Vec<(bool, Option<String>)>>,
        live: Arc<AtomicBool>,
        retire_during_resume: bool,
        reply: Mutex<Option<Result<Option<SharedSuccessorResume>, NativeError>>>,
    }
    impl SavedSuccessorIo for Existing {
        async fn resume(
            &self,
            after: Option<String>,
        ) -> Result<Option<SharedSuccessorResume>, NativeError> {
            self.requests.lock().unwrap().push((true, after));
            if self.retire_during_resume {
                self.live.store(false, Ordering::Release);
            }
            self.reply.lock().unwrap().take().unwrap_or(Ok(None))
        }
        async fn poll(
            &self,
            after: Option<String>,
        ) -> Result<SharedExecutionProgress, NativeError> {
            self.requests.lock().unwrap().push((false, after));
            Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: "original-attempt".into(),
            })
        }
    }
    fn fixture(
        reply: Result<Option<SharedSuccessorResume>, NativeError>,
        retire: bool,
    ) -> Existing {
        Existing {
            requests: Mutex::new(vec![]),
            live: Arc::new(AtomicBool::new(true)),
            retire_during_resume: retire,
            reply: Mutex::new(Some(reply)),
        }
    }
    #[test]
    fn request_accepts_only_original_assignment_selection() {
        let valid = serde_json::json!({"assignmentId":"existing-assignment"});
        assert!(serde_json::from_value::<SharedSuccessorRequest>(valid.clone()).is_ok());
        for field in [
            "startIntentId",
            "command",
            "provider",
            "registry",
            "consent",
            "tokens",
        ] {
            let mut input = valid.clone();
            input[field] = serde_json::json!("caller-choice");
            assert!(serde_json::from_value::<SharedSuccessorRequest>(input).is_err());
        }
    }
    #[tokio::test]
    async fn recovery_cursor_preserves_original_failure_and_does_not_block_other_attempts() {
        let io = fixture(
            Ok(Some(SharedSuccessorResume {
                intent_id: "saved-intent".into(),
                outcome: Err(source_error("server_unavailable")),
            })),
            false,
        );
        let mut polling = Polling::default();
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        assert_eq!(polling.journal.as_deref(), Some("saved-intent"));
        assert_eq!(polling.attempt.as_deref(), Some("original-attempt"));
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        assert!(
            polling.journal.is_none(),
            "end of saved journal rotates only its cursor"
        );
        assert_eq!(
            *io.requests.lock().unwrap(),
            vec![
                (true, None),
                (false, None),
                (true, Some("saved-intent".into())),
                (false, Some("original-attempt".into()))
            ]
        );
    }
    #[tokio::test]
    async fn authority_retirement_after_recovery_prevents_attempt_poll() {
        let io = fixture(Ok(None), true);
        let mut polling = Polling::default();
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        assert_eq!(*io.requests.lock().unwrap(), vec![(true, None)]);
    }
    #[tokio::test]
    async fn unavailable_saved_journal_does_not_advance_its_cursor() {
        let io = fixture(Err(source_error("server_unavailable")), false);
        let mut polling = Polling {
            journal: Some("last-saved".into()),
            attempt: None,
        };
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        assert_eq!(polling.journal.as_deref(), Some("last-saved"));
        assert_eq!(polling.attempt.as_deref(), Some("original-attempt"));
    }
}
