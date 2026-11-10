//! One OS-owner resource ledger; Matrix profiles are explicitly granted access.
//! No caller-selected production path, credential storage or provider execution.
pub mod adoption;
mod capacity;
pub use capacity::{budget, shared_execution};
mod namespace;
pub use namespace::{NamespaceEnrollment, ResourceAccountSelection, ResourceNamespace};
mod provenance;
pub mod graph_journal;
mod edit_binding;
pub use edit_binding::ResourceEditBinding;
mod provider_binding;
pub use provider_binding::VerifiedOwnerResourceBinding;
mod previous_build;
#[cfg(test)]
mod tests;
use super::*;
use crate::{DomainStore, management::ResourceSummary};
pub(super) use capacity::additional_commitments;
pub(super) use capacity::unsettled_resource;
pub(super) use capacity::unsettled_resource_except;
pub(super) use capacity::unsettled_exact_resource;
pub use capacity::{
    InterruptedCall, InterruptedCallPage, InterruptedCallQuery, InterruptedCallReconciliation,
    ReconcileInterruptedCall,
    ManagedAgentPeriodRenewCommand, ManagedAgentPeriodReceipt, VerifiedManagedAgentPeriodDelivery,
    AGENT_SETTLEMENT_CAPABILITY, ManagedAgentSettlementCommand, ManagedAgentSettlementReceipt,
    VerifiedManagedAgentSettlement, AGENT_CAPABILITY, AGENT_FENCE_CAPABILITY,
    AGENT_MATRIX_CAPABILITY, AGENT_RUNTIME_CAPABILITY, AllocationCommand, CapacityGrantChainHead,
    CapacityGrantSuccession, CapacityGrantSuccessionFacts, GrantClosureCommand, GrantSuccession,
    RENEWAL_WINDOW_MS, SUCCESSION_AMENDMENT, SUCCESSION_RENEWAL,
    ManagedAgentDefinition, ManagedAgentFenceCommand, ManagedAgentFenceReceipt,
    ManagedAgentMatrixChallenge, ManagedAgentMatrixObservation, ManagedAgentReservationReceipt,
    ManagedAgentReserveCommand, ManagedAssignmentAuthority, ManagedCallAuthority, ManagedCallClaim,
    ManagedCallCustody, ManagedCallHistory, ManagedCleanupAuthority, ManagedObservationReceipt,
    ManagedProvisionClaim, ManagedProvisionCustody, ManagedProvisionHistory,
    ManagedProvisionObservation, ManagedRuntimeAcknowledgement, ManagedRuntimeClaim,
    ManagedRuntimeCommand, ManagedRuntimeCustody, ManagedRuntimeDelivery,
    ManagedRuntimeEvidenceSink, ManagedRuntimeHistory, ManagedRuntimeProvisionCommand,
    ManagedRuntimeReadiness, ManagedRuntimeReceipt, ManagedRuntimeRecordedProcess,
    ManagedRuntimeRecoverInput, ManagedRuntimeResultInput, ManagedRuntimeStopCommand,
    ManagedUsageObservation, NativeAllocationReceipt, NativeGrantReceipt,
    NativeGrantSettlementReceipt, OwnerGrantBinding, OwnerGrantConsent, OwnerGrantPeriod,
    OwnerGrantSelection, PARENT_CAPABILITY, VerifiedAllocationDelivery,
    VerifiedManagedAgentDelivery, VerifiedManagedAgentFence, VerifiedManagedMatrixCheck,
    VerifiedManagedRuntimeDelivery, VerifiedManagementEnrollment, VerifiedParentDelivery,
};
use hagency_core::canonical;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

static REGISTRIES: LazyLock<Mutex<std::collections::BTreeMap<PathBuf, Weak<Inner>>>> =
    LazyLock::new(Default::default);
struct Inner {
    store: DomainStore,
    id: String,
    closed: AtomicBool,
    /// Call ids whose custody object is alive in this process. Owner
    /// reconciliation refuses them: only a dead process leaves a call open.
    live_calls: std::sync::Mutex<std::collections::HashSet<String>>,
}
#[derive(Clone)]
pub struct OwnerResourceRegistry(Arc<Inner>);

