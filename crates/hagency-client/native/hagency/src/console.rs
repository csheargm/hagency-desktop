//! Native management API shared by Desktop and its owner daemon.
//! The historical module name and API routes are retained; no web UI is served.
mod accounts;
mod agent_detail;
mod agent_extras;
mod agents;
mod alerts;
mod approval_bindings;
mod approval_signal;
mod approvals;
mod authority;
pub mod client;
pub mod device_execution;
mod engagements;
mod exec_policy;
mod graphs;
pub(crate) mod hosting;
mod invites;
mod lifecycle;
mod matrix_diag;
pub(crate) mod native;
mod offer_book;
mod owned_agents;
mod owned_runtime;
mod owner_projects;
mod owner_provider;
pub mod palpo_import;
mod project_sides;
mod resource_configuration;
mod resources;
mod server_login;
mod setup;
mod side_budget;
mod side_lifecycle;
pub mod side_registration;
mod stream;
mod tasks;
mod usage;
use crate::{App, refusal};
use authority::{Authority, COOKIE, Session};
use hyper::Method;
use salvo::prelude::*;
use serde::Deserialize;
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum Error {
    #[error("native console request is invalid")]
    Invalid,
    #[error("native console access is required or expired")]
    Unauthorized,
    #[error("resource publication management scope is required")]
    Forbidden,
    #[error("resource configuration management scope is required")]
    ConfigurationForbidden,
    #[error("account enrollment management scope is required")]
    AccountForbidden,
    #[error("agent lifecycle management scope is required")]
    LifecycleForbidden,
    #[error("native console capacity is exhausted")]
    Busy,
    #[error("native console is unavailable")]
    Unavailable,
}
pub use server_login::AuthorizedDevice;
pub use approval_signal::subscribe as approvals_changed;
// TODO(R4): console.rs is over budget; move the in-process approval helpers into console/approval_signal.rs.
/// Whether "Always allow" could remember this runtime proposal (a pending
/// approval's `proposal`): only exact shell commands and Claude tool calls can.
pub fn can_remember(proposal: &serde_json::Value) -> bool {
    serde_json::from_value::<hagency_agent_local::ToolProposal>(proposal.clone())
        .is_ok_and(|p| hagency_agent_local::remembered::request_key(&p).is_some())
}

