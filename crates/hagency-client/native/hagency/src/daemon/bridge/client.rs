use super::{Call, Failure, Lease, Operation, Package, decode, encode, error, journal::Journal};
use crate::native_owner::*;
#[cfg(feature = "resource-adoption")]
use hagency_store::management as service;
use serde::Serialize;
use serde_json::Value;
use service::PendingIntent;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct ControllerClient {
    home: PathBuf,
    owner: String,
    origin: String,
    epoch: u64,
    lease: tokio::sync::Mutex<Option<(Lease, Instant)>>,
    retired: AtomicBool,
    pending: Mutex<Option<Arc<Journal>>>,
    runtime: Option<tokio::runtime::Handle>,
}
impl ControllerClient {
    pub fn new(home: PathBuf, owner: String, origin: String, epoch: u64) -> Arc<Self> {
        Arc::new(Self {
            home,
            owner,
            origin,
            epoch,
            lease: tokio::sync::Mutex::new(None),
            retired: AtomicBool::new(false),
            pending: Mutex::new(None),
            runtime: tokio::runtime::Handle::try_current().ok(),
        })
    }
    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Some(runtime) = self
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
        {
            runtime.spawn(task);
        }
    }
    pub(super) fn check(&self) -> Result<(), NativeError> {
        if self.retired.load(Ordering::Acquire) {
            Err(error(401, "management_scope_changed"))
        } else {
            Ok(())
        }
    }
    async fn send(&self, id:String, call:Call) -> Result<Value,NativeError> {
        self.send_receipt(id,call).await.0
    }
    #[cfg(not(unix))]
    async fn send_receipt(&self,_id:String,_call:Call)->(Result<Value,NativeError>,bool) {
        (Err(error(503,"owner_daemon_platform_unsupported")),false)
    }
    #[cfg(unix)]
    async fn send_receipt(&self,id:String,call:Call)->(Result<Value,NativeError>,bool) {
        let response:Result<Result<Value,Failure>,NativeError>=async {
            let mut client=super::super::DaemonClient::connect(&self.home).await.map_err(|_|error(503,"owner_daemon_unavailable"))?;
            let request=super::super::Request::new(id,Some(self.owner.clone()),super::super::Command::Bridge{call});
            let reply=client.call(&request).await.map_err(|_|error(503,"daemon_command_pending"))?;
            if let Some(error_body)=reply.error {return Err(error(503,&error_body.code));}
            decode(reply.result.ok_or_else(||error(503,"daemon_bridge_invalid_reply"))?)
        }.await;
        match response {
            Ok(Ok(value))=>(Ok(value),true),
            Ok(Err(failure))=>{let recorded=failure.terminal;(Err(failure.native()),recorded)},
            Err(error)=>(Err(error),false),
        }
    }
    async fn lease(&self) -> Result<Lease, NativeError> {
        self.check()?;
        let mut lease = self.lease.lock().await;
        if let Some((value, _)) = lease.as_ref() {
            // The daemon renews an active controller without changing its ID.
            // Reattach only after an actual expiry/revocation response, keeping
            // retained confirmation handles bound to their original manager.
            return Ok(value.clone());
        }
        let value: Lease = decode(
            self.send(
                super::super::new_request_id(),
                Call::Attach {
                    origin: self.origin.clone(),
                    epoch: self.epoch,
                },
            )
            .await?,
        )?;
        self.check()?;
        *lease = Some((value.clone(), Instant::now()));
        Ok(value)
    }
    async fn invalidate_lease(&self, id: &str) {
        let mut slot = self.lease.lock().await;
        if slot.as_ref().is_some_and(|(lease, _)| lease.id == id) {
            slot.take();
        }
    }
    fn journal(&self) -> Result<Arc<Journal>, NativeError> {
        let mut slot = self
            .pending
            .lock()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if let Some(value) = slot.as_ref() {
            return Ok(value.clone());
        }
        let dir = self.home.join("controllers");
        hagency_store::private::directory(&dir)
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let journal = Arc::new(Journal::open(&dir.join("pending.sqlite3"))?);
        *slot = Some(journal.clone());
        Ok(journal)
    }
    async fn call(&self, scope: &str, call: Call, mutation: bool) -> Result<Value, NativeError> {
        self.check()?;
        if serde_json::to_vec(&call).map_or(true, |bytes| {
            bytes.len() > super::super::protocol::MAX_FRAME - 1024
        }) {
            return Err(error(413, "invalid_management_arguments"));
        }
        let (id, record) = if mutation {
            let fingerprint = fingerprint(&self.owner, scope, &call)?;
            let journal = self.journal()?;
            let id=match journal.pending_for(&self.owner, &fingerprint) {
                Err(error) if error.code=="daemon_command_receipt_expired" && explicit_original_recovery(&call) => {
                    journal.resume_fingerprint(&self.owner,&fingerprint)?;
                    journal.pending_for(&self.owner,&fingerprint)?
                },
                result=>result?,
            };
            (id, Some((journal, fingerprint)))
        } else {
            (super::super::new_request_id(), None)
        };
        let (result, recorded) = self.send_receipt(id.clone(), call).await;
        // An outer gate or unrecognized error cannot settle an earlier
        // attempt. Only an authoritative receipt releases its logical ID.
        let pending = !recorded || result.as_ref().is_err_and(|error| error.code == "daemon_bridge_result_too_large");
        if !pending {
            if let Some((journal, fingerprint)) = record {
                journal.clear(&fingerprint, &id)?;
            }
        }
        self.check()?;
        result
    }
    /// Bounded recovery inventory for this owner. Unknown outcomes are kept;
    /// expiry never silently creates a replacement mutation.
    pub fn pending_commands(&self) -> Result<Vec<super::PendingCommand>, NativeError> {
        self.check()?;
        self.journal()?.pending_commands(&self.owner)
    }
    /// After the user selects an original request, allow its exact operation
    /// to recover that request again. This does not execute or clear anything.
    pub fn resume_pending_command(&self, request_id: &str) -> Result<(), NativeError> {
        self.check()?;
        self.journal()?.resume_pending(&self.owner, request_id)
    }
    pub async fn management(
        self: &Arc<Self>,
        package: ManagementPackage,
    ) -> Result<Arc<ManagementClient>, NativeError> {
        self.management_package(package.into()).await
    }
    pub async fn management_until(
        self: &Arc<Self>,
        package: ManagementPackage,
        until: Instant,
    ) -> Result<Arc<ManagementClient>, NativeError> {
        self.management_package(Package::from(package).with_deadline(until)?)
            .await
    }
    async fn management_package(
        self: &Arc<Self>,
        package: Package,
    ) -> Result<Arc<ManagementClient>, NativeError> {
        let (lease, id) = self.attach_manager(&package).await?;
        Ok(Arc::new(ManagementClient::Remote(RemoteManagement {
            controller: self.clone(),
            session: tokio::sync::Mutex::new(Some((lease, id))),
            package,
            retired: AtomicBool::new(false),
        })))
    }
    async fn attach_manager(&self, package: &Package) -> Result<(Lease,String),NativeError> {
        for _ in 0..2 {
        let lease = self.lease().await?;
        let result = self.call(
                &package.digest,
                Call::Management {
                    controller: lease.id.clone(),
                    package: package.clone(),
                },
                false,
            )
            .await;
        if result.as_ref().is_err_and(|error| error.code == "daemon_bridge_lease_expired") {
            self.invalidate_lease(&lease.id).await;
            continue;
        }
        return Ok((lease,decode(result?)?));
        }
        Err(error(401,"daemon_bridge_lease_expired"))

    }
    pub async fn execute(&self, command: Command) -> Result<Value, NativeError> {
        let mutation = super::owner_mutation(&command);
        let command = encode(command)?;
        for attempt in 0..2 {
            let lease = self.lease().await?;
            let result = self
                .call(
                    "owner",
                    Call::Execute {
                        controller: lease.id.clone(),
                        command: command.clone(),
                    },
                    mutation,
                )
                .await;
            if attempt == 0
                && result
                    .as_ref()
                    .is_err_and(|e| e.code == "daemon_bridge_lease_expired")
            {
                self.invalidate_lease(&lease.id).await;
            } else {
                return result;
            }
        }
        Err(error(401, "daemon_bridge_lease_expired"))
    }
    #[cfg(feature = "resource-adoption")]
    pub async fn owner_access(
        &self,
        package: Package,
        operation: super::OwnerOperation,
        arguments: Value,
    ) -> Result<Value, NativeError> {
        let lease = self.lease().await?;
        let result = self.call(
            &package.digest.clone(),
            Call::OwnerAccess {
                controller: lease.id.clone(),
                package,
                operation,
                arguments,
            },
            operation.is_mutation(),
        )
        .await;
        if result.as_ref().is_err_and(|error| error.code=="daemon_bridge_lease_expired") { self.invalidate_lease(&lease.id).await; }
        result
    }
    #[cfg(feature = "resource-adoption")]
    pub fn retire_owner_access(self: &Arc<Self>, access: String) {
        let this = self.clone();
        self.spawn(async move {
            if let Ok(lease) = this.lease().await {
                let _ = this
                    .send(
                        super::super::new_request_id(),
                        Call::RetireOwnerAccess {
                            controller: lease.id,
                            access,
                        },
                    )
                    .await;
            }
        });
    }
    /// Acceptance means the durable local logout fence is stored. The daemon
    /// drains and revokes its independent server grant asynchronously.
    pub async fn sign_out(&self) -> Result<(), NativeError> {
        super::super::revocation::request(&self.home, &self.origin, &self.owner)
            .map_err(|code| error(503, code))?;
        if let Some(request) = super::super::revocation::load(&self.home)
            .map_err(|code| error(503, code))?
        {
            if request.owner != self.owner || !super::super::same_origin(&request.origin, &self.origin) {
                return Err(error(401, "daemon_owner_scope_mismatch"));
            }
            // If the daemon is absent, also fence native root resumption. A
            // running host holds this root; its admission/token gates observe
            // the same durable request even when its socket cannot be reached.
            if let Ok(_held) = super::super::DaemonHome::open(&self.home) {
                NativeOwner::revoke_saved_matrix_resumption(&request.root, &request.origin, &request.owner)?;
            }
        }
        // Best effort wakeup. Do not make browser logout wait on network or
        // native drain completion; the request survives a closed controller.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            if let Ok(lease) = self.lease().await {
                let _ = self.send(super::super::new_request_id(), Call::SignOut { controller: lease.id }).await;
            }
        }).await;
        Ok(())
    }
    /// Local fencing is immediate. Remote expiry also covers process crashes.
    pub fn retire(self: &Arc<Self>) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            let lease = this.lease.lock().await.take();
            if let Some((lease, _)) = lease {
                let _ = this
                    .send(
                        super::super::new_request_id(),
                        Call::Detach {
                            controller: lease.id,
                        },
                    )
                    .await;
            }
        });
    }
}
fn explicit_original_recovery(call:&Call)->bool {
    match call {
        Call::Invoke{operation:Operation::retry_saved_command|Operation::acknowledge_command,..}=>true,
        Call::Invoke{operation:Operation::project_setup,arguments,..}=>matches!(decode::<super::project_setup::SetupOperation>(arguments.clone()),Ok(super::project_setup::SetupOperation::Resume{..}|super::project_setup::SetupOperation::Retry{..}|super::project_setup::SetupOperation::Acknowledge{..})),
        _=>false,
    }
}
pub(crate) fn fingerprint(owner: &str, scope: &str, call: &Call) -> Result<String, NativeError> {
    use sha2::{Digest, Sha256};
    // Lease IDs change on reattachment. They authorize this lookup but are
    // not the identity of an already admitted native mutation.
    let value = match call {
        Call::Invoke {
            operation,
            arguments,
            ..
        } => {
            let mut arguments = arguments.clone();
            if matches!(
                operation,
                Operation::save_personal_model
                    | Operation::save_personal_rules
                    | Operation::apply_web_connection
                    | Operation::retire_account
                    | Operation::create_resource_configuration
                    | Operation::prepare_personal_workspace
                    | Operation::edit_local_resource
                    | Operation::enroll_account_resource
            ) {
                if let Some(args) = arguments.as_array_mut() {
                    args.pop();
                }
            }
            encode((owner, scope, operation, arguments))?
        }
        #[cfg(feature = "resource-adoption")]
        Call::OwnerAccess {
            operation,
            arguments,
            ..
        } => {
            use super::OwnerOperation::*;
            let arguments = if matches!(
                operation,
                PublicationForm
                    | SupportsSuccession
                    | SuccessionForm
                    | PrepareGrant
                    | PrepareSuccessor
                    | ReviewGrant
                    | ConfirmGrant
                    | RecoverGrant
                    | ListGrants
                    | CheckAdmission
                    | ObservePublication
                    | ReviewPublication
                    | ConfirmPublication
                    | RecoverPublication
                    | ReviewCancellation
                    | ConfirmCancellation
            ) {
                arguments.get(1).unwrap_or(arguments)
            } else {
                arguments
            };
            encode((owner, scope, operation, arguments))?
        }
        Call::Execute { command, .. } => encode((owner, scope, "execute", command))?,
        Call::SignOut { .. } => encode((owner, scope, "signOut"))?,
        _ => encode(call)?,
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&value).map_err(|_| error(400, "invalid_management_arguments"))?
        )
    ))
}

