//! Bounded durable-intake polling under the signed-in native service lifetime.
//! Closing a mini-app cannot revoke this service or authorize a replacement
//! process. The native engine owns the exact demand and Stop journals.
use super::NativeError;
use ::hagency::native_owner::CurrentSharedExecutor;

trait DemandIo {
    fn recover_stop(
        &self,
        after: Option<String>,
    ) -> impl std::future::Future<Output = Result<Option<String>, NativeError>> + Send;
    fn recover_demand(
        &self,
        after: Option<String>,
    ) -> impl std::future::Future<Output = Result<Option<String>, NativeError>> + Send;
    fn poll_demand(
        &self,
        after: Option<String>,
    ) -> impl std::future::Future<Output = Result<Option<String>, NativeError>> + Send;
}

impl DemandIo for CurrentSharedExecutor {
    async fn recover_stop(&self, after: Option<String>) -> Result<Option<String>, NativeError> {
        self.resume_next_demand_stop(after)
            .await
            .map(|result| result.map(|saved| saved.command_id))
    }
    async fn recover_demand(&self, after: Option<String>) -> Result<Option<String>, NativeError> {
        self.resume_next_demand(after)
            .await
            .map(|result| result.map(|saved| saved.demand_id))
    }
    async fn poll_demand(&self, after: Option<String>) -> Result<Option<String>, NativeError> {
        CurrentSharedExecutor::poll_demand(self, after)
            .await
            .map(|result| result.next_after)
    }
}

#[derive(Default)]
pub(super) struct Polling {
    stop: Option<String>,
    demand: Option<String>,
    assignment: Option<String>,
}

impl Polling {
    pub(super) async fn poll(&mut self, executor: &CurrentSharedExecutor, live: impl Fn() -> bool) {
        self.existing(executor, live).await;
    }

    async fn existing(&mut self, io: &impl DemandIo, live: impl Fn() -> bool) {
        if !live() {
            return;
        }
        let result = io.recover_stop(self.stop.clone()).await;
        if !live() {
            return;
        }
        if let Ok(next) = result {
            self.stop = next;
        }
        let result = io.recover_demand(self.demand.clone()).await;
        if !live() {
            return;
        }
        if let Ok(next) = result {
            self.demand = next;
        }
        // Discovery, current Run revision, original device and budget remain
        // native checks. Recovery stays available when new issuance is disabled.
        let result = io.poll_demand(self.assignment.clone()).await;
        if !live() {
            return;
        }
        if let Ok(next) = result {
            self.assignment = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Fixture {
        live: AtomicBool,
        retire_at: Option<&'static str>,
        fail_at: Option<&'static str>,
        requests: Mutex<Vec<(&'static str, Option<String>)>>,
    }
    impl Fixture {
        fn call(
            &self,
            step: &'static str,
            after: Option<String>,
        ) -> Result<Option<String>, NativeError> {
            self.requests.lock().unwrap().push((step, after));
            if self.retire_at == Some(step) {
                self.live.store(false, Ordering::Release);
            }
            if self.fail_at == Some(step) {
                return Err(super::super::source_error("journal_unavailable"));
            }
            Ok(Some(format!("original-{step}")))
        }
    }
    impl DemandIo for Fixture {
        async fn recover_stop(&self, after: Option<String>) -> Result<Option<String>, NativeError> {
            self.call("stop", after)
        }
        async fn recover_demand(
            &self,
            after: Option<String>,
        ) -> Result<Option<String>, NativeError> {
            self.call("demand", after)
        }
        async fn poll_demand(&self, after: Option<String>) -> Result<Option<String>, NativeError> {
            self.call("assignment", after)
        }
    }
    fn fixture(retire_at: Option<&'static str>, fail_at: Option<&'static str>) -> Fixture {
        Fixture {
            live: AtomicBool::new(true),
            retire_at,
            fail_at,
            requests: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn original_stop_precedes_demand_recovery_and_each_cursor_is_independent() {
        let io = fixture(None, None);
        let mut polling = Polling::default();
        for _ in 0..2 {
            polling
                .existing(&io, || io.live.load(Ordering::Acquire))
                .await;
        }
        assert_eq!(
            *io.requests.lock().unwrap(),
            vec![
                ("stop", None),
                ("demand", None),
                ("assignment", None),
                ("stop", Some("original-stop".into())),
                ("demand", Some("original-demand".into())),
                ("assignment", Some("original-assignment".into())),
            ]
        );
    }

    #[tokio::test]
    async fn unavailable_original_journal_keeps_its_cursor_without_starving_other_assignments() {
        let io = fixture(None, Some("stop"));
        let mut polling = Polling {
            stop: Some("prior-stop".into()),
            ..Default::default()
        };
        polling
            .existing(&io, || io.live.load(Ordering::Acquire))
            .await;
        assert_eq!(polling.stop.as_deref(), Some("prior-stop"));
        assert_eq!(polling.demand.as_deref(), Some("original-demand"));
        assert_eq!(polling.assignment.as_deref(), Some("original-assignment"));
    }

    #[tokio::test]
    async fn retirement_during_any_await_prevents_followup_and_cursor_update() {
        for (step, count) in [("stop", 1), ("demand", 2), ("assignment", 3)] {
            let io = fixture(Some(step), None);
            let mut polling = Polling::default();
            polling
                .existing(&io, || io.live.load(Ordering::Acquire))
                .await;
            assert_eq!(io.requests.lock().unwrap().len(), count);
            match step {
                "stop" => assert!(polling.stop.is_none() && polling.demand.is_none()),
                "demand" => assert!(polling.demand.is_none() && polling.assignment.is_none()),
                _ => assert!(polling.assignment.is_none()),
            }
            polling
                .existing(&io, || io.live.load(Ordering::Acquire))
                .await;
            assert_eq!(io.requests.lock().unwrap().len(), count);
        }
    }
}