struct Inner {
    authority: Authority,
    requests: Arc<Semaphore>,
    /// Operator task graphs (#47): the TS `task_graphs.json` document on the
    /// state directory, loaded at startup like the retained boot. `None` when
    /// no state dir was provided — the routes then answer
    /// `console_unavailable`, TS's `dispatch_unavailable` class.
    graphs: Option<graphs::GraphStore>,
    server_login: server_login::ServerLogin,
    owned_runtime: owned_runtime::OwnedRuntime,
    owner_provider: owner_provider::OwnerProvider,
    hosting: std::sync::Mutex<Arc<hosting::Admission>>,
    dashboard_telemetry: std::sync::Mutex<Option<native::OwnerTelemetry>>,
}
/// Clones retain the same finite management authority.
#[derive(Clone)]
pub struct Console(Arc<Inner>);
impl Console {
    pub(crate) fn hosting_admission(&self) -> Arc<hosting::Admission> {
        self.0.hosting.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    /// Headless management facade with optional historical graph storage.
    pub fn with_state(state_dir: Option<&Path>) -> Result<Self, Error> {
        let console = Self(Arc::new(Inner {
            server_login: server_login::ServerLogin::new(state_dir)?,
            owned_runtime: owned_runtime::OwnedRuntime::default(),
            owner_provider: owner_provider::OwnerProvider::default(),
            hosting: std::sync::Mutex::new(Arc::new(hosting::Admission::running())),
            dashboard_telemetry: std::sync::Mutex::new(None),
            authority: match state_dir {
                Some(dir) => Authority::persistent(dir),
                None => Authority::new(),
            },
            requests: Arc::new(Semaphore::new(8)),
            graphs: match state_dir {
                Some(dir) => Some(graphs::GraphStore::open(dir).map_err(|_| Error::Unavailable)?),
                None => None,
            },
        }));
        if tokio::runtime::Handle::try_current().is_ok() {
            server_login::ServerLogin::start_worker(&console);
        }
        Ok(console)
    }
    /// Owner host startup never opens legacy graphs or persisted console grants.
    pub(crate) fn native_with_state(state: &Path) -> Result<Self, Error> {
        let console = Self(Arc::new(Inner {
            authority: Authority::new(),
            requests: Arc::new(Semaphore::new(8)),
            graphs: None,
            server_login: server_login::ServerLogin::new(Some(state))?,
            owned_runtime: owned_runtime::OwnedRuntime::default(),
            owner_provider: owner_provider::OwnerProvider::default(),
            hosting: std::sync::Mutex::new(Arc::new(hosting::Admission::running())),
            dashboard_telemetry: std::sync::Mutex::new(None),
        }));
        if tokio::runtime::Handle::try_current().is_ok() {
            server_login::ServerLogin::start_worker(&console);
        }
        Ok(console)
    }
    /// Local process entry point only; no HTTP issuer is mounted by OwnerHost.
    pub fn owner_access_ticket(&self) -> Result<String, Error> {
        self.0.authority.issue_owner_ticket()
    }
    pub(crate) async fn stop_owner_provider(&self) {
        self.0.owner_provider.stop_all().await;
    }
    pub(super) fn graphs(&self) -> Option<&graphs::GraphStore> {
        self.0.graphs.as_ref()
    }
    /// Current owner-scoped device capability for the Rust host runtime.
    pub async fn authorized_device(&self) -> Result<server_login::AuthorizedDevice, Error> {
        self.0.server_login.authorized_device().await
    }
    pub fn retire(&self) {
        self.0.server_login.retire();
        self.0.owned_runtime.request_stop_all();
        self.0.owner_provider.request_stop_all();
        self.0.authority.retire();
    }
    pub(super) async fn stop_owned_runtimes(&self) {
        self.0.owned_runtime.stop_all().await;
    }
}
pub(crate) fn router() -> Router {
    Router::with_path("console")
        .hoop(browser_boundary)
        .push(server_login::router())
        .push(Router::with_path("session").post(exchange).delete(logout))
        .push(
            Router::with_path("api")
                .hoop(authenticate)
                .push(usage::router())
                .push(alerts::router())
                .push(agents::router())
                .push(owned_agents::router())
                .push(owner_provider::router())
                .push(agent_extras::router())
                .push(exec_policy::router())
                .push(stream::router())
                .push(engagements::router())
                .push(setup::router())
                .push(graphs::router())
                .push(invites::router())
                .push(offer_book::router())
                .push(project_sides::router())
                .push(side_registration::router())
                .push(palpo_import::router())
                .push(side_budget::router())
                .push(side_lifecycle::router())
                .push(approvals::router())
                .push(approval_bindings::router())
                .push(resources::router())
                .push(accounts::router())
                .push(matrix_diag::router())
                .push(resource_configuration::router())
                // Task #46: the operator-facing capability and framework reads
                // (GET /api/capability, /api/frameworks, /api/frameworks/detect)
                // are session-scoped reads, mounted under the console API so
                // the native console reaches them without an operator bearer.
                .push(crate::fleet_views::router())
                .push(tasks::router())
                .push(tasks::extra_router()),
        )
        .push(Router::with_path("{**retired_ui}").get(retired_ui).head(retired_ui))
}
pub(crate) fn owner_router() -> Router {
    Router::with_path("console")
        .hoop(browser_boundary)
        .push(server_login::router())
        .push(Router::with_path("session").post(exchange).delete(logout))
        .push(
            Router::with_path("api")
                .hoop(authenticate)
                .push(owned_agents::router())
                .push(owner_provider::router())
                .push(owner_projects::router())
                .push(server_login::matrix_creation_router()),
        )
        .push(Router::with_path("{**retired_ui}").get(retired_ui).head(retired_ui))
}
// A removed document is 404, including the bare /console/ prefix that the
// router would otherwise classify as a known path with no allowed method.
#[handler]
async fn retired_ui(res: &mut Response) {
    res.status_code(StatusCode::NOT_FOUND);
}
pub(crate) fn operator_router() -> Router {
    Router::new().push(Router::with_path("console/access").post(issue))
}
fn console(depot: &Depot) -> Result<&Console, Error> {
    if let Ok(host) = depot.get_typed::<crate::owner_host::OwnerHost>() {
        return Ok(&host.console);
    }
    depot
        .get_typed::<App>()
        .ok()
        .and_then(|app| app.console.as_ref())
        .ok_or(Error::Unavailable)
}
fn failed(res: &mut Response, error: Error) {
    let (status, code) = match error {
        Error::Unavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "console_unavailable")
        }
        Error::Invalid => (StatusCode::BAD_REQUEST, "invalid_console_request"),
        Error::Unauthorized => (StatusCode::UNAUTHORIZED, "console_access_required"),
        Error::Forbidden => (StatusCode::FORBIDDEN, "resource_publication_scope_required"),
        Error::ConfigurationForbidden => (
            StatusCode::FORBIDDEN,
            "resource_configuration_scope_required",
        ),
        Error::AccountForbidden => (StatusCode::FORBIDDEN, "account_scope_required"),
        Error::LifecycleForbidden => (StatusCode::FORBIDDEN, "agent_lifecycle_scope_required"),
        Error::Busy => (StatusCode::TOO_MANY_REQUESTS, "console_busy"),
    };
    refusal(res, status, code);
}
fn host_authority(depot: &Depot) -> Option<&str> {
    if let Ok(host) = depot.get_typed::<crate::owner_host::OwnerHost>() {
        return Some(host.authority.as_str());
    }
    depot
        .get_typed::<App>()
        .ok()
        .map(|app| app.authority.as_str())
}
fn common_authority(req: &Request, depot: &Depot) -> bool {
    let Some(authority) = host_authority(depot) else {
        return false;
    };
    let headers = req.headers();
    headers.get_all("host").iter().count() == 1
        && headers.get("host").and_then(|v| v.to_str().ok()) == Some(authority)
        && !headers
            .keys()
            .any(|k| k == "forwarded" || k.as_str().starts_with("x-forwarded-"))
        && !headers.contains_key("authorization")
}
fn same_origin(req: &Request, depot: &Depot, mutation: bool) -> bool {
    let Some(authority) = host_authority(depot) else {
        return false;
    };
    let origin = format!("http://{authority}");
    let h = req.headers();
    h.get_all("sec-fetch-site").iter().count() == 1
        && h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("same-origin")
        && h.get_all("origin").iter().count() <= 1
        && match h.get("origin") {
            Some(value) => value.to_str().ok() == Some(origin.as_str()),
            None => !mutation,
        }
}
#[handler]
async fn browser_boundary(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
    ] {
        res.headers_mut()
            .insert(name, value.parse().expect("static header"));
    }
    if !common_authority(req, depot) {
        refusal(res, StatusCode::FORBIDDEN, "console_origin_required");
        ctrl.skip_rest();
        return;
    }
    let result = console(depot).and_then(|c| {
        c.0.requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)
    });
    match result {
        Ok(permit) => {
            depot.insert("console_permit", permit);
        }
        Err(error) => {
            failed(res, error);
            ctrl.skip_rest();
        }
    }
}
fn cookie(req: &Request) -> Result<&str, Error> {
    let headers = req.headers();
    if headers.get_all("cookie").iter().count() != 1 {
        return Err(Error::Unauthorized);
    }
    let value = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 4096)
        .ok_or(Error::Unauthorized)?;
    let mut selected = None;
    let mut count = 0;
    for field in value.split(';') {
        count += 1;
        if count > 16 {
            return Err(Error::Unauthorized);
        }
        let (name, value) = field.trim().split_once('=').ok_or(Error::Unauthorized)?;
        if name == COOKIE && selected.replace(value).is_some() {
            return Err(Error::Unauthorized);
        }
    }
    selected.ok_or(Error::Unauthorized)
}
fn current(req: &Request, depot: &Depot) -> Result<Session, Error> {
    // Board #92: HEAD is a SAFE method (RFC 9110 §9.2.1) and Express serves it
    // from the GET route, so the retained server runs its read guard for a
    // HEAD. Treating it as a mutation demanded an `Origin` header that a
    // browser never sends on a same-origin `<Link>` prefetch, so the console
    // 401'd its own prefetch.
    let mutation = !matches!(*req.method(), Method::GET | Method::HEAD);
    if !same_origin(req, depot, mutation) {
        return Err(Error::Unauthorized);
    }
    console(depot)?.0.authority.authenticate(cookie(req)?)
}
#[handler]
async fn authenticate(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    match current(req, depot) {
        Ok(session) => {
            if let Err(error) = console(depot)
                .unwrap()
                .0
                .server_login
                .validate(cookie(req).unwrap())
                .await
            {
                failed(res, error);
                ctrl.skip_rest();
                return;
            }
            depot.insert_typed(session);
        }
        Err(error) => {
            failed(res, error);
            ctrl.skip_rest();
        }
    }
}
fn recheck(depot: &Depot) -> Result<(), Error> {
    console(depot)?.0.authority.check(
        depot
            .get_typed::<Session>()
            .map_err(|_| Error::Unauthorized)?,
    )
}
async fn body(req: &mut Request, maximum: usize) -> Result<Vec<u8>, Error> {
    tokio::time::timeout(Duration::from_secs(2), req.payload_with_max_size(maximum))
        .await
        .map_err(|_| Error::Invalid)?
        .map(|v| v.to_vec())
        .map_err(|_| Error::Invalid)
}
#[handler]
async fn issue(req: &mut Request, depot: &Depot, res: &mut Response) {
    // TS parity: one issue route, no scope selection — the operator asks
    // for access and gets the whole console (`createApiAuthMiddleware`
    // admitted one credential to every `/api` route). No rate limit: the
    // TS middleware never throttled re-authentication.
    let result = async {
        let c = console(depot)?;
        let _permit =
            c.0.requests
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Busy)?;
        if req.uri().query().is_some() || !body(req, 1).await?.is_empty() {
            return Err(Error::Invalid);
        }
        let value = c.0.authority.issue()?;
        Ok(serde_json::json!({"ticket":value,"expires_in":120}))
    }
    .await;
    match result {
        Ok(value) => res.render(Json(value)),
        Err(error) => failed(res, error),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    ticket: String,
}
#[handler]
async fn exchange(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let result = async {
        if req.uri().query().is_some()
            || !same_origin(req, depot, true)
            || req.headers().get_all("content-type").iter().count() != 1
            || req
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            return Err(Error::Invalid);
        }
        let input: Exchange =
            serde_json::from_slice(&body(req, 256).await?).map_err(|_| Error::Invalid)?;
        console(depot)?.0.authority.exchange(&input.ticket)
    }
    .await;
    match result {
        Ok(value) => {
            // Browser-session cookie; Authority independently enforces the
            // OwnerHost's finite lifetime and process/logout revocation.
            res.headers_mut().insert(
                "set-cookie",
                format!("{COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/console")
                    .parse()
                    .expect("generated cookie"),
            );
            res.render(Json(serde_json::json!({"ok":true})));
        }
        Err(error) => failed(res, error),
    }
}
#[handler]
async fn logout(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let result = async {
        if req.uri().query().is_some() || !body(req, 1).await?.is_empty() {
            return Err(Error::Invalid);
        }
        let session = current(req, depot)?;
        console(depot)?.0.authority.revoke(&session)?;
        console(depot)?.0.server_login.sign_out(cookie(req)?).await;
        console(depot)?.stop_owned_runtimes().await;
        console(depot)?.0.owner_provider.stop_all().await;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            res.headers_mut().insert(
                "set-cookie",
                format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/console; Max-Age=0")
                    .parse()
                    .expect("static cookie"),
            );
            res.render(Json(serde_json::json!({"ok":true})));
        }
        Err(error) => failed(res, error),
    }
}