pub struct RemoteManagement {
    controller: Arc<ControllerClient>,
    session: tokio::sync::Mutex<Option<(Lease, String)>>,
    package: Package,
    retired: AtomicBool,
}
impl RemoteManagement {
    fn release_session(&self, (lease, management): (Lease, String)) {
        let controller = self.controller.clone();
        self.controller.spawn(async move {
            let _ = controller
                .send(
                    super::super::new_request_id(),
                    Call::Retire {
                        controller: lease.id,
                        management,
                    },
                )
                .await;
        });
    }

    async fn session(&self) -> Result<(Lease, String), NativeError> {
        let lease = self.controller.lease().await?;
        let mut slot = self.session.lock().await;
        if let Some((old, id)) = slot.as_ref() {
            if old.id == lease.id {
                return Ok((old.clone(), id.clone()));
            }
        }
        let (lease, id) = self.controller.attach_manager(&self.package).await?;
        if self.retired.load(Ordering::Acquire) {
            let _ = self
                .controller
                .send(
                    super::super::new_request_id(),
                    Call::Retire {
                        controller: lease.id,
                        management: id,
                    },
                )
                .await;
            return Err(error(401, "management_scope_changed"));
        }
        *slot = Some((lease.clone(), id.clone()));
        Ok((lease, id))
    }
    pub(super) async fn invoke<T: Serialize>(
        &self,
        operation: Operation,
        arguments: T,
    ) -> Result<Value, NativeError> {
        if self.retired.load(Ordering::Acquire) {
            return Err(error(401, "management_scope_changed"));
        }
        let arguments = encode(arguments)?;
        let mutation = operation.mutation_for(&arguments)?;
        let mut result = Err(error(401, "daemon_bridge_lease_expired"));
        for _ in 0..2 {
            let (lease, id) = self.session().await?;
            result = self
                .controller
                .call(
                    &self.package.digest,
                    Call::Invoke {
                        controller: lease.id.clone(),
                        management: id,
                        operation,
                        arguments: arguments.clone(),
                    },
                    mutation,
                )
                .await;
            if result
                .as_ref()
                .is_err_and(|e| e.code == "daemon_bridge_lease_expired")
            {
                self.controller.invalidate_lease(&lease.id).await;
                self.session.lock().await.take();
            } else {
                break;
            }
        }
        if self.retired.load(Ordering::Acquire) {
            return Err(error(401, "management_scope_changed"));
        }
        result
    }
}
impl Drop for RemoteManagement {
    fn drop(&mut self) {
        // The remote lifetime follows the final local Arc, just like the
        // in-process NativeManagement lifetime. Use the original lease so
        // cleanup never attaches a new controller or retires a sibling handle.
        // Drain even after explicit retire: it may have raced a locked session.
        self.retired.store(true, Ordering::Release);
        if let Some(session) = self.session.get_mut().take() {
            self.release_session(session);
        }
    }
}
pub enum ManagementClient {
    Local(Arc<NativeManagement>),
    Remote(RemoteManagement),
}
impl ManagementClient {
    pub fn local(manager: Arc<NativeManagement>) -> Arc<Self> {
        Arc::new(Self::Local(manager))
    }
    pub fn is_retired(&self) -> bool {
        match self {
            Self::Local(m) => m.is_retired(),
            Self::Remote(m) => {
                m.retired.load(Ordering::Acquire)
                    || m.controller.retired.load(Ordering::Acquire)
                    || m.package.deadline().is_err()
            }
        }
    }
    pub fn retire(&self) {
        match self {
            Self::Local(m) => m.retire(),
            Self::Remote(m) => {
                m.retired.store(true, Ordering::Release);
                if let Ok(mut session) = m.session.try_lock() {
                    if let Some(session) = session.take() {
                        m.release_session(session);
                    }
                }
            }
        }
    }
}
macro_rules! facade {
    ($( $(#[$meta:meta])* $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty; $mutation:literal; )*)=>{
        impl ManagementClient {$($(#[$meta])* pub async fn $name(&self,$($arg:$ty),*)->Result<$result,NativeError>{
            match self {Self::Local(manager)=>manager.$name($($arg),*).await,Self::Remote(manager)=>decode(manager.invoke(Operation::$name,($($arg,)* )).await?)}
        })*}
    }
}
super::methods::management_methods!(facade);

#[cfg(all(test, unix))]
#[path = "client_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn active_controller_identity_survives_local_ninety_second_age() {
        let client=ControllerClient::new(PathBuf::from("/nonexistent-fixture"),"@o:s".into(),"https://s/".into(),1);
        *client.lease.lock().await=Some((Lease{id:"original-review-controller".into(),generation:1},Instant::now()-Duration::from_secs(95)));
        assert_eq!(client.lease().await.unwrap().id,"original-review-controller");
        client.retired.store(true,Ordering::Release);
        assert!(client.lease().await.is_err());
    }

    #[test]
    fn uncertainty_keeps_original_identity_across_lease_and_deadline_changes() {
        let make = |lease: &str, manager: &str, deadline: u64| Call::Invoke {
            controller: lease.into(),
            management: manager.into(),
            operation: Operation::save_personal_model,
            arguments: serde_json::json!(["sealed-original-review", deadline]),
        };
        let first = fingerprint("@o:s", "package", &make("c1", "m1", 60000)).unwrap();
        assert_eq!(
            first,
            fingerprint("@o:s", "package", &make("c2", "m2", 50000)).unwrap()
        );
        assert_ne!(
            first,
            fingerprint("@other:s", "package", &make("c1", "m1", 60000)).unwrap()
        );
        assert_ne!(
            first,
            fingerprint("@o:s", "other-package", &make("c1", "m1", 60000)).unwrap()
        );
    }
    #[cfg(feature = "resource-adoption")]
    #[test]
    fn grant_reattachment_preserves_native_original_request() {
        let package = || Package {
            digest: "a".repeat(64),
            instance: 1,
            account_epoch: 1,
            permissions: vec![],
            expires_at_ms: None,
        };
        let call = |access: &str, contribution: &str| Call::OwnerAccess {
            controller: "controller".into(),
            package: package(),
            operation: super::super::OwnerOperation::PrepareGrant,
            arguments: serde_json::json!([access,[contribution,{"allocatedTokens":20}]]),
        };
        assert_eq!(
            fingerprint("@o:s", "package", &call("a", "original")).unwrap(),
            fingerprint("@o:s", "package", &call("b", "original")).unwrap()
        );
        assert_ne!(
            fingerprint("@o:s", "package", &call("a", "original")).unwrap(),
            fingerprint("@o:s", "package", &call("b", "changed")).unwrap()
        );
    }
}
