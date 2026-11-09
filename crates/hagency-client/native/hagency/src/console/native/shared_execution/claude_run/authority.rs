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
pub(super) enum Authority {
    Root(Arc<SharedProcessSink>, SharedRuntimeDelivery),
    Attempt(Arc<SharedAttemptProcessSink>, attempt_io::Delivery),
    #[cfg(feature = "shared-budget-execution")]
    Budget(Arc<SharedBudgetAttemptProcessSink>, budget_io::Delivery),
}
impl Claimed {
    pub(super) fn bind(self, source: SharedTextProcessSource) -> Result<Authority, NativeError> {
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
    ) -> Result<(), NativeError> {
        // Local commit precedes network recovery. Failed upload never discards evidence.
        let upload = match self {
            Self::Root(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                run::report_process(io, r, &d.command, &receipt).await
            }
            Self::Attempt(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
                attempt_run::report_process(io, r, &d.command, &receipt).await
            }
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(s, d) => {
                let receipt = s.record(observation).await.map_err(store)?;
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
pub(super) struct Lease {
    pub lease: SharedActivityLease,
    pub until: Instant,
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
        Ok(Self { lease, until })
    }
}
pub(super) struct Active {
    pub live: Live,
    control: SharedTextActivityControl,
    send: watch::Sender<Lease>,
}
#[derive(Clone)]
pub(super) struct Live {
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
        })
    }
    async fn renew(&mut self) -> Result<(), NativeError> {
        let (b, p, state) = self.live.proof().await?;
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
        let input = SharedActivityRenewInput {
            authority: state.lease.authority.clone(),
            renewal_id: id,
        };
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
        Ok(())
    }
    pub async fn wait<T>(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        future: impl std::future::Future<Output = Result<T, NativeError>>,
    ) -> Result<T, NativeError> {
        tokio::pin!(future);
        loop {
            if *cancel.borrow() {
                return Err(failure(409, "shared_execution_closed"));
            }
            let state = self.live.state();
            let wake = state
                .until
                .checked_sub(Duration::from_secs(20))
                .unwrap_or_else(Instant::now);
            tokio::select! {biased;
                _=cancel.changed()=>return Err(failure(409,"shared_execution_closed")),
                _=tokio::time::sleep_until(state.until.into())=>return Err(failure(409,"shared_execution_expired")),
                _=tokio::time::sleep_until(wake.into())=>{
                    tokio::select! {biased;
                        _=cancel.changed()=>return Err(failure(409,"shared_execution_closed")),
                        _=tokio::time::sleep_until(state.until.into())=>return Err(failure(409,"shared_execution_expired")),
                        renewed=self.renew()=>renewed?,
                    }
                },
                result=&mut future=>return result,
            }
        }
    }
}
