//! Project the existing signed versions without reserializing one as another.
use super::*;
use hagency_runtime::owned::{
    SharedTextActivityControl, SharedTextProcessObservation, SharedTextProcessSource,
};

pub(in crate::console::native::shared_execution) enum Claimed {
    Root(Box<(SharedRuntimeCustody, SharedRuntimeDelivery)>),
    Attempt(Box<(SharedAttemptCustody, attempt_io::Delivery)>),
    #[cfg(feature = "shared-budget-execution")]
    Budget(Box<(SharedBudgetAttemptCustody, budget_io::Delivery)>),
}
pub(in crate::console::native::shared_execution) enum Authority {
    Root(Arc<SharedProcessSink>, SharedRuntimeDelivery),
    Attempt(Arc<SharedAttemptProcessSink>, attempt_io::Delivery),
    #[cfg(feature = "shared-budget-execution")]
    Budget(Arc<SharedBudgetAttemptProcessSink>, budget_io::Delivery),
}
impl Claimed {
    pub(in crate::console::native::shared_execution) fn root(&self) -> &SharedRuntimeCommand {
        match self {
            Self::Root(c) => &c.1.command,
            Self::Attempt(c) => &c.1.command.original_runtime_command,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(c) => &c.1.command.original_runtime_command,
        }
    }
    pub(in crate::console::native::shared_execution) fn bind(self, source: SharedTextProcessSource) -> Result<Authority, NativeError> {
        Ok(match self {
            Self::Root(claimed) => {
                let (c, d) = *claimed;
                Authority::Root(Arc::new(c.bind_process(source).map_err(store)?), d)
            }
            Self::Attempt(claimed) => {
                let (c, d) = *claimed;
                Authority::Attempt(Arc::new(c.bind_process(source).map_err(store)?), d)
            }
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(claimed) => {
                let (c, d) = *claimed;
                Authority::Budget(Arc::new(c.bind_process(source).map_err(store)?), d)
            }
        })
    }
}
impl Authority {
    pub fn root(&self) -> &SharedRuntimeCommand {
        match self {
            Self::Root(_, d) => &d.command,
            Self::Attempt(_, d) => &d.command.original_runtime_command,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(_, d) => &d.command.original_runtime_command,
        }
    }
    pub fn id(&self) -> &str {
        match self {
            Self::Root(_, d) => &d.command.command_id,
            Self::Attempt(_, d) => &d.command.command_id,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(_, d) => &d.command.command_id,
        }
    }
    pub fn is_root(&self) -> bool {
        matches!(self, Self::Root(..))
    }
    pub fn is_budget(&self) -> bool {
        #[cfg(feature = "shared-budget-execution")]
        {
            matches!(self, Self::Budget(..))
        }
        #[cfg(not(feature = "shared-budget-execution"))]
        {
            false
        }
    }
    fn digest(&self) -> &str {
        match self {
            Self::Root(_, d) => &d.command_digest,
            Self::Attempt(_, d) => &d.command_digest,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(_, d) => &d.command_digest,
        }
    }
    fn process_digest(&self) -> Result<String, NativeError> {
        match self {
            Self::Root(s, _) => s.process_digest(),
            Self::Attempt(s, _) => s.process_digest(),
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(s, _) => s.process_digest(),
        }
        .map_err(store)
    }
    pub async fn qualified(
        &self,
        io: &Io,
        until: Instant,
        observation: SharedTextProcessObservation,
    ) -> Result<(SharedActivityLease, u64, Instant), NativeError> {
        let b = io.bound().await?;
        io::binding(&b.scope, self.root(), true)?;
        match self {
            Self::Root(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                let (ack, at, sent) = run::upload_process(&b, d, until, &receipt).await?;
                Ok((ack.activity.ok_or_else(invalid)?, at, sent))
            }
            Self::Attempt(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                let (ack, at, sent) = attempt_run::upload_process(&b, d, until, &receipt).await?;
                Ok((ack.activity.ok_or_else(invalid)?, at, sent))
            }
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                let (ack, at, sent) = budget_run::upload_process(&b, d, until, &receipt).await?;
                Ok((ack.activity.ok_or_else(invalid)?, at, sent))
            }
        }
    }
    pub async fn stopped(
        &self,
        io: &Io,
        r: &OwnerResourceRegistry,
        observation: SharedTextProcessObservation,
        after_local: impl FnOnce() -> Result<(), NativeError>,
    ) -> Result<(), NativeError> {
        // Local commit precedes network recovery. Failed upload never discards evidence.
        let upload = match self {
            Self::Root(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                after_local()?;
                run::report_process(io, r, &d.command, &receipt).await
            }
            Self::Attempt(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                after_local()?;
                attempt_run::report_process(io, r, &d.command, &receipt).await
            }
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                after_local()?;
                budget_run::report_process(io, r, &d.command, &receipt).await
            }
        };
        if let Err(error) = upload {
            tracing::debug!(code=%error.code,"Claude stop evidence retained for recovery");
        }
        Ok(())
    }
    pub async fn report(
        &self,
        io: &Io,
        r: &OwnerResourceRegistry,
        w: &SharedWorkItem,
        receipt: &SharedWorkReceipt,
    ) -> Result<(), NativeError> {
        match self {
            Self::Root(_, d) => run::report_work(io, r, &d.command, w, receipt).await,
            Self::Attempt(_, d) => attempt_run::report_work(io, r, &d.command, w, receipt).await,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(_, d) => budget_run::report_work(io, r, &d.command, w, receipt).await,
        }
        .map(|_| ())
    }
    pub async fn proof(
        &self,
        io: &Io,
        r: &OwnerResourceRegistry,
        state: &Lease,
    ) -> Result<(Bound, VerifiedSharedActivity), NativeError> {
        let b = io.bound().await?;
        let e = b.enrollment(r, self.root(), true).await?;
        let deadline = state.until.min(b.scope.deadline);
        let proof = match self {
            Self::Root(s, _) => s.authorize_activity(e, &state.lease, deadline),
            Self::Attempt(s, _) => s.authorize_activity(e, &state.lease, deadline),
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(s, _) => s.authorize_activity(e, &state.lease, deadline),
        }
        .map_err(store)?;
        Ok((b, proof))
    }
    pub fn work_poll(&self, a: SharedActivityRef) -> Action {
        #[cfg(feature = "shared-budget-execution")]
        if self.is_budget() {
            return budget_work_io::WorkAction::Poll(a).into();
        }
        Action::WorkPoll(a)
    }
    pub fn work_ack(&self, input: SharedWorkInput) -> Action {
        #[cfg(feature = "shared-budget-execution")]
        if self.is_budget() {
            return budget_work_io::WorkAction::Ack(input).into();
        }
        Action::WorkAck(input)
    }
    fn renewal(&self, input: SharedActivityRenewInput) -> Action {
        #[cfg(feature = "shared-budget-execution")]
        if self.is_budget() {
            return budget_work_io::WorkAction::Renew(input).into();
        }
        Action::Renew(input)
    }
}
#[derive(Clone)]
pub(in crate::console::native::shared_execution) struct Lease {
    pub lease: SharedActivityLease,
    pub until: Instant,
    renew_at: Instant,
}
impl Lease {
    fn new(
        authority: &Authority,
        lease: SharedActivityLease,
        observed: u64,
        started: Instant,
        b: &Bound,
    ) -> Result<Self, NativeError> {
        lease.authority.validate().map_err(store)?;
        if lease.command_digest != authority.digest()
            || lease.authority.runtime_command_id != authority.id()
            || lease.process_digest != authority.process_digest()?
        {
            return Err(invalid());
        }
        let until = io::window(
            &lease.authority.token,
            lease.expires_at_ms,
            observed,
            started,
            &b.scope,
        )?;
        if until <= Instant::now() {
            return Err(failure(409, "shared_execution_expired"));
        }
        Ok(Self {
            lease,
            until,
            renew_at: renewal_time(until),
        })
    }
}
pub(in crate::console::native::shared_execution) struct Active {
    pub live: Live,
    control: SharedTextActivityControl,
    send: watch::Sender<Lease>,
    renewal: Option<SharedActivityRenewInput>,
}
#[derive(Clone)]
pub(in crate::console::native::shared_execution) struct Live {
    pub io: Io,
    pub registry: OwnerResourceRegistry,
    pub authority: Arc<Authority>,
    pub lease: watch::Receiver<Lease>,
}
impl Live {
    pub fn state(&self) -> Lease {
        self.lease.borrow().clone()
    }
    pub async fn proof(&self) -> Result<(Bound, VerifiedSharedActivity, Lease), NativeError> {
        let state = self.state();
        let (b, p) = self
            .authority
            .proof(&self.io, &self.registry, &state)
            .await?;
        Ok((b, p, state))
    }
}
impl Active {
    pub async fn new_octos(
        io: Io,
        r: OwnerResourceRegistry,
        authority: Arc<Authority>,
        process: &mut hagency_runtime::owned::OwnedOctosSession,
        lease: SharedActivityLease,
        observed: (u64, Instant),
    ) -> Result<Self, NativeError> {
        let b = io.bound().await?;
        let state = Lease::new(&authority, lease, observed.0, observed.1, &b)?;
        process.install_renewable_shared_activity(state.lease.authority.epoch, state.until).map_err(|_| invalid())?;
        let control = process.shared_activity_control().map_err(|_| invalid())?;
        let (send, lease) = watch::channel(state);
        Ok(Self {
            live: Live { io, registry: r, authority, lease },
            control, send, renewal: None,
        })
    }
    pub async fn new(
        io: Io,
        r: OwnerResourceRegistry,
        authority: Arc<Authority>,
        process: &mut OwnedClaudeSession,
        lease: SharedActivityLease,
        observed: (u64, Instant),
    ) -> Result<Self, NativeError> {
        let b = io.bound().await?;
        let state = Lease::new(&authority, lease, observed.0, observed.1, &b)?;
        process
            .install_renewable_shared_activity(state.lease.authority.epoch, state.until)
            .map_err(|_| invalid())?;
        let control = process.shared_activity_control().map_err(|_| invalid())?;
        let (send, lease) = watch::channel(state);
        Ok(Self {
            live: Live {
                io,
                registry: r,
                authority,
                lease,
            },
            control,
            send,
            renewal: None,
        })
    }
    async fn renew(&mut self) -> Result<(), NativeError> {
        let (b, p, state) = self.live.proof().await?;
        if self.renewal.is_none() {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).map_err(|_| failure(503, "native_random_unavailable"))?;
            let id = format!(
                "sar_{}",
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            self.live
                .registry
                .prepare_shared_renewal(p, id.clone())
                .await
                .map_err(store)?;
            self.renewal = Some(SharedActivityRenewInput {
                authority: state.lease.authority.clone(),
                renewal_id: id,
            });
        }
        // A lost response may already have advanced the server. Retry the same
        // original renewal, never create another id for an uncertain request.
        let input = self.renewal.as_ref().ok_or_else(invalid)?.clone();
        let sent = Instant::now();
        let (b, reply): (Bound, Timed<io::ActivityReply>) = self
            .live
            .io
            .upload_renewal(
                b,
                || self.live.authority.renewal(input.clone()),
                state.until,
            )
            .await?;
        let next = Lease::new(
            &self.live.authority,
            reply.value.activity,
            reply.observed,
            sent,
            &b,
        )?;
        if next.lease.authority.epoch <= state.lease.authority.epoch {
            return Err(invalid());
        }
        self.control
            .renew_lease(next.lease.authority.epoch, next.until)
            .map_err(|_| invalid())?;
        self.send.send_replace(next);
        self.renewal = None;
        Ok(())
    }
    pub async fn wait<T>(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        future: impl std::future::Future<Output = Result<T, NativeError>>,
    ) -> Result<T, NativeError> {
        wait(self, cancel, future).await
    }
}

