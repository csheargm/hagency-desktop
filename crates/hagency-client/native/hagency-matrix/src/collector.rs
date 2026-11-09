use crate::{CancellationToken, Error, HostConfig, HostRoom, http::Http, sdk::Owner};
use hagency_core::replies::*;
use hagency_store::DomainStore;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::{Mutex, Semaphore};

/// Wall-clock milliseconds for the profile-reconcile throttle (the retained
/// `Date.now()`). A missing clock reads 0, which the throttle treats as
/// "never checked" — so a clock fault never suppresses a reconcile.
fn profile_clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod observation;
macro_rules! observe {
    ($phase:ident) => {
        observe!($phase, None);
    };
    ($phase:ident, $index:expr) => {
        #[cfg(test)]
        crate::collector::observation::mark(crate::collector::observation::Phase::$phase, $index);
    };
}
pub(crate) use observe;

pub(crate) struct Inner {
    pub(crate) enrollment_jobs: crate::enrollment::Jobs,
    pub(crate) config: HostConfig,
    pub(crate) http: Http,
    /// The representative/bot client for the send path's kick recovery
    /// (bridge-matrix.js:10912-10918): re-invite before the agent rejoins.
    /// Built only when the provisioning custody handed a credential over.
    pub(crate) representative: Option<Http>,
    pub(crate) domain: DomainStore,
    pub(crate) owner: Mutex<Option<Owner>>,
    pub(crate) busy: Arc<Semaphore>,
    pub(crate) attachment_handles: Arc<Semaphore>,
    pub(crate) receiver: crate::receive::Receiver,
    pub(crate) uploads: crate::upload::Registry,
    /// Verification-time-only room snapshots + authority facts (ADR-095),
    /// captured at intake and read by the provisioning hook; never stored.
    pub(crate) room_facts: Mutex<BTreeMap<String, (MatrixRoomObservation, RoomAuthorityFacts)>>,
    /// ADR-188: rooms this agent joined by invitation, read from the store on
    /// every pass (room → working). Never part of `config.rooms`, so the
    /// store binding is unchanged; working rooms join the sync filter, intake
    /// targets and send checks alongside the identity rooms.
    pub(crate) joined: std::sync::Mutex<BTreeMap<String, bool>>,
    /// Joined rooms the agent does not work in (encrypted, shared): observed
    /// so a change can be noticed, never published as a room scope.
    pub(crate) joined_shared: std::sync::Mutex<BTreeSet<String>>,
    /// When this agent's display name was last reconciled, in wall-clock ms;
    /// 0 is "never". The 300 s throttle of `reconcile_agent_profile`
    /// (bridge-matrix.js:5939-5940).
    pub(crate) profile_checked_at: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub(crate) handoff_fault: std::sync::atomic::AtomicU8,
    #[cfg(test)]
    pub(crate) handoff_reached: tokio::sync::Notify,
    #[cfg(test)]
    pub(crate) handoff_continue: tokio::sync::Notify,
    #[cfg(test)]
    pub(crate) outgoing_fault: std::sync::atomic::AtomicU8,
    #[cfg(test)]
    pub(crate) outgoing_reached: tokio::sync::Notify,
    #[cfg(test)]
    pub(crate) outgoing_continue: tokio::sync::Notify,
    #[cfg(test)]
    lose_positive_response: std::sync::atomic::AtomicBool,
}
pub struct Collector {
    pub(crate) inner: Arc<Inner>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct ObservationSummary {
    pub transport_generation: u64,
    pub rooms: usize,
}
impl Collector {
    /// Does not trust credentials or create an SDK identity until collect's
    /// authenticated exact whoami succeeds. No public raw-response setter.
    pub fn new(config: HostConfig, domain: DomainStore) -> Result<Self, Error> {
        if config.approval {
            return Err(Error::Config);
        }
        Ok(Self {
            inner: Inner::new(config, domain)?,
        })
    }
    /// One finite owned job, not a detached sync loop. Caller cancellation or
    /// dropping this future cannot discard a received failure before fencing.
    pub async fn collect(&self, cancel: &CancellationToken) -> Result<ObservationSummary, Error> {
        let permit = self
            .inner
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let inner = self.inner.clone();
        let cancel = cancel.clone();
        #[cfg(test)]
        let observation = observation::current();
        let job = async move {
            let _permit = permit;
            inner.collect(&cancel).await
        };
        #[cfg(test)]
        let job = observation::owned(observation, job);
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }
    pub async fn close(&self) -> Result<(), Error> {
        let _permit = self
            .inner
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        self.close_with_permit(_permit).await
    }
    /// Reconcile this agent's Matrix display name from its own definition,
    /// throttled to the retained 300 s per agent (`reconcileAgentProfile`,
    /// bridge-matrix.js:5938-5955). The control plane is this port's local
    /// store — the name is the engagement's own `agentName`, which the
    /// provision request's agent definition set — never an outbound backend
    /// fetch. The throttle is written only after a completed reconcile, so a
    /// refused one is retried on the next cycle exactly as the retained
    /// bridge retries it on its next registration poll; the caller warns and
    /// never ends the worker over it.
    pub async fn reconcile_agent_profile(
        &self,
        engagement: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, Error> {
        let inner = &self.inner;
        let now = profile_clock_ms();
        let last = inner
            .profile_checked_at
            .load(std::sync::atomic::Ordering::Relaxed);
        if last != 0
            && now.saturating_sub(last) < crate::identity_polish::PROFILE_RECONCILE_INTERVAL_MS
        {
            return Ok(false);
        }
        let name = inner
            .domain
            .engagement(engagement.to_owned())
            .await?
            .agent_name;
        let changed = crate::identity_polish::reconcile_display_name(
            &inner.http,
            &inner.config.identity.transport.sender_mxid,
            name.as_str(),
            name.as_str(),
            cancel,
        )
        .await?;
        inner
            .profile_checked_at
            .store(profile_clock_ms(), std::sync::atomic::Ordering::Relaxed);
        Ok(changed)
    }

    /// The engagement this collector's transport belongs to — task #12's
    /// invite poll keys records by the engagement's agent NAME, resolved
    /// from this id through the store.
    pub fn engagement_id(&self) -> &str {
        &self.inner.config.identity.transport.engagement_id
    }

    /// Task #12: one lightweight invite sync — `timeline limit 0`, the TS
    /// poll's exact filter (`bridge-matrix.js:7903`) — parsed into the
    /// invitations addressed to THIS collector's sender mxid.
    ///
    /// NO `busy` PERMIT, deliberately (board #86). The permit guards the SDK
    /// OWNER: the one `Owner` and the single-writer crypto/state stores under
    /// it, which every `busy`-taking job (`collect`, `intake`, `close`,
    /// `send_notice`, enrollment, approval) touches. This read touches none of
    /// them — it is a stateless `Http` GET, exactly the retained product's
    /// plain `fetch` sync, which holds no client lock and so cannot contend
    /// with the agent's own sync (`bridge-matrix.js:7894-8131`). Taking the
    /// permit here made a parked poll refuse the agent's refresh with `Busy`,
    /// and the agent stopped ingesting room events (the live 19447 run).
    pub async fn observe_invites(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<crate::invites::ObservedInvite>, Error> {
        let inner = self.inner.clone();
        let cancel = cancel.clone();
        let job = async move {
            let sync = crate::invites::invite_sync(&inner.http, None, &cancel).await?;
            Ok(crate::invites::parse_invites(
                &sync,
                &inner.config.identity.transport.sender_mxid,
            ))
        };
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }

    /// Task #12: accept an invitation by joining, returning the room id
    /// the SERVER reports (`bridge-matrix.js:9073-9082`).
    ///
    /// No `busy` permit, for `observe_invites`' reason: this is a stateless
    /// `POST /join` on `Http`, and holds nothing the SDK owner holds. A join
    /// parked on the permit used to refuse the agent's own refresh with `Busy`.
    pub async fn join_room(
        &self,
        room_id: &str,
        cancel: &CancellationToken,
    ) -> Result<String, Error> {
        let inner = self.inner.clone();
        let room_id = room_id.to_owned();
        let cancel = cancel.clone();
        let job = async move { crate::invites::join_room(&inner.http, &room_id, &cancel).await };
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }

    /// Task #12: decline by leaving — best-effort by design
    /// (`bridge-matrix.js:9135-9147`); the decision is the record.
    ///
    /// No `busy` permit, for `observe_invites`' reason: a stateless
    /// `POST /leave` on `Http`, holding nothing the SDK owner holds.
    pub async fn leave_room(&self, room_id: &str, cancel: &CancellationToken) -> Result<(), Error> {
        let inner = self.inner.clone();
        let room_id = room_id.to_owned();
        let cancel = cancel.clone();
        let job = async move { crate::invites::leave_room(&inner.http, &room_id, &cancel).await };
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }
    pub(crate) async fn close_with_permit(
        &self,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), Error> {
        self.inner.uploads.close()?;
        let inner = self.inner.clone();
        #[cfg(test)]
        let observation = observation::current();
        let job = async move {
            let _permit = _permit;
            // A clean close retires nothing: the incarnation stays available so
            // the same state directory starts again at the same generation, as
            // it already does after a crash. Only a failure fences.
            observe!(CloseOwnerLock);
            if let Some(owner) = inner.owner.lock().await.take() {
                observe!(CloseSdk);
                owner.close().await?;
            }
            Ok(())
        };
        #[cfg(test)]
        let job = observation::owned(observation, job);
        tokio::spawn(job).await.map_err(|_| Error::OutcomeUnknown)?
    }
}
impl Inner {
    pub(crate) fn new(config: HostConfig, domain: DomainStore) -> Result<Arc<Self>, Error> {
        let http = Http::new(&config)?;
        let representative = match &config.representative {
            Some(_) => Some(Http::new_representative(&config)?),
            None => None,
        };
        Ok(Arc::new(Self {
            enrollment_jobs: crate::enrollment::Jobs::default(),
            config,
            http,
            representative,
            domain,
            owner: Mutex::new(None),
            busy: Arc::new(Semaphore::new(1)),
            attachment_handles: Arc::new(Semaphore::new(crate::attachments::MAX_HANDLES)),
            receiver: crate::receive::Receiver::new(),
            uploads: crate::upload::Registry::new(),
            room_facts: Mutex::new(BTreeMap::new()),
            joined: std::sync::Mutex::new(BTreeMap::new()),
            joined_shared: std::sync::Mutex::new(BTreeSet::new()),
            profile_checked_at: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            handoff_fault: std::sync::atomic::AtomicU8::new(0),
            #[cfg(test)]
            handoff_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            handoff_continue: tokio::sync::Notify::new(),
            #[cfg(test)]
            outgoing_fault: std::sync::atomic::AtomicU8::new(0),
            #[cfg(test)]
            outgoing_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            outgoing_continue: tokio::sync::Notify::new(),
            #[cfg(test)]
            lose_positive_response: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub(crate) async fn collect(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ObservationSummary, Error> {
        let t = &self.config.identity.transport;
        observe!(ExpectedTransport);
        let prior = self
            .domain
            .matrix_transport_state(t.engagement_id.clone())
            .await?;
        if prior.as_ref().is_some_and(|p| {
            p.observation.registration_generation != t.registration_generation
                || p.observation.generation > t.generation
                || (p.observation.generation == t.generation
                    && (!p.available || p.observation != *t))
        }) {
            return Err(Error::Generation);
        }
        drop(prior);
        let result = async {
            observe!(Whoami);
            self.whoami(cancel).await?;
            observe!(OwnerLock);
            let mut guard = self.owner.lock().await;
            if guard.is_none() {
                observe!(OpenOwner);
                *guard = Some(Owner::open(&self.config).await?);
            }
            let owner = guard.as_ref().ok_or(Error::Storage)?;
            observe!(Batch);
            if let Some(batch) = owner.batch().await?
                && batch.phase != crate::event_batch::Phase::Derived
            {
                return Err(Error::OutcomeUnknown);
            }
            observe!(IntakeMode);
            if !owner.intake_mode().await? {
                observe!(Cursor);
                let cursor = owner.cursor().await?;
                let filter = json!({
                    "room": {
                        "rooms": self.observed().iter().map(|r| r.room_id.clone()).collect::<Vec<_>>(),
                        "timeline": {"limit": 0}, "ephemeral": {"types": []},
                        "account_data": {"types": []}, "state": {"lazy_load_members": false}
                    },
                    "presence": {"types": []}, "account_data": {"types": []}
                })
                .to_string();
                let mut query = vec![
                    ("timeout", "0"),
                    ("full_state", "true"),
                    ("filter", filter.as_str()),
                ];
                if let Some(cursor) = cursor.as_deref() {
                    query.push(("since", cursor));
                }
                observe!(SyncHttp);
                let value = self
                    .http
                    .request(&["_matrix", "client", "v3", "sync"], Some(&query), cancel)
                    .await?
                    .success()?;
                let value = self.scope_sync(value)?;
                observe!(SyncApply);
                owner.sync(value).await?;
            }
            drop(guard);
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            observe!(PublishTransport);
            self.domain.observe_matrix_transport(t.clone()).await?;
            #[cfg(test)]
            if self
                .lose_positive_response
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(Error::OutcomeUnknown);
            }
            for target in self.config.observed_rooms() {
                self.collect_room(target, cancel).await?;
            }
            Ok(ObservationSummary {
                transport_generation: t.generation,
                rooms: self.config.rooms.len(),
            })
        }
        .await;
        if let Err(error) = result {
            return self.refused_read(error).await;
        }
        result
    }
    pub(crate) async fn expected_transport(&self) -> Result<MatrixTransportObservation, Error> {
        let t = &self.config.identity.transport;
        observe!(ExpectedTransport);
        let prior = self
            .domain
            .matrix_transport_state(t.engagement_id.clone())
            .await?;
        if prior.as_ref().is_some_and(|p| {
            p.observation.registration_generation != t.registration_generation
                || p.observation.generation > t.generation
                || (p.observation.generation == t.generation
                    && (!p.available || p.observation != *t))
        }) {
            return Err(Error::Generation);
        }
        Ok(prior.map_or_else(|| t.clone(), |p| p.observation))
    }
    /// For observation that only reads the homeserver (whoami, sync, room
    /// state). A read that failed is not evidence about the transport: nothing
    /// was sent, and the last complete collection stands, as it would after a
    /// crash at this instant. So a refused read never fences (ADR-183, the
    /// operator's rule that the bridge decides nothing is done; the retained
    /// product retries every transport error and treats only an
    /// authentication rejection as final — and even that parks the worker for
    /// the human, it does not retire the incarnation). An unsafe room
    /// snapshot is evidence about the ROOM, and the room path retires that
    /// room's route on its own generation; it is not evidence about this
    /// incarnation either. `collect` sends no outbound Matrix write — whoami,
    /// sync and room state are reads, and its domain writes are local, whose
    /// outcome the next pass simply re-reads — so every refusal of it comes
    /// here. A path that DOES have an outbound write in flight keeps
    /// `fence_observation` (ADR-064's uncertainty is a write's), and the
    /// intake path keeps `fence_read` until this rule is carried there too.
    pub(crate) async fn refused_read<T>(&self, error: Error) -> Result<T, Error> {
        #[cfg(test)]
        observation::primary(error.clone());
        Err(error)
    }
    /// The homeserver could not be read (a restart, a dropped connection, a
    /// gateway error): no evidence about the incarnation at all. An identity
    /// mismatch, an authentication rejection or an unknown local custody
    /// outcome IS evidence and keeps its fence.
    pub(crate) fn unreachable(error: &Error) -> bool {
        matches!(
            error,
            Error::Timeout
                | Error::Transport
                | Error::Redirect
                | Error::Headers
                | Error::BodyTooLarge
                | Error::InvalidJson
                | Error::Busy
                | Error::Remote(500..=599)
                | Error::Remote(429)
        )
    }
    /// The pre-ADR-183 read fence, still used by the intake path: a refused
    /// read retires the incarnation unless it was the caller's own
    /// cancellation. Carrying `refused_read` there is the next slice's.
    pub(crate) async fn fence_read<T>(
        &self,
        expected: MatrixTransportObservation,
        error: Error,
    ) -> Result<T, Error> {
        // ADR-183: a homeserver that could not be read is not evidence about
        // this incarnation; the last complete batch stands and the next pass
        // reads again. Everything else keeps the original fence.
        if error == Error::Cancelled || Self::unreachable(&error) {
            #[cfg(test)]
            observation::primary(error.clone());
            return Err(error);
        }
        self.fence_observation(expected, error).await
    }
    pub(crate) async fn fence_observation<T>(
        &self,
        expected: MatrixTransportObservation,
        error: Error,
    ) -> Result<T, Error> {
        #[cfg(test)]
        observation::primary(error.clone());
        let t = &self.config.identity.transport;
        // Conservative whole-device fence: incomplete collection cannot
        // leave an earlier private room or long-lived grant usable.
        // Positive commit may have succeeded before its response was lost.
        // Try both known exact identities; never a third/newer incarnation.
        let mut failed = false;
        let candidates = if expected == *t {
            vec![expected]
        } else {
            vec![expected, t.clone()]
        };
        for expected in candidates {
            observe!(Fence);
            match self
                .domain
                .invalidate_matrix_transport(MatrixTransportInvalidation {
                    expected,
                    reason: "Matrix authenticated collection failed".into(),
                })
                .await
            {
                Ok(()) => {
                    #[cfg(test)]
                    observation::fence(None);
                }
                Err(
                    _error @ (hagency_store::Error::Generation | hagency_store::Error::Conflict),
                ) => {
                    #[cfg(test)]
                    observation::fence(Some(_error.into()));
                }
                Err(_error) => {
                    #[cfg(test)]
                    observation::fence(Some(_error.into()));
                    failed = true;
                }
            }
        }
        if failed {
            return Err(Error::OutcomeUnknown);
        }
        Err(error)
    }
    /// Only original factory Group rooms may advance beyond startup metadata.
    /// A current database row alone cannot replace this collector's own proof.
    pub(crate) async fn observed_room_generation(&self, target: &HostRoom) -> Result<u64, Error> {
        if self.config.factory_rooms.is_none() || !matches!(target.privacy, RoomPrivacy::Group {}) {
            return Ok(target.generation);
        }
        let observation = self
            .room_facts
            .lock()
            .await
            .get(&target.room_id)
            .map(|(observation, _)| observation.clone())
            .ok_or(Error::Generation)?;
        let t = &self.config.identity.transport;
        if observation.engagement_id != t.engagement_id
            || observation.registration_generation != t.registration_generation
            || observation.transport_generation != t.generation
            || observation.room_id != target.room_id
            || observation.privacy != target.privacy
        {
            return Err(Error::Generation);
        }
        let current = self
            .domain
            .matrix_room_state(t.engagement_id.clone(), target.room_id.clone())
            .await?;
        if !current.is_some_and(|room| room.available && room.generation == observation.generation)
        {
            return Err(Error::Generation);
        }
        Ok(observation.generation)
    }
    pub(crate) async fn collect_room(
        &self,
        target: &HostRoom,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        self.collect_room_observation(target, cancel)
            .await
            .map(|_| ())
    }
    pub(crate) async fn collect_room_observation(
        &self,
        target: &HostRoom,
        cancel: &CancellationToken,
    ) -> Result<MatrixRoomObservation, Error> {
        let t = &self.config.identity.transport;
        // Only `rooms` members are published: observe_matrix_room refuses a
        // Group room that is not the host engagement's own project room
        // (matrix_routes.rs:413), and the pre-project reception room has no
        // scope row by design. Every other observed room (the reception room
        // and any verify-time room fetch) keeps its authority facts in memory
        // for verify_request and is never published.
        let publish = self
            .config
            .rooms
            .iter()
            .any(|r| r.room_id == target.room_id)
            || (self.joined.lock().unwrap().contains_key(&target.room_id)
                && !self.joined_shared.lock().unwrap().contains(&target.room_id));
        let coordinator = self
            .config
            .factory_rooms
            .as_ref()
            .filter(|_| publish && matches!(target.privacy, RoomPrivacy::Group {}));
        // Serialize the complete original read/GET/CAS/recheck across the
        // finite warm factory, not independent per-device generation guesses.
        let _room_guard = if let Some(coordinator) = coordinator {
            Some(tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err(Error::Cancelled),
                guard=tokio::time::timeout(self.config.limits.sdk,coordinator.lock())=>guard.map_err(|_|Error::Timeout)?,
            })
        } else {
            None
        };
        observe!(RoomPrior);
        // The prior read exists to retire stale positive evidence when the
        // fetch fails; a never-published room can hold none, so a verify-only
        // room skips it (the reception room has no scope row by design).
        let prior = if publish {
            self.domain
                .matrix_room_state(t.engagement_id.clone(), target.room_id.clone())
                .await?
        } else {
            None
        };
        // ADR-183: a read that FAILED is not evidence about the room (a
        // homeserver restart, a dropped connection); the last complete
        // observation stands and the next pass reads again. Only a snapshot
        // that was received and then refused below retires the room.
        observe!(RoomHttp);
        let read = match self
            .http
            .request(
                &["_matrix", "client", "v3", "rooms", &target.room_id, "state"],
                None,
                cancel,
            )
            .await
            .and_then(|response| response.success())
        {
            Err(error) if error == Error::Cancelled || Self::unreachable(&error) => {
                return self.refused_read(error).await;
            }
            read => read,
        };
        let result = async {
            let state = read?;
            // The server's creation time of a direct room THIS agent created
            // (its own DM): the room's visibility starts there (ADR-184).
            let own_created = matches!(target.privacy, RoomPrivacy::Direct { .. })
                .then(|| {
                    state.as_array()?.iter().find_map(|e| {
                        (e["type"] == "m.room.create"
                            && e["state_key"] == ""
                            && e["sender"] == t.sender_mxid.as_str())
                        .then(|| e["origin_server_ts"].as_u64())
                        .flatten()
                    })
                })
                .flatten();
            // Representable unsafe membership/privacy must reach the domain's
            // shared-room invalidation path rather than becoming a local-only error.
            let (mut observation, facts) = self.room(target, state)?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !publish {
                self.room_facts
                    .lock()
                    .await
                    .insert(target.room_id.clone(), (observation.clone(), facts));
                return Ok(observation);
            }
            observe!(RoomPublish);
            if coordinator.is_some() {
                observation.generation = prior.as_ref().map_or(1, |room| room.generation);
                observation = self
                    .domain
                    .refresh_matrix_group_room(observation, prior.clone())
                    .await?;
            } else {
                self.domain.observe_matrix_room(observation.clone()).await?;
                if let Some(created_at) = own_created {
                    self.domain
                        .own_direct_room_created(
                            t.engagement_id.clone(),
                            target.room_id.clone(),
                            created_at,
                        )
                        .await?;
                }
            }
            self.room_facts
                .lock()
                .await
                .insert(target.room_id.clone(), (observation.clone(), facts));
            observe!(RoomRecheck);
            let current = self
                .domain
                .matrix_room_state(t.engagement_id.clone(), target.room_id.clone())
                .await?;
            if !current.is_some_and(|s| s.available && s.generation == observation.generation) {
                return Err(Error::Wire);
            }
            Ok(observation)
        }
        .await;
        #[cfg(test)]
        if let Err(error) = &result {
            observation::primary(error.clone());
        }
        // The caller's own cancellation of this read is not evidence about the
        // room, as it is not about the transport: nothing was refused and
        // nothing was sent. Retiring the shared project room here made every
        // agent's next collection fail with Generation and fence its transport.
        if result
            .as_ref()
            .is_err_and(|error| *error != Error::Cancelled)
            && let Some(prior) = prior
            && prior.available
        {
            // Only the scope captured before the failed request can be retired.
            // A concurrent newer room generation is preserved by the domain CAS.
            match self
                .domain
                .invalidate_matrix_room(MatrixRoomInvalidation {
                    engagement_id: t.engagement_id.clone(),
                    registration_generation: t.registration_generation,
                    transport_generation: t.generation,
                    room_id: target.room_id.clone(),
                    generation: prior.generation.checked_add(1).ok_or(Error::Capacity)?,
                    reason: hagency_core::replies::OBSERVATION_FAILED.into(),
                })
                .await
            {
                Ok(()) | Err(hagency_store::Error::Generation | hagency_store::Error::Conflict) => {
                }
                Err(_error) => {
                    #[cfg(test)]
                    observation::fence(Some(_error.into()));
                    return Err(Error::OutcomeUnknown);
                }
            }
        }
        result
    }
    pub(crate) async fn whoami(&self, cancel: &CancellationToken) -> Result<(), Error> {
        if let Some(guard) = &self.config.as_guard {
            guard.check(cancel).await?;
        }
        let value = self
            .http
            .request(
                &["_matrix", "client", "v3", "account", "whoami"],
                None,
                cancel,
            )
            .await?
            .success()?;
        if value.get("user_id").and_then(Value::as_str)
            != Some(&self.config.identity.transport.sender_mxid)
            || value.get("device_id").and_then(Value::as_str)
                != Some(&self.config.identity.transport.device_id)
            || value
                .get("is_guest")
                .is_some_and(|v| v != &Value::Bool(false))
        {
            return Err(Error::Identity);
        }
        if let Some(guard) = &self.config.as_guard {
            guard.check(cancel).await?;
        }
        Ok(())
    }
    pub(crate) fn sync_bounds(&self, value: &Value) -> Result<(), Error> {
        let token = value
            .get("next_batch")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?;
        if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_control) {
            return Err(Error::Wire);
        }
        if let Some(rooms) = value.get("rooms") {
            let rooms = rooms.as_object().ok_or(Error::Wire)?;
            for (kind, rooms) in rooms {
                if !matches!(kind.as_str(), "join" | "invite" | "leave" | "knock") {
                    return Err(Error::Wire);
                }
                for id in rooms.as_object().ok_or(Error::Wire)?.keys() {
                    ruma::RoomId::parse(id).map_err(|_| Error::Wire)?;
                }
            }
        }
        fn count(section: &Value, n: &mut usize, max: usize) -> Result<(), Error> {
            let section = section.as_object().ok_or(Error::Wire)?;
            if let Some(events) = section.get("events") {
                *n = n
                    .checked_add(events.as_array().ok_or(Error::Wire)?.len())
                    .ok_or(Error::Capacity)?;
                if *n > max {
                    return Err(Error::Capacity);
                }
            }
            Ok(())
        }
        // Only protocol event-list locations count. An event's arbitrary
        // content may itself contain an `events` map (power levels) or list;
        // it is not another sync batch and remains bounded by the HTTP body.
        let mut total = 0;
        for name in ["to_device", "presence", "account_data"] {
            if let Some(section) = value.get(name) {
                count(section, &mut total, self.config.limits.events)?;
            }
        }
        if let Some(rooms) = value.get("rooms").and_then(Value::as_object) {
            for rooms in rooms.values() {
                for room in rooms.as_object().ok_or(Error::Wire)?.values() {
                    let room = room.as_object().ok_or(Error::Wire)?;
                    for name in [
                        "state",
                        "timeline",
                        "ephemeral",
                        "account_data",
                        "invite_state",
                        "knock_state",
                    ] {
                        if let Some(section) = room.get(name) {
                            count(section, &mut total, self.config.limits.events)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    /// A server-side room filter is a bandwidth hint, not our authority
    /// boundary. Some homeservers return other joined rooms despite it.
    /// Validate the ENTIRE response's shape/event budget before narrowing the
    /// SDK application view to configured rooms. Account-scoped to-device and
    /// key metadata remain intact so crypto can progress; unrelated room state
    /// and timeline events never enter the SDK or an intake journal.
    pub(crate) fn scope_sync(&self, value: Value) -> Result<Value, Error> {
        let observed = self.observed();
        let allowed = observed
            .iter()
            .map(|r| r.room_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut value = self.scope_sync_to(value, &allowed)?;
        // ADR-188: a joined room's first sync after it enters the filter is
        // a limited (truncated) timeline, and so is any later gap. The intake
        // refuses limited timelines, which for an identity room is right; for
        // a joined room it would fence the whole transport. A joined room
        // only ever admits messages after the agent joined, so its gap is
        // dropped here and the room carries on from the next event.
        let identity = self
            .config
            .observed_rooms()
            .map(|r| r.room_id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(joined) = value
            .pointer_mut("/rooms/join")
            .and_then(Value::as_object_mut)
        {
            for (room, update) in joined.iter_mut() {
                if identity.contains(room.as_str())
                    || update.pointer("/timeline/limited") != Some(&Value::Bool(true))
                {
                    continue;
                }
                let dropped = update
                    .pointer("/timeline/events")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                eprintln!(
                    "joined room {room}: limited timeline, {dropped} earlier event(s) not read"
                );
                update["timeline"]["events"] = Value::Array(Vec::new());
                update["timeline"]["limited"] = Value::Bool(false);
            }
        }
        // A joined room the agent just left (or was removed from) arrives
        // under `leave` with its last events; the intake refuses those too.
        // Leaving is noticed by the driver's own membership check instead.
        if let Some(left) = value
            .pointer_mut("/rooms/leave")
            .and_then(Value::as_object_mut)
        {
            for (room, update) in left.iter_mut() {
                if !identity.contains(room.as_str()) && update.get("timeline").is_some() {
                    update["timeline"]["events"] = Value::Array(Vec::new());
                }
            }
        }
        Ok(value)
    }
    /// ADR-188: the identity rooms, then the working joined rooms.
    pub(crate) fn host_rooms(&self) -> Vec<HostRoom> {
        let mut rooms = self.config.rooms.clone();
        for (room_id, working) in self.joined.lock().unwrap().iter() {
            if *working && !rooms.iter().any(|r| &r.room_id == room_id) {
                rooms.push(HostRoom {
                    room_id: room_id.clone(),
                    generation: 1,
                    privacy: RoomPrivacy::Group {},
                });
            }
        }
        rooms
    }
    /// `host_rooms` followed by the reception room, as `observed_rooms`.
    pub(crate) fn observed(&self) -> Vec<HostRoom> {
        let mut rooms = self.host_rooms();
        rooms.extend(self.config.reception_room.iter().cloned());
        rooms
    }
    /// `scope_sync` over an explicit room set: the approval bot's rooms are
    /// derived from the store per engagement, not only its startup list.
    pub(crate) fn scope_sync_to(
        &self,
        mut value: Value,
        allowed: &BTreeSet<&str>,
    ) -> Result<Value, Error> {
        self.sync_bounds(&value)?;
        if let Some(rooms) = value.get_mut("rooms").and_then(Value::as_object_mut) {
            for rooms in rooms.values_mut() {
                rooms
                    .as_object_mut()
                    .ok_or(Error::Wire)?
                    .retain(|id, _| allowed.contains(id.as_str()));
            }
        }
        Ok(value)
    }
    pub(crate) fn room(
        &self,
        target: &HostRoom,
        value: Value,
    ) -> Result<(MatrixRoomObservation, RoomAuthorityFacts), Error> {
        let events = value.as_array().ok_or(Error::Wire)?;
        if events.len() > self.config.limits.events {
            return Err(Error::Capacity);
        }
        let mut states = BTreeSet::new();
        let mut joined = BTreeSet::new();
        let mut invite_only = false;
        let mut encrypted = false;
        let mut ts_binding = None;
        let mut facts = RoomAuthorityFacts::default();
        for event in events {
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .ok_or(Error::Wire)?;
            let key = event
                .get("state_key")
                .and_then(Value::as_str)
                .ok_or(Error::Wire)?;
            if !states.insert((kind, key))
                || event
                    .get("room_id")
                    .is_some_and(|v| v.as_str() != Some(target.room_id.as_str()))
            {
                return Err(Error::Wire);
            }
            let content = event
                .get("content")
                .and_then(Value::as_object)
                .ok_or(Error::Wire)?;
            match kind {
                "m.room.member" => {
                    matrix_user(key, &self.config.identity.server_name).map_err(|_| Error::Wire)?;
                    match content.get("membership").and_then(Value::as_str) {
                        Some("join") => {
                            joined.insert(key.to_owned());
                        }
                        Some("invite" | "leave" | "ban" | "knock") => {}
                        _ => return Err(Error::Wire),
                    }
                }
                "m.room.join_rules" => {
                    if !key.is_empty() {
                        return Err(Error::Wire);
                    }
                    invite_only =
                        content.get("join_rule").and_then(Value::as_str) == Some("invite");
                }
                "m.room.encryption" => {
                    if !key.is_empty()
                        || content.get("algorithm").and_then(Value::as_str)
                            != Some("m.megolm.v1.aes-sha2")
                    {
                        return Err(Error::Wire);
                    }
                    encrypted = true;
                }
                // ADR-095 mapping: verification-time-only authority facts read
                // from the same /state the scope snapshot already observes, and
                // carried in memory (never stored, no migration).
                "m.room.power_levels" => {
                    if !key.is_empty() {
                        return Err(Error::Wire);
                    }
                    facts.default_power = content
                        .get("users_default")
                        .and_then(Value::as_i64)
                        .ok_or(Error::Wire)?;
                    facts.invite_power = content
                        .get("invite")
                        .and_then(Value::as_i64)
                        .ok_or(Error::Wire)?;
                    let users = content
                        .get("users")
                        .and_then(Value::as_object)
                        .ok_or(Error::Wire)?;
                    for (mxid, level) in users {
                        matrix_user(mxid, &self.config.identity.server_name)
                            .map_err(|_| Error::Wire)?;
                        facts
                            .powers
                            .insert(mxid.clone(), level.as_i64().ok_or(Error::Wire)?);
                    }
                }
                "m.room.name" => {
                    if !key.is_empty() {
                        return Err(Error::Wire);
                    }
                    facts.name = content
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                "com.hagency.admin.binding.v1"
                    // TS parity (lib/fleet-protocol.js:52): the binding is a
                    // per-fleet state event keyed by the fleet id (never the
                    // empty key). TS reads the exact tuple
                    // `state/<type>/<fleetId>`; an event under another key is
                    // simply not this fleet's binding, and the fleet match is
                    // decided from the binding content (`fleetId`) downstream.
                    // Palpo also writes one under the empty key when it
                    // creates a project or approval room; TS never reads that
                    // tuple, so neither does the collector (not a refusal).
                    if !key.is_empty() && content.get("fleetId").and_then(Value::as_str) == Some(key) => {
                        ts_binding = Some(Value::Object(content.clone()));
                    }
                _ => {}
            }
        }
        // Only the current fleet-keyed administrative binding can establish
        // project authority; unknown/retired event types are never aliases.
        facts.binding = ts_binding;
        let t = &self.config.identity.transport;
        let observation = MatrixRoomObservation {
            engagement_id: t.engagement_id.clone(),
            registration_generation: t.registration_generation,
            transport_generation: t.generation,
            room_id: target.room_id.clone(),
            generation: target.generation,
            privacy: target.privacy.clone(),
            joined,
            invite_only,
            encrypted,
        };
        Ok((observation, facts))
    }
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests {
    use super::fixtures as common;
    use super::*;
    #[tokio::test]
    async fn native_factory_observed_group_generation() {
        let f = common::Fixture::new();
        let mut fake = common::Fake::start(false).await;
        let coordinator = Arc::new(Mutex::new(()));
        let config = |name: &str| {
            let mut config = f.config(&fake.endpoint);
            config.root = f.root.path().join(name);
            config.rooms = vec![HostRoom {
                room_id: "!project:example.test".into(),
                generation: 1,
                privacy: RoomPrivacy::Group {},
            }];
            config.factory_rooms = Some(coordinator.clone());
            config
        };
        let first = Collector::new(config("first"), f.store.clone()).unwrap();
        let second = Collector::new(config("second"), f.store.clone()).unwrap();
        let target = &first.inner.config.rooms[0];
        let cancel = CancellationToken::new();
        assert_eq!(
            first.inner.observed_room_generation(target).await,
            Err(Error::Generation)
        );
        f.store
            .observe_matrix_transport(f.identity.transport.clone())
            .await
            .unwrap();
        let (result, _) = tokio::join!(
            first.inner.collect_room_observation(target, &cancel),
            async {
                let request = fake.next().await;
                assert!(request.target.ends_with("/state"));
                request.json(200, common::state());
            }
        );
        assert_eq!(result.unwrap().generation, 1);
        assert_eq!(first.inner.observed_room_generation(target).await, Ok(1));
        let mut changed = common::state();
        changed.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@new_member:example.test","content":{"membership":"join"}}));
        let (result, _) = tokio::join!(
            second
                .inner
                .collect_room_observation(&second.inner.config.rooms[0], &cancel),
            async {
                fake.next().await.json(200, changed.clone());
            }
        );
        assert_eq!(result.unwrap().generation, 2);
        assert_eq!(
            first.inner.observed_room_generation(target).await,
            Err(Error::Generation),
            "another owner cannot supply this collector's observation"
        );
        let (result, _) = tokio::join!(
            first.inner.collect_room_observation(target, &cancel),
            async {
                fake.next().await.json(200, changed);
            }
        );
        assert_eq!(result.unwrap().generation, 2);
        assert_eq!(first.inner.observed_room_generation(target).await, Ok(2));
        f.store
            .invalidate_matrix_room(MatrixRoomInvalidation {
                engagement_id: f.identity.transport.engagement_id.clone(),
                registration_generation: 1,
                transport_generation: 1,
                room_id: target.room_id.clone(),
                generation: 3,
                reason: "original fixture authority revoked".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            first.inner.observed_room_generation(target).await,
            Err(Error::Generation)
        );
        assert_eq!(
            second
                .inner
                .observed_room_generation(&second.inner.config.rooms[0])
                .await,
            Err(Error::Generation)
        );
        let current = f
            .store
            .matrix_room_state(
                f.identity.transport.engagement_id.clone(),
                target.room_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.generation, 3);
        assert!(!current.available);
        first.close_refusal_owner().await.unwrap();
        second.close_refusal_owner().await.unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
    #[tokio::test]
    async fn native_matrix_sync_scope_narrows_ignored_server_filter_after_full_bounds() {
        let f = common::Fixture::new();
        let c = Collector::new(f.config("https://127.0.0.1:19443/"), f.store.clone()).unwrap();
        let mut scoped = common::sync("scoped");
        scoped["rooms"]["join"]["!direct:example.test"] = json!({
            "state": {"events": [{"type":"m.room.power_levels","content":{"events":{"m.room.message":0}}}]},
            "timeline": {"events": [], "limited": false}
        });
        let mut raw = scoped.clone();
        raw["rooms"]["join"]["!unrelated:example.test"] = json!({
            "state": {"events": [{"type":"m.room.name","content":{"name":"outside"}}]},
            "timeline": {"events": [{"type":"m.room.message","content":{"body":"outside"}}]}
        });
        raw["to_device"] = json!({"events":[{"type":"m.room_key","content":{}}]});
        raw["device_lists"] = json!({"changed":["@owner:example.test"],"left":[]});
        let narrowed = c.inner.scope_sync(raw.clone()).unwrap();
        assert_eq!(narrowed["rooms"], scoped["rooms"]);
        assert_eq!(narrowed["to_device"], raw["to_device"]);
        assert_eq!(narrowed["device_lists"], raw["device_lists"]);
        assert_eq!(narrowed["next_batch"], "scoped");
        raw["rooms"]["join"]["!unrelated:example.test"]["timeline"]["events"] =
            json!(vec![json!({}); c.inner.config.limits.events + 1]);
        assert_eq!(c.inner.scope_sync(raw), Err(Error::Capacity));
        c.close().await.unwrap();
        f.store.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn native_matrix_transport_generation_lost_positive_response_retires_nothing() {
        let mut fake = common::Fake::start(false).await;
        let mut f = common::Fixture::new();
        f.store
            .observe_matrix_transport(f.identity.transport.clone())
            .await
            .unwrap();
        f.identity.transport.generation = 2;
        let c = Collector::new(f.config(&fake.endpoint), f.store.clone()).unwrap();
        c.inner
            .lose_positive_response
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let (result, _) = tokio::join!(c.collect(&cancel), async {
            fake.next().await.json(200, common::who());
            fake.next().await.json(200, common::sync("lost"));
        });
        assert_eq!(result, Err(Error::OutcomeUnknown));
        let state = f
            .store
            .matrix_transport_state(f.identity.transport.engagement_id.clone())
            .await
            .unwrap()
            .unwrap();
        // ADR-183 (ADR-047 amendment): the write itself committed — the
        // store holds generation 2 — and only its RESPONSE was lost. The
        // collection reports the uncertainty to its caller and retires
        // nothing: `collect` sends no outbound Matrix write, and the next
        // pass simply re-reads this local row. So the incarnation is
        // available and re-observing it is accepted.
        assert_eq!(state.observation.generation, 2);
        assert!(state.available);
        assert!(
            f.store
                .observe_matrix_transport(f.identity.transport.clone())
                .await
                .is_ok()
        );
        c.close().await.unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
    /// Board #61 row 5 (bridge-matrix.js:5938-5955): the display name comes
    /// from THIS agent's definition in the local store — never an outbound
    /// backend fetch — and the reconcile is throttled to one per 300 s. The
    /// fixture's engagement was admitted with the agent definition name
    /// "Worker" (`domain::request("worker", "Worker", ..)`), while its fresh
    /// account's localpart is machine-generated, so the definition's name
    /// overwrites it (lib/matrix-agent-profile.js:22).
    #[tokio::test]
    async fn native_matrix_agent_display_name_reconciles_from_the_store() {
        let f = common::Fixture::new();
        let mut fake = common::Fake::start(false).await;
        let c = Collector::new(f.config(&fake.endpoint), f.store.clone()).unwrap();
        let cancel = CancellationToken::new();
        let engagement = f.identity.transport.engagement_id.clone();
        let script = async {
            let read = fake.next().await;
            assert_eq!(read.method, "GET");
            assert!(read.target.ends_with("/displayname"));
            assert!(read.target.contains("@worker:example.test"));
            assert_eq!(
                read.headers["authorization"],
                format!("Bearer {}", common::TOKEN)
            );
            // The account's own localpart is machine-generated: overwritable.
            read.json(200, json!({"displayname": "worker"}));
            let write = fake.next().await;
            assert_eq!(write.method, "PUT");
            let body: Value = serde_json::from_slice(&write.body).unwrap();
            assert_eq!(body["displayname"], "Worker");
            write.json(200, json!({}));
            let readback = fake.next().await;
            assert_eq!(readback.method, "GET");
            readback.json(200, json!({"displayname": "Worker"}));
        };
        let (result, ()) = tokio::join!(c.reconcile_agent_profile(&engagement, &cancel), script);
        assert!(result.unwrap(), "the definition's name was written");
        // The 300 s throttle (bridge-matrix.js:5939-5940): a second pass now
        // issues no request at all.
        let admitted = fake.requests();
        let (second, ()) = tokio::join!(c.reconcile_agent_profile(&engagement, &cancel), async {});
        assert!(!second.unwrap());
        fake.quiesced(admitted, &common::limits()).await;
        c.close().await.unwrap();
        f.store.shutdown().await.unwrap();
        fake.close().await;
    }
}