/// Produced only by trusted SDK/host integration; never accepted from a script.
#[derive(Clone)]
pub struct ProfileAuthority {
    digest: String,
    account: String,
    expires: Instant,
    live: Arc<AtomicBool>,
    native_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl ProfileAuthority {
    pub fn new(digest: String, account: String, expires: Instant) -> Result<Self, Error> {
        hash(&digest)?;
        palpo_hagency_contract::MatrixUserId::try_from(account.clone())
            .map_err(|_| Error::LocalAuthority)?;
        if expires <= Instant::now() || expires > Instant::now() + Duration::from_secs(300) {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            digest,
            account,
            expires,
            live: Arc::new(AtomicBool::new(true)),
            native_gate: None,
        })
    }
    pub fn retire(&self) {
        self.live.store(false, Ordering::Release);
    }
    /// Bind the proof to the native host's synchronous session/device fence.
    /// This callback must not block or perform IO and is never script supplied.
    pub fn with_native_gate(
        mut self,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        self.native_gate = Some(Arc::new(gate));
        self.check()?;
        Ok(self)
    }
    fn check(&self) -> Result<(), Error> {
        if !self.live.load(Ordering::Acquire)
            || Instant::now() >= self.expires
            || self.native_gate.as_ref().is_some_and(|gate| !gate())
        {
            Err(Error::LocalAuthority)
        } else {
            Ok(())
        }
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    /// Revalidate the current native session/device gate after off-writer work.
    /// This does not mint authority or extend the proof's expiration.
    pub fn check_current(&self) -> Result<(), Error> {
        self.check()
    }
}
fn hash(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

/// A native provider adapter has verified a stable provider account subject.
/// A local home/path/credential-reference is not such proof. If unavailable,
/// the native UI may explicitly link an existing owner resource instead, but
/// this API cannot infer another independent account or mint capacity from it.
#[derive(Clone)]
pub struct VerifiedProviderAccount {
    digest: String,
    runtime: String,
    expires: Instant,
    claude: Option<hagency_runtime::claude::account::AccountBinding>,
}
impl VerifiedProviderAccount {
    pub fn new(
        runtime: &str,
        provider: &str,
        subject: &str,
        expires: Instant,
    ) -> Result<Self, Error> {
        if !matches!(runtime, "codex" | "claude")
            || provider.is_empty()
            || provider.len() > 128
            || subject.is_empty()
            || subject.len() > 512
            || provider
                .chars()
                .chain(subject.chars())
                .any(char::is_control)
            || expires <= Instant::now()
            || expires > Instant::now() + Duration::from_secs(60)
        {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            // One provider quota account has one seat even when multiple
            // runtimes, credential homes or Matrix profiles can access it.
            digest: canonical::digest(&json!({"provider":provider,"subject":subject}))?,
            runtime: runtime.into(),
            expires,
            claude: None,
        })
    }
    pub fn from_claude_binding(binding: hagency_runtime::claude::account::AccountBinding) -> Result<Self, Error> {
        binding.check().map_err(|_| Error::LocalAuthority)?;
        let mut proof = Self::new("claude", hagency_runtime::claude::account::PROVIDER, binding.subject(), binding.expires())?;
        if proof.digest != binding.provider_digest() { return Err(Error::LocalAuthority); }
        proof.claude = Some(binding);
        Ok(proof)
    }
    /// The original native provider probe, never reconstructed from a resource
    /// row. A shared Claude process must also produce its own matching receipt.
    pub fn claude_binding(&self) -> Result<&hagency_runtime::claude::account::AccountBinding, Error> {
        let binding = self.claude.as_ref().ok_or(Error::LocalAuthority)?;
        binding.check().map_err(|_| Error::LocalAuthority)?;
        Ok(binding)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerResourceReceipt {
    pub command_id: String,
    pub registry_id: String,
    pub resource_id: String,
    pub grant_revision: u64,
    pub ledger_revision: u64,
    pub active: bool,
}

/// Native UI owns this confirmation. Non-cloneable/non-deserializable; approve
/// consumes it once and rechecks profile, registry and provider observation.
pub struct OwnerResourceConsent {
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    command: String,
    action: Action,
    expires: Instant,
}
enum Action {
    Enroll {
        proof: VerifiedProviderAccount,
        model: String,
        reasoning: Option<String>,
        monthly_tokens: u64,
        namespace: Option<Box<ResourceAccountSelection>>,
    },
    Link {
        resource: String,
    },
    Revoke {
        resource: String,
    },
}
impl OwnerResourceConsent {
    pub fn account(&self) -> &str {
        self.profile.account()
    }
    pub fn resource(&self) -> Option<&str> {
        match &self.action {
            Action::Link { resource } | Action::Revoke { resource } => Some(resource),
            _ => None,
        }
    }
    pub async fn approve(self) -> Result<OwnerResourceReceipt, Error> {
        self.check()?;
        let store = self.registry.0.store.clone();
        let profile = self.profile.clone();
        let fence = self.registry.0.clone();
        let expires = match &self.action {
            Action::Enroll { proof, .. } => self.expires.min(proof.expires),
            _ => self.expires,
        };
        let result = store
            .call(16 * 1024, move |db| db.owner_consent(self))
            .await?;
        if profile.check().is_err()
            || fence.closed.load(Ordering::Acquire)
            || Instant::now() >= expires
        {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
    fn check(&self) -> Result<(), Error> {
        self.profile.check()?;
        if self.registry.0.closed.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        if let Action::Enroll { namespace: Some(selection), .. }=&self.action { selection.check()?; }
        if let Action::Enroll { proof, .. } = &self.action
            && Instant::now() >= proof.expires
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
/// Sealed canonical-only initialization, constructed solely after the fixed
/// owner root (or explicit isolated fixture root) passes registry checks.
pub(super) struct CanonicalStoreAuthority {
    root: PathBuf,
}
impl CanonicalStoreAuthority {
    pub(super) fn check(&self, path: &Path) -> Result<(), Error> {
        if path != self.root {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
#[cfg(feature = "test-support")]
static REGISTRY_OPEN_ATTEMPTED: AtomicBool = AtomicBool::new(false);

impl OwnerResourceRegistry {
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn ensure_fixture_startup() -> Result<(), Error> {
        if REGISTRY_OPEN_ATTEMPTED.load(Ordering::Acquire) {
            Err(Error::LocalAuthority)
        } else {
            Ok(())
        }
    }
    /// Isolated fixture only. Absent from production/default builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn isolated_for_tests(root: &Path) -> Result<Self, Error> {
        Self::open_root(root)
    }
    /// Runs fixture setup through the actual isolated canonical writer.
    /// This helper is absent from production/default builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn with_repository_for_tests<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut DomainRepository) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        self.0.store.call(128 * 1024, operation).await
    }
    /// Call from a blocking native initialization task. Application profile
    /// paths and HOME environment overrides cannot select an alternate ledger.
    pub fn open() -> Result<Self, Error> {
        let root = hagency_platform::owner_home()?.join(".hagency-owner-resources");
        Self::open_root(&root)
    }
    /// Existing native adoption selection cannot initialize an empty replacement
    /// for a missing canonical ledger. Paths still come only from the OS owner.
    pub fn open_existing() -> Result<Self, Error> {
        let root = hagency_platform::owner_home()?.join(".hagency-owner-resources");
        crate::private::open(&root.join("domain.sqlite3"), false)?;
        Self::open_root(&root)
    }
    fn open_root(root: &Path) -> Result<Self, Error> {
        #[cfg(feature = "test-support")]
        REGISTRY_OPEN_ATTEMPTED.store(true, Ordering::Release);
        crate::private::directory(root)?;
        let root = root.canonicalize()?;
        let mut registries = REGISTRIES.lock().map_err(|_| Error::Unavailable)?;
        if let Some(existing) = registries.get(&root).and_then(Weak::upgrade)
            && !existing.closed.load(Ordering::Acquire)
        {
            return Ok(Self(existing));
        }
        let file = root.join("domain.sqlite3");
        if file.exists() {
            // Inspect before opening/recovering: populated legacy/profile data
            // must never be silently adopted or have its runtime state changed.
            crate::private::open(&file, false)?;
            let db = rusqlite::Connection::open_with_flags(
                &file,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let has_identity:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_resource_identity')",[],|r|r.get(0))?;
            if !has_identity
                || db.query_row("SELECT COUNT(*) FROM owner_resource_identity", [], |r| {
                    r.get::<_, i64>(0)
                })? != 1
            {
                return Err(Error::LocalAuthority);
            }
        }
        let mut repository = DomainRepository::open_canonical(
            &root,
            CanonicalStoreAuthority { root: root.clone() },
        )?;
        let id = repository.bind_owner_registry(&root)?;
        let store = DomainStore::start(repository, 16)?;
        let inner = Arc::new(Inner {
            store,
            id,
            closed: AtomicBool::new(false),
            live_calls: std::sync::Mutex::new(std::collections::HashSet::new()),
        });
        registries.insert(root, Arc::downgrade(&inner));
        Ok(Self(inner))
    }
    pub fn id(&self) -> &str {
        &self.0.id
    }
    /// Names this database's mutation numbers to the server. Every database
    /// counts from 1, so a server tells two databases of one device apart by
    /// this name. It is derived, so the database identity itself stays local.
    pub fn ledger_id(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"hagency-owner-ledger-v1\0");
        hash.update(self.0.id.as_bytes());
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.0.closed.store(true, Ordering::Release);
        self.0.store.shutdown().await
    }
    fn consent(
        &self,
        profile: ProfileAuthority,
        command: String,
        action: Action,
        expires: Instant,
    ) -> Result<OwnerResourceConsent, Error> {
        hagency_core::project::identifier(&command, 128)?;
        if expires > Instant::now() + Duration::from_secs(120) {
            return Err(Error::LocalAuthority);
        }
        let consent = OwnerResourceConsent {
            registry: self.clone(),
            profile,
            command,
            action,
            expires,
        };
        consent.check()?;
        Ok(consent)
    }
    #[allow(clippy::too_many_arguments)] // Explicit native provider selection; no untyped script map.
    pub fn prepare_enrollment(
        &self,
        profile: ProfileAuthority,
        command: String,
        proof: VerifiedProviderAccount,
        model: String,
        reasoning: Option<String>,
        monthly_tokens: u64,
        expires: Instant,
    ) -> Result<OwnerResourceConsent, Error> {
        if monthly_tokens == 0
            || monthly_tokens > JSON_SAFE_MAX
            || model.is_empty()
            || model.len() > 256
            || model.chars().any(char::is_control)
            || reasoning
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 64 || v.chars().any(char::is_control))
        {
            return Err(InvalidInput("invalid owner resource selection").into());
        }
        self.consent(
            profile,
            command,
            Action::Enroll {
                proof,
                model,
                reasoning,
                monthly_tokens,
                namespace: None,
            },
            expires,
        )
    }
    /// A trusted owner consciously shares an existing resource with another
    /// authenticated profile. This adds access only; never another ceiling.
    pub fn prepare_link(
        &self,
        profile: ProfileAuthority,
        command: String,
        resource: String,
        expires: Instant,
    ) -> Result<OwnerResourceConsent, Error> {
        hagency_core::project::identifier(&resource, 128)?;
        self.consent(profile, command, Action::Link { resource }, expires)
    }
    pub fn prepare_revoke(
        &self,
        profile: ProfileAuthority,
        command: String,
        resource: String,
        expires: Instant,
    ) -> Result<OwnerResourceConsent, Error> {
        hagency_core::project::identifier(&resource, 128)?;
        self.consent(profile, command, Action::Revoke { resource }, expires)
    }
    pub async fn resource(
        &self,
        profile: &ProfileAuthority,
        id: String,
    ) -> Result<ResourceSummary, Error> {
        profile.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let caller_profile = profile.clone();
        let profile = profile.clone();
        let fence = self.0.clone();
        let result = self
            .0
            .store
            .call(4096, move |db| {
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                authorize_resource(&db.db, &profile, &id)?;
                let value = db.management_resource(&id, graphs::now_ms()?)?;
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                Ok(value)
            })
            .await?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        caller_profile.check()?;
        Ok(result)
    }
}
fn authorize_resource(db: &Connection, profile: &ProfileAuthority, id: &str) -> Result<(), Error> {
    let seat: String = db.query_row(
        "SELECT json_extract(config,'$.seatId') FROM resources WHERE id=?1",
        [id],
        |r| r.get(0),
    ).optional()?.ok_or(Error::LocalAuthority)?;
    let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2 AND active=1)",params![profile.digest,id],|r|r.get(0))?;
    if allowed {
        Ok(())
    } else {
        Err(Error::LocalAuthority)
    }
}

impl DomainRepository {
    fn bind_owner_registry(&mut self, root: &Path) -> Result<String, Error> {
        let directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        let identity = serde_json::to_string(&hagency_platform::directory_identity(
            &directory.into_std_file(),
        )?)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM management_identity)",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::LocalAuthority);
        }
        let stored: Option<(String, String)> = tx
            .query_row(
                "SELECT id,root_identity FROM owner_resource_identity WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let id = match stored {
            Some((id, old)) if old == identity => id,
            Some(_) => return Err(Error::Private),
            None => {
                let mut bytes = [0; 32];
                getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
                let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                tx.execute(
                    "INSERT INTO owner_resource_identity VALUES(1,?1,?2,1)",
                    params![id, identity],
                )?;
                id
            }
        };
        tx.commit()?;
        Ok(id)
    }
    fn owner_consent(
        &mut self,
        consent: OwnerResourceConsent,
    ) -> Result<OwnerResourceReceipt, Error> {
        consent.check()?;
        let action = match &consent.action {
            Action::Enroll {
                proof,
                model,
                reasoning,
                monthly_tokens,
                namespace,
            } => {
                let mut action=json!({"kind":"enroll","provider":proof.digest,"runtime":proof.runtime,"model":model,"reasoning":reasoning,"tokens":monthly_tokens});
                if let Some(selection)=namespace { selection.check_on(&self.db)?; action["namespace"]=selection.key(); }
                action
            }
            Action::Link { resource } => json!({"kind":"link","resource":resource}),
            Action::Revoke { resource } => json!({"kind":"revoke","resource":resource}),
        };
        let digest = canonical::digest(
            &json!({"registry":consent.registry.id(),"profile":consent.profile.digest,"account":consent.profile.account,"action":action}),
        )?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        consent.check()?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,result FROM owner_resource_receipts WHERE command_id=?1",
                [&consent.command],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old, value)) = old {
            if old != digest {
                return Err(Error::Conflict);
            }
            consent.check()?;
            return Ok(serde_json::from_str(&value)?);
        }
        let mut created_resource = false;
        let (resource, active) = match &consent.action {
            Action::Enroll {
                proof,
                model,
                reasoning,
                monthly_tokens,
                namespace,
            } => {
                if let Some(selection)=namespace { selection.check_on(&tx)?; }
                let mapping: Option<String> = tx
                    .query_row(
                        "SELECT seat_id FROM owner_resource_accounts WHERE provider_digest=?1",
                        [&proof.digest],
                        |r| r.get(0),
                    )
                    .optional()?;
                let seat_id = mapping
                    .clone()
                    .unwrap_or_else(|| format!("seat_owner_{}", proof.digest));
                // The owner chose this pool's maximum, not a provider-wide cap.
                let seat = Seat { id: seat_id.clone(), declaration: None };
                let stored: Option<String> = tx
                    .query_row("SELECT config FROM seats WHERE id=?1", [&seat_id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if let Some(old) = stored {
                    if mapping.is_none() {
                        return Err(Error::Quarantined);
                    }
                    if serde_json::from_str::<Seat>(&old)?.id != seat_id {
                        return Err(Error::Conflict);
                    }
                } else {
                    bounded_row(&tx, "seats", "id", &seat_id, 2048)?;
                    tx.execute(
                        "INSERT INTO seats(id,config) VALUES(?1,?2)",
                        params![seat_id, serde_json::to_string(&seat)?],
                    )?;
                    tx.execute(
                        "INSERT INTO owner_resource_accounts VALUES(?1,?2,'verified_provider')",
                        params![proof.digest, seat_id],
                    )?;
                }
                let mut definition=json!({"provider":proof.digest,"runtime":proof.runtime,"model":model,"reasoning":reasoning});
                if let Some(selection)=namespace { definition["namespace"]=selection.key(); }
                let preset = canonical::digest(&definition)?;
                let preset_id=if namespace.is_some() {format!("owner_ns_{preset}")} else {format!("owner_{preset}")};
                let r: Resource = serde_json::from_value(
                    json!({"presetId":preset_id,"seatId":seat_id,"framework":proof.runtime,"model":model,"reasoning":reasoning,"published":false,"ceiling":{"tokens":monthly_tokens,"period":"monthly"}}),
                )?;
                r.validate()?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1)",
                    [r.id()],
                    |r| r.get(0),
                )?;
                if !exists {
                    let r = prepare_resource_write(&tx, &r, Some(false), true)?;
                    write_resource_configuration(&tx, &r, true)?;
                    created_resource = true;
                } else if canonical::digest(&serde_json::to_value(read_resource(&tx, &r.id())?)?)?
                    != canonical::digest(&serde_json::to_value(&r)?)?
                {
                    return Err(Error::Conflict);
                }
                if let Some(selection)=namespace {
                    namespace::bind(&tx, &r.id(), selection)?;
                }
                (r.id(), true)
            }
            Action::Link { resource } => {
                let target = read_resource(&tx, resource)?;
                (resource.clone(), true)
            }
            Action::Revoke { resource } => {
                authorize_resource(&tx, &consent.profile, resource)?;
                (resource.clone(), false)
            }
        };
        let revision: Option<u64> = tx
            .query_row(
                "SELECT revision FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2",
                params![consent.profile.digest, resource],
                |r| r.get(0),
            )
            .optional()?;
        let revision = revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        tx.execute("INSERT INTO owner_resource_grants VALUES(?1,?2,?3,?4) ON CONFLICT(profile,resource_id) DO UPDATE SET revision=excluded.revision,active=excluded.active",params![consent.profile.digest,resource,revision,active])?;
        tx.execute(
            "UPDATE owner_resource_identity SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        let ledger_revision = tx.query_row(
            "SELECT revision FROM owner_resource_identity WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let receipt = OwnerResourceReceipt {
            command_id: consent.command.clone(),
            registry_id: consent.registry.id().into(),
            resource_id: resource,
            grant_revision: revision,
            ledger_revision,
            active,
        };
        bounded_row(
            &tx,
            "owner_resource_receipts",
            "command_id",
            &consent.command,
            100_000,
        )?;
        tx.execute(
            "INSERT INTO owner_resource_receipts VALUES(?1,?2,?3)",
            params![consent.command, digest, serde_json::to_string(&receipt)?],
        )?;
        if created_resource {
            provenance::created(&tx, &receipt, &digest, &consent.profile.digest)?;
        }
        if let Action::Enroll { namespace: Some(selection), model, reasoning, monthly_tokens, .. }=&consent.action {
            let input=NamespaceEnrollment {command_id:consent.command.clone(), account:selection.query.clone(), model:model.clone(), reasoning:reasoning.clone(), monthly_tokens:*monthly_tokens};
            tx.execute("INSERT INTO owner_namespace_enrollments VALUES(?1,?2,?3,?4)",params![consent.command,consent.profile.digest,canonical::digest(&serde_json::to_value(input)?)?,receipt.resource_id])?;
            selection.check_on(&tx)?;
        }
        consent.check()?;
        tx.commit()?;
        consent.check().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
