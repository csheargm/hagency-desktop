//! Native managed Matrix setup. The shipped mini-app requests no setup capabilities.
//! Original creation custody survives rebase; unknown createRoom is never resent.
use super::{NativeError, NativeManagement, failure};
use crate::console::{
    AuthorizedDevice,
    device_execution::DeviceIdentity,
    server_login::{
        OwnerOperation,
        matrix_creations::{MatrixOperation, service::Scope as MatrixScope},
    },
};
use hagency_store::management::{HostContext, Permission, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
pub(in crate::console) mod matrix;
include!("project_setup/types.rs");
include!("project_setup/command.rs");
include!("project_setup/journal.rs");
include!("project_setup/driver.rs");
static WORKFLOWS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn invalid() -> NativeError {
    failure(502, "invalid_project_setup_response")
}
fn key(v: &str) -> Result<(), NativeError> {
    super::key(v)
}
fn safe(v: u64) -> Result<(), NativeError> {
    if v <= hagency_core::JSON_SAFE_MAX {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn positive(v: u64) -> Result<(), NativeError> {
    safe(v)?;
    if v == 0 { Err(invalid()) } else { Ok(()) }
}
fn hash(v: &str) -> Result<(), NativeError> {
    if v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn mxid(v: &str) -> Result<(), NativeError> {
    if v.len() <= 255
        && v.split_once(':')
            .is_some_and(|(a, b)| a.starts_with('@') && a.len() > 1 && !b.is_empty())
        && !v.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn room_id(v: &str) -> Result<(), NativeError> {
    crate::console::server_login::matrix_creations::room_id(v)
        .map_err(|e| failure(e.status, &e.code))
}
fn digest(v: &impl Serialize) -> Result<String, NativeError> {
    hagency_core::canonical::digest(&serde_json::to_value(v).map_err(|_| invalid())?)
        .map_err(|_| invalid())
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, NativeError> {
    fn exact(v: &Value) -> bool {
        match v {
            Value::Number(n) => n.as_u64().is_some_and(|n| n <= hagency_core::JSON_SAFE_MAX),
            Value::Array(a) => a.iter().all(exact),
            Value::Object(o) => o.values().all(exact),
            _ => true,
        }
    }
    if !exact(&v) || serde_json::to_vec(&v).map_or(true, |v| v.len() > 128 * 1024) {
        return Err(invalid());
    }
    serde_json::from_value(v).map_err(|_| invalid())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadReply {
    readiness: ProjectReadiness,
    observed_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryReply {
    recorded: bool,
    readiness: Option<ProjectReadiness>,
    observed_at_ms: u64,
}
/// Closed internal route; never constructible from a raw mini-app request.
pub(in crate::console) struct ServerOperation(Operation);
enum Operation {
    Read(String),
    Command(Box<Command>),
    Recover(Box<Command>),
}
impl ServerOperation {
    #[cfg(feature = "resource-adoption")]
    pub(super) fn recipient_read(project: String) -> Self { Self(Operation::Read(project)) }
    pub(in crate::console) fn request(
        self,
    ) -> Result<(reqwest::Method, String, Option<Value>), crate::console::server_login::OwnerError>
    {
        let build = || -> Result<_, NativeError> {
            let (method, path, body) = match self.0 {
                Operation::Read(p) => {
                    key(&p)?;
                    (reqwest::Method::GET, format!("projects/{p}/setup"), None)
                }
                Operation::Command(c) => {
                    c.validate()?;
                    let (m, p, b) = c.route();
                    (
                        reqwest::Method::from_bytes(m.as_bytes()).unwrap(),
                        p,
                        Some(b),
                    )
                }
                Operation::Recover(c) => {
                    c.validate()?;
                    (
                        reqwest::Method::POST,
                        format!("project-setup-commands/{}/recover", c.id),
                        Some(json!({"operation":c.operation()})),
                    )
                }
            };
            Ok((method, format!("/hagency/api/v1/management/{path}"), body))
        };
        build().map_err(|e| crate::console::server_login::OwnerError {
            status: e.status,
            code: e.code,
        })
    }
}
#[cfg(feature = "resource-adoption")]
pub(super) fn recipient_readiness(value: Value, project: &str) -> Result<ProjectReadiness, NativeError> {
    let reply: ReadReply = decode(value)?;
    positive(reply.observed_at_ms)?;
    reply.readiness.validate(project)?;
    Ok(reply.readiness)
}
struct NativeIo<'a> {
    manager: &'a NativeManagement,
    scope: MatrixScope<'a>,
    context: HostContext,
    session: Session,
    device: AuthorizedDevice,
}
impl NativeIo<'_> {
    fn require(&self, p: Permission) -> Result<(), NativeError> {
        self.session
            .authorize(&self.context, p)
            .map_err(super::management::store_error)
    }
    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        operation: Operation,
    ) -> Result<T, NativeError> {
        self.check().await?;
        let reply = self
            .manager
            .owner
            .0
            .host
            .console
            .owner_api_pinned(
                &DeviceIdentity::from(&self.device),
                OwnerOperation::ProjectSetup(ServerOperation(operation)),
            )
            .await;
        self.check().await?;
        let reply = reply.map_err(|e| failure(e.status, &e.code))?;
        if reply.owner != self.device.owner_mxid()
            || reply.origin != self.device.origin()
            || reply.issuer != self.device.issuer()
            || reply.subject != self.device.subject()
        {
            return Err(failure(401, "management_scope_changed"));
        }
        decode(reply.value)
    }
}
impl Io for NativeIo<'_> {
    fn owner(&self) -> &str {
        self.device.owner_mxid()
    }
    async fn check(&self) -> Result<(), NativeError> {
        self.require(Permission::ReadProjectSetup)?;
        self.device
            .bearer()
            .map_err(|_| failure(401, "management_scope_changed"))?;
        self.scope
            .check()
            .await
            .map_err(|e| failure(e.status, &e.code))?;
        let (current, _) = self.manager.authorized().await?;
        if current != self.context {
            return Err(failure(401, "management_scope_changed"));
        }
        self.require(Permission::ReadProjectSetup)
    }
    async fn read(&self, p: &str) -> Result<ProjectReadiness, NativeError> {
        let r: ReadReply = self.request(Operation::Read(p.into())).await?;
        positive(r.observed_at_ms)?;
        r.readiness.validate(p)?;
        Ok(r.readiness)
    }
    async fn command(&self, c: &Command) -> Result<ProjectReadiness, NativeError> {
        self.require(c.permission())?;
        let r: ReadReply = self
            .request(Operation::Command(Box::new(c.clone())))
            .await?;
        positive(r.observed_at_ms)?;
        r.readiness.validate(&c.project)?;
        Ok(r.readiness)
    }
    async fn recover(&self, c: &Command) -> Result<Option<ProjectReadiness>, NativeError> {
        self.require(c.permission())?;
        let r: RecoveryReply = self
            .request(Operation::Recover(Box::new(c.clone())))
            .await?;
        positive(r.observed_at_ms)?;
        if r.recorded != r.readiness.is_some() {
            return Err(invalid());
        }
        if let Some(v) = &r.readiness {
            v.validate(&c.project)?;
        }
        Ok(r.readiness)
    }
    async fn matrix(&self, operation: matrix::Operation) -> Result<Value, NativeError> {
        self.require(Permission::CreateProjectRooms)?;
        self.check().await?;
        let r = self.scope.matrix(MatrixOperation::Managed(operation)).await;
        self.check().await?;
        Ok(r.map_err(|e| failure(e.status, &e.code))?.value)
    }
}
/// The trusted native host provides the reviewed package permissions. The shipped
/// mini-app has no grants for these methods; server policy also stays default-off.
pub struct ProjectSetupTransport {
    manager: Arc<NativeManagement>,
}
enum Action<'a> {
    #[cfg(any(unix, test))]
    RetryTerminal(String, &'a std::sync::atomic::AtomicBool),
    #[cfg(any(unix, test))]
    ResumeBeginPinned(String, Box<dyn FnOnce(String) -> Result<(), NativeError> + Send + 'a>),
    // Keep the action lifetime valid when daemon-only variants are absent.
    Read(String, std::marker::PhantomData<&'a ()>),
    Apply(Command),
    ResumeBegin(String),
    Retry(String),
    Continue(String, SetupCandidates),
    Acknowledge(String),
}
impl ProjectSetupTransport {
    pub fn new(manager: Arc<NativeManagement>) -> Self {
        Self { manager }
    }
    pub async fn progress(&self, project: String) -> Result<SetupProgress, NativeError> {
        key(&project)?;
        self.run(Action::Read(project, std::marker::PhantomData)).await
    }
    pub async fn begin_create(&self, input: BeginSetup) -> Result<SetupProgress, NativeError> {
        self.run(Action::Apply(Command::new(
            input.project_id,
            Mutation::Begin {
                allocation_id: Some(input.allocation_id),
                expected_project_revision: input.expected_project_revision,
                expected_allocation_revision: input.expected_allocation_revision,
                expected_authority_revision: input.expected_authority_revision,
            },
        )?))
        .await
    }
    pub async fn assign_administrator(
        &self,
        input: AssignAdministrator,
    ) -> Result<SetupProgress, NativeError> {
        self.run(Action::Apply(Command::new(
            input.project_id,
            Mutation::Assign {
                expected_authority_revision: input.expected_authority_revision,
                administrator_mxid: input.administrator_mxid,
            },
        )?))
        .await
    }
    pub async fn cancel(
        &self,
        project: String,
        setup: String,
        expected_revision: u64,
    ) -> Result<SetupProgress, NativeError> {
        self.run(Action::Apply(Command::new(
            project,
            Mutation::Cancel {
                setup_id: setup,
                expected_revision,
            },
        )?))
        .await
    }
    pub async fn rebase(&self, input: RebaseSetup) -> Result<SetupProgress, NativeError> {
        self.run(Action::Apply(Command::new(
            input.project_id,
            Mutation::Rebase {
                setup_id: input.setup_id,
                expected_revision: input.expected_revision,
                allocation_id: input.allocation_id,
                expected_project_revision: input.expected_project_revision,
                expected_allocation_revision: input.expected_allocation_revision,
                expected_authority_revision: input.expected_authority_revision,
            },
        )?))
        .await
    }
    pub async fn continue_setup(
        &self,
        project: String,
        candidates: SetupCandidates,
    ) -> Result<SetupProgress, NativeError> {
        self.run(Action::Continue(project, candidates)).await
    }
    pub async fn resume_begin(&self, project: String) -> Result<SetupProgress, NativeError> {
        key(&project)?;
        self.run(Action::ResumeBegin(project)).await
    }
    #[cfg(any(unix, test))]
    pub(crate) async fn retry_command_terminal(&self, command:String, terminal:&std::sync::atomic::AtomicBool)->Result<SetupProgress,NativeError> {
        key(&command)?;
        self.run(Action::RetryTerminal(command,terminal)).await
    }
    #[cfg(any(unix, test))]
    pub(crate) async fn resume_begin_pinned(&self, project: String, pin: impl FnOnce(String) -> Result<(), NativeError> + Send) -> Result<SetupProgress, NativeError> {
        key(&project)?;
        self.run(Action::ResumeBeginPinned(project, Box::new(pin))).await
    }
    pub async fn retry_command(&self, command: String) -> Result<SetupProgress, NativeError> {
        key(&command)?;
        self.run(Action::Retry(command)).await
    }
    /// Only terminal rejected commands may be removed. Accepted begin and every
    /// submitted Matrix stage remain permanently under this installation's custody.
    pub async fn acknowledge_failure(&self, command: String) -> Result<SetupProgress, NativeError> {
        key(&command)?;
        self.run(Action::Acknowledge(command)).await
    }
    async fn run(&self, action: Action<'_>) -> Result<SetupProgress, NativeError> {
        let (context, session) = self.manager.authorized().await?;
        session
            .authorize(&context, Permission::ReadProjectSetup)
            .map_err(super::management::store_error)?;
        let device = self
            .manager
            .owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let cookie = self.manager.owner.0.cookie.lock().await.clone();
        let cookie = cookie
            .strip_prefix("hagency_console=")
            .ok_or_else(|| failure(401, "owner_authorization_required"))?;
        let gate_session = session.clone();
        let gate_context = context.clone();
        let gate_device = device.clone();
        let scope = MatrixScope::management(
            &self.manager.owner.0.host.console,
            cookie,
            &device,
            Box::new(move || {
                gate_device.bearer().is_ok()
                    && gate_session
                        .authorize(&gate_context, Permission::ReadProjectSetup)
                        .is_ok()
            }),
        )
        .await
        .map_err(|e| failure(e.status, &e.code))?;
        let identity = scope.management_identity(device.device_id());
        let path = scope.management_path();
        let io = NativeIo {
            manager: &self.manager,
            scope,
            context,
            session,
            device,
        };
        match &action {
            Action::Apply(c) => io.require(c.permission())?,
            Action::Continue(..) => {
                io.require(Permission::ManageProjectSetup)?;
                io.require(Permission::CreateProjectRooms)?;
            }
            _ => {}
        }
        let _guard = WORKFLOWS.lock().await;
        io.check().await?;
        let mut driver = Driver {
            history: History::load(&path, identity)?,
            io,
        };
        match action {
            Action::Read(p, _) => driver.progress(&p).await,
            Action::Apply(c) => driver.apply(c).await,
            Action::ResumeBegin(p) => driver.resume_begin(&p).await,
            #[cfg(any(unix, test))]
            Action::ResumeBeginPinned(p, pin) => {
                let n = driver.history.index(&p).ok_or_else(history_error)?;
                let k = driver.history.data.records[n].commands.iter().rposition(|c| matches!(c.mutation, Mutation::Begin { .. })).ok_or_else(history_error)?;
                pin(driver.history.data.records[n].commands[k].id.clone())?;
                driver.execute(n,k).await
            },
            Action::Retry(c) => driver.retry(&c).await,
            #[cfg(any(unix, test))]
            Action::RetryTerminal(c,terminal)=>{
                let result=driver.retry(&c).await;
                let failed=driver.history.data.records.iter().flat_map(|r|&r.commands).any(|command|command.id==c && matches!(command.outcome,Outcome::Failed{..}));
                terminal.store(failed,std::sync::atomic::Ordering::Release);
                result
            },
            Action::Continue(p, c) => driver.continue_setup(&p, c).await,
            Action::Acknowledge(id) => {
                let (n, k) = driver
                    .history
                    .data
                    .records
                    .iter()
                    .enumerate()
                    .find_map(|(n, r)| r.commands.iter().position(|c| c.id == id).map(|k| (n, k)))
                    .ok_or_else(history_error)?;
                let c = &driver.history.data.records[n].commands[k];
                driver.io.require(c.permission())?;
                if !matches!(c.outcome, Outcome::Failed { .. }) {
                    return Err(failure(409, "setup_command_not_terminal"));
                }
                let project = c.project.clone();
                driver.history.data.records[n].commands.remove(k);
                driver.history.save()?;
                driver.progress(&project).await
            }
        }
    }
}
#[cfg(test)]
mod tests;