fn renewal_time(until: Instant) -> Instant {
    let left = until.saturating_duration_since(Instant::now());
    until - Duration::from_secs(20).min(left / 2)
}
trait LeaseDriver: Send {
    fn times(&self) -> (Instant, Instant);
    fn transient(&self, error: &NativeError) -> bool;
    fn renew(&mut self) -> impl std::future::Future<Output = Result<(), NativeError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Driver {
        until: Instant,
        renew_at: Instant,
        calls: usize,
        fail_once: bool,
        blocked: Option<Arc<tokio::sync::Notify>>,
    }
    impl Driver {
        fn new() -> Self {
            Self {
                until: Instant::now() + Duration::from_secs(2),
                renew_at: Instant::now(),
                calls: 0,
                fail_once: false,
                blocked: None,
            }
        }
    }
    impl LeaseDriver for Driver {
        fn times(&self) -> (Instant, Instant) {
            (self.until, self.renew_at)
        }
        fn transient(&self, error: &NativeError) -> bool {
            error.code == "management_scope_changed"
        }
        async fn renew(&mut self) -> Result<(), NativeError> {
            self.calls += 1;
            if let Some(notify) = &self.blocked {
                notify.notify_one();
                return std::future::pending().await;
            }
            if std::mem::take(&mut self.fail_once) {
                return Err(failure(409, "management_scope_changed"));
            }
            self.until = Instant::now() + Duration::from_millis(80);
            self.renew_at = renewal_time(self.until);
            Ok(())
        }
    }
    #[tokio::test]
    async fn canceled_admission_does_not_poll_or_start_native_work() {
        let mut driver = Driver::new();
        let (_send, mut cancel) = watch::channel(true);
        let result: Result<(), _> = wait(&mut driver, &mut cancel, async {
            panic!("canceled work must never receive its first native poll")
        })
        .await;
        assert_eq!(result.unwrap_err().code, "shared_execution_closed");
        assert_eq!(driver.calls, 0);
    }
    #[tokio::test]
    async fn native_stream_continues_while_renewal_io_is_pending() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let mut driver = Driver::new();
        driver.blocked = Some(notify.clone());
        let (_send, mut cancel) = watch::channel(false);
        let value = tokio::time::timeout(
            Duration::from_secs(1),
            wait(&mut driver, &mut cancel, async {
                notify.notified().await;
                Ok("original final answer")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(value, "original final answer");
        assert_eq!(driver.calls, 1);
    }
    #[tokio::test]
    async fn transient_renewal_error_keeps_the_original_work_alive() {
        let mut driver = Driver::new();
        driver.fail_once = true;
        let (_send, mut cancel) = watch::channel(false);
        assert_eq!(
            wait(&mut driver, &mut cancel, async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(17)
            })
            .await
            .unwrap(),
            17
        );
        assert_eq!(
            driver.calls, 1,
            "transient failure backs off instead of spinning"
        );
    }
    #[tokio::test]
    async fn short_leases_neither_starve_native_work_nor_spin_renewals() {
        let mut driver = Driver::new();
        let (_send, mut cancel) = watch::channel(false);
        wait(&mut driver, &mut cancel, async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
        .await
        .unwrap();
        assert!((1..=4).contains(&driver.calls), "renewals={}", driver.calls);
    }
}
impl LeaseDriver for Active {
    fn times(&self) -> (Instant, Instant) {
        let state = self.live.state();
        (state.until, state.renew_at)
    }
    fn transient(&self, error: &NativeError) -> bool {
        self.live.io.transient(error)
    }
    async fn renew(&mut self) -> Result<(), NativeError> {
        Active::renew(self).await
    }
}
async fn wait<T>(
    driver: &mut impl LeaseDriver,
    cancel: &mut watch::Receiver<bool>,
    future: impl std::future::Future<Output = Result<T, NativeError>>,
) -> Result<T, NativeError> {
    tokio::pin!(future);
    let mut retry_at = Instant::now();
    loop {
        if super::super::stop_capture::requested(cancel) {
            return Err(failure(409, "shared_execution_closed"));
        }
        let (until, wake) = driver.times();
        tokio::select! {biased;
            result = &mut future => return result,
            _ = cancel.changed() => return Err(failure(409, "shared_execution_closed")),
            _ = tokio::time::sleep_until(until.into()) => return Err(failure(409, "shared_execution_expired")),
            _ = tokio::time::sleep_until(wake.max(retry_at).into()) => {}
        }
        // Renewal IO can take most of a lease. Continue polling the SAME
        // provider future while credentials refresh or a reply is retried.
        let renewed = tokio::select! {biased;
            result = &mut future => return result,
            _ = cancel.changed() => return Err(failure(409, "shared_execution_closed")),
            _ = tokio::time::sleep_until(until.into()) => return Err(failure(409, "shared_execution_expired")),
            result = driver.renew() => result,
        };
        if let Err(error) = renewed {
            if !driver.transient(&error) {
                return Err(error);
            }
            tracing::debug!(code = %error.code, "Claude continues reading during transient renewal failure");
            retry_at = Instant::now() + Duration::from_millis(500);
        }
    }
}
