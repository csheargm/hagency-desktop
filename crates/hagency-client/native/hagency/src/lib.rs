// JIT PORT: relax spends more solver depth per nested future than rustc;
// the deep matrix-sdk Send checks overflow at the default 128.
#![recursion_limit = "1024"]
use hagency_core::custody::{Delivery, MAX_DELIVERY_BYTES};
use hagency_store::{DomainStore, Error, Store};
/// Explicit process-fixed resource authority for isolated native-host fixtures.
/// Not compiled into production/default builds; never uses the OS-owner root.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub use hagency_store::owner_resources::OwnerResourceRegistry;
    pub fn initialize_resource_authority_at(
        root: &std::path::Path,
    ) -> Result<(), hagency_platform::legacy_writer::Error> {
        hagency_platform::legacy_writer::test_support::initialize(root)
    }
    pub fn initialize_resource_authority() -> Result<(), hagency_platform::legacy_writer::Error> {
        hagency_platform::legacy_writer::test_support::initialize_process_root().map(|_| ())
    }
}
mod alerts;
pub mod bootstrap;
pub mod bot_commands;
pub mod console;
/// Per-user owner daemon and its private local control API (ADR 0012).
pub mod daemon;
mod documents;
/// Questions an Agent asks its room during a run, and the room's answers.
pub mod agent_questions;
pub mod agent_run_controls;
pub mod encrypted_approvals;
pub(crate) mod file_service;
pub(crate) mod fleet_views;
pub mod inspect;
pub mod issue_watch;
pub mod mcp;
pub mod native_owner;
mod operator;
mod operator_graphs;
pub mod operator_cli;
pub mod ops;
pub mod owner_host;
/// One owner root per account per computer (ADR-192).
pub mod owner_roots;
pub(crate) mod receive_service;
mod resources;
mod runner;
mod runtime;
pub mod server_admission;
pub mod service;
pub mod setup;
pub mod task_client;
mod usage;
use salvo::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct App {
    store: Store,
    domain: Option<DomainStore>,
    token_hash: [u8; 32],
    authority: String,
    requests: Arc<Semaphore>,
    development: Option<bootstrap::StatusHandle>,
    palpo: Option<bootstrap::palpo::StatusHandle>,
    palpo_live: Option<bootstrap::palpo::Live>,
    files: Option<file_service::FileHandle>,
    receives: Option<receive_service::ReceiveHandle>,
    fleet: Option<bootstrap::fleet::Routes>,
    console: Option<console::Console>,
    /// The ceiling-sweep loop's task handle (shared so readiness can observe
    /// liveness without owning the loop) and its tick channel. Both None
    /// when the loop was never started (unit wiring without `Bootstrap`).
    ceiling_sweep: Option<Arc<tokio::task::JoinHandle<()>>>,
    sweep_tick: Option<tokio::sync::watch::Receiver<bootstrap::CeilingSweepTick>>,
    /// The retention sweep's handle and tick channel — exactly the ceiling
    /// sweep's shape (wiring review b): bootstrap keeps the handle to abort
    /// at shutdown, readiness observes liveness and the last tick's word.
    /// Both None when the loop was never started.
    retention_sweep: Option<Arc<tokio::task::JoinHandle<()>>>,
    retention_tick: Option<tokio::sync::watch::Receiver<bootstrap::RetentionSweepTick>>,
}

impl App {
    pub fn new(store: Store, token: &[u8], address: SocketAddr) -> Result<Self, Error> {
        if (!address.ip().is_loopback() || address.port() == 0)
            || token.len() < 32
            || token.len() > 256
            || !token.iter().all(u8::is_ascii_graphic)
        {
            return Err(hagency_core::InvalidInput(
                "use a loopback address and a 32..256 byte ASCII token",
            )
            .into());
        }
        Ok(Self {
            store,
            domain: None,
            token_hash: Sha256::digest(token).into(),
            authority: address.to_string(),
            requests: Arc::new(Semaphore::new(8)),
            development: None,
            palpo: None,
            palpo_live: None,
            files: None,
            receives: None,
            fleet: None,
            console: None,
            ceiling_sweep: None,
            sweep_tick: None,
            retention_sweep: None,
            retention_tick: None,
        })
    }

    pub fn with_domain(mut self, domain: DomainStore) -> Self {
        self.domain = Some(domain);
        self
    }
    /// Attach the owning native fleet's shared routing table before serving.
    /// Entries can only be installed by that original Host service.
    pub fn with_fleet(mut self, fleet: &bootstrap::fleet::Service) -> Self {
        self.fleet = Some(fleet.routes());
        self
    }

    /// Attach the ceiling-sweep loop for readiness observation (brief 19).
    /// Called by `Bootstrap::serve` after `start_ceiling_sweep`; the handle
    /// is SHARED (bootstrap keeps it to abort at shutdown, `/health` reads
    /// liveness) so neither owns the loop. Public because integration tests
    /// wire the loop the same way bootstrap does. Readiness is diagnostic
    /// only: nothing may read it to retry, release or complete anything.
    pub fn with_ceiling_sweep(
        mut self,
        sweep: std::sync::Arc<tokio::task::JoinHandle<()>>,
        tick: tokio::sync::watch::Receiver<bootstrap::CeilingSweepTick>,
    ) -> Self {
        self.ceiling_sweep = Some(sweep);
        self.sweep_tick = Some(tick);
        self
    }

    /// Attach the retention sweep loop the same way (wiring review b): the
    /// handle is shared so bootstrap aborts it at shutdown and readiness
    /// observes liveness; the tick channel is the liveness/observation hook
    /// the retention loop's own `let _ = …` drop removed. Readiness is
    /// diagnostic only — an outcome word never fails it.
    pub fn with_retention_sweep(
        mut self,
        sweep: std::sync::Arc<tokio::task::JoinHandle<()>>,
        tick: tokio::sync::watch::Receiver<bootstrap::RetentionSweepTick>,
    ) -> Self {
        self.retention_sweep = Some(sweep);
        self.retention_tick = Some(tick);
        self
    }

    pub fn with_console(mut self, console: console::Console) -> Self {
        self.console = Some(console);
        self
    }

    pub(crate) fn with_development(mut self, status: bootstrap::StatusHandle) -> Self {
        self.development = Some(status);
        self
    }

    pub(crate) fn with_palpo(mut self, status: bootstrap::palpo::StatusHandle) -> Self {
        self.palpo = Some(status);
        self
    }

    pub(crate) fn with_palpo_live(mut self, live: bootstrap::palpo::Live) -> Self {
        self.palpo_live = Some(live);
        self
    }

    /// The console's Palpo import, saving into `state` without connecting
    /// (the transport is not enabled). For a host assembled without
    /// `Bootstrap`, such as the console test fixture.
    pub fn with_palpo_import(self, state: std::path::PathBuf) -> Self {
        let Some(domain) = self.domain.clone() else {
            return self;
        };
        let status = bootstrap::palpo::StatusHandle::new(false);
        let live =
            bootstrap::palpo::Live::new(state, self.store.clone(), domain, status.clone(), false);
        self.with_palpo(status).with_palpo_live(live)
    }

    pub(crate) fn palpo_live(&self) -> Option<&bootstrap::palpo::Live> {
        self.palpo_live.as_ref()
    }

    pub(crate) fn with_files(mut self, files: file_service::FileHandle) -> Self {
        self.files = Some(files);
        self
    }

    pub(crate) fn with_receive_service(mut self, receives: receive_service::ReceiveHandle) -> Self {
        self.receives = Some(receives);
        self
    }

    pub fn router(self) -> Router {
        Router::new()
            .hoop(self)
            .push(Router::with_path("health").get(health))
            .push(Router::with_path("ready").get(ready))
            .push(runner::router())
            .push(console::router())
            .push(
                Router::with_path("api/native/v1")
                    .hoop(authorize)
                    .push(Router::with_path("capabilities").get(capabilities))
                    .push(resources::router())
                    .push(fleet_views::router())
                    .push(usage::router())
                    .push(alerts::router())
                    .push(console::operator_router())
                    .push(runtime::router())
                    .push(operator::router())
                    .push(Router::with_path("custody").post(receive)),
            )
    }
}

/// The readiness vocabulary (brief 21, F3): ONE enum from which both the
/// wire words and the ready predicate derive — the word set and the arm
/// list can no longer encode two vocabularies. Every variant is enumerated
/// by `native_health_readiness_enumerates_every_state`. Public because the
/// integration test enumerates it against the predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    /// A writer channel is open (`domain_writer`, `custody_store`).
    Open,
    /// The sweep task is running (whether or not it has ever ticked).
    Alive,
    /// Never configured — unit wiring without `Bootstrap`. Ready by
    /// design: readiness never requires a component to exist.
    Disabled,
    /// A configured owner reported a healthy state word.
    Ready,
    /// The sweep is wired but has not ticked yet. Ready: readiness never
    /// requires a sweep to have RUN (F1's rule).
    Unstarted,
    /// A tick completed (any outcome, including refusals — see `Tick`).
    Tick(TickOutcome),
    /// An owner's settled running word.
    Running,
    /// A writer channel is closed (drained and exited). Not ready.
    Closed,
    /// The sweep task finished (cancelled or dead). Not ready.
    Stopped,
    /// An owner's settled failure word. Not ready.
    Unavailable,
    /// An owner's outcome-unknown word. Not ready.
    OutcomeUnknown,
    /// A wiring bug: a sweep handle exists but no tick channel does. Not
    /// ready — the honest word for an impossible configuration (F3).
    NotStarted,
    /// A live owner retrying or parked on a Matrix or component refusal
    /// (ADR-183): not serving, so not ready, and not a settled failure —
    /// the bridge never ends it; the fact changing or a human clears it.
    Refusing,
}

/// The sweep tick's outcome, decoupled from liveness (F1): a refused tick
/// is a LIVE loop that was refused, not a dead one, so the outcome NEVER
/// feeds the ready predicate — only the loop's liveness and the tick's age
/// do. The words stay on the wire for diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Swept,
    RefusedBusy,
    RefusedOutcomeUnknown,
    Refused,
}

impl ComponentState {
    /// The wire word. Names and state words only — never counts. Public for
    /// the enumeration test (the vocabulary IS the contract).
    pub fn word(self) -> &'static str {
        match self {
            ComponentState::Open => "open",
            ComponentState::Alive => "alive",
            ComponentState::Disabled => "disabled",
            ComponentState::Ready => "ready",
            ComponentState::Unstarted => "unstarted",
            ComponentState::Tick(TickOutcome::Swept) => "swept",
            ComponentState::Tick(TickOutcome::RefusedBusy) => "refused_busy",
            ComponentState::Tick(TickOutcome::RefusedOutcomeUnknown) => "refused_outcome_unknown",
            ComponentState::Tick(TickOutcome::Refused) => "refused",
            ComponentState::Running => "running",
            ComponentState::Closed => "closed",
            ComponentState::Stopped => "stopped",
            ComponentState::Unavailable => "unavailable",
            ComponentState::OutcomeUnknown => "outcome_unknown",
            ComponentState::NotStarted => "not_started",
            ComponentState::Refusing => "refusing",
        }
    }
    /// The ONE ready predicate (F1): a component is ready unless it is a
    /// settled not-serving state. A tick outcome — swept OR refused — is
    /// always ready: a refused tick is a live loop waiting for the next
    /// tick, exactly what the loop's own `tracing::warn!` promises, and a
    /// routine back-pressure refusal must never report the service down.
    /// Public for the enumeration test.
    pub fn is_ready(self) -> bool {
        !matches!(
            self,
            ComponentState::Closed
                | ComponentState::Stopped
                | ComponentState::Unavailable
                | ComponentState::OutcomeUnknown
                | ComponentState::NotStarted
                | ComponentState::Refusing
        )
    }
}

#[handler]
async fn health(depot: &mut Depot, res: &mut Response) {
    // Brief 21 (F2): /health KEEPS the retained contract — unauthenticated,
    // 200 whenever the process is live, readiness as BODY detail with names
    // and state words only (the stricter native body stays). The
    // 503-when-not-ready semantics live on /ready, added beside it for
    // uptime monitoring; no existing consumer changes. Both boundaries are
    // diagnostic, never authority: nothing may read either to retry,
    // release or complete anything.
    readiness(depot, res, false);
}

#[handler]
async fn ready(res: &mut Response, depot: &mut Depot) {
    readiness(depot, res, true);
}

/// The shared rollup (brief 19/21): every probe is SYNCHRONOUS — no writer
/// job, no lock, no allocation on the request path beyond the reply (the
/// `bounded_work_keeps_health_responsive` invariant). Readiness is
/// diagnostic, never authority.
fn readiness(depot: &mut Depot, res: &mut Response, refuse: bool) {
    let app = depot.get_typed::<App>().ok();
    let mut components: Vec<(&str, ComponentState)> = Vec::new();
    let domain = app.as_ref().and_then(|a| a.domain.as_ref());
    components.push((
        "domain_writer",
        match domain {
            None => ComponentState::Disabled,
            Some(store) if store.writer_open() => ComponentState::Open,
            Some(_) => ComponentState::Closed,
        },
    ));
    let custody = app.as_ref().map(|a| &a.store);
    components.push((
        "custody_store",
        if custody.is_some_and(|s| s.writer_open()) {
            ComponentState::Open
        } else {
            ComponentState::Closed
        },
    ));
    // The sweep: liveness is the task; the last tick's outcome is a
    // separate named component so a dead loop can never hide behind a good
    // outcome — and an outcome (swept, or any refusal waiting for the next
    // tick) can NEVER fail readiness (F1): the ready predicate for the
    // sweep is liveness alone, never the tick's outcome word.
    let sweep = app.as_ref().and_then(|a| a.ceiling_sweep.as_ref());
    let tick = app.as_ref().and_then(|a| a.sweep_tick.as_ref());
    let (sweep_state, last_tick) = match (sweep, tick) {
        (None, _) => (ComponentState::Disabled, None),
        (Some(handle), Some(receiver)) => {
            let last = match &*receiver.borrow() {
                bootstrap::CeilingSweepTick::Swept(_) => ComponentState::Tick(TickOutcome::Swept),
                bootstrap::CeilingSweepTick::Refused("unstarted") => ComponentState::Unstarted,
                bootstrap::CeilingSweepTick::Refused("busy") => {
                    ComponentState::Tick(TickOutcome::RefusedBusy)
                }
                bootstrap::CeilingSweepTick::Refused("outcome_unknown") => {
                    ComponentState::Tick(TickOutcome::RefusedOutcomeUnknown)
                }
                bootstrap::CeilingSweepTick::Refused(_) => {
                    ComponentState::Tick(TickOutcome::Refused)
                }
            };
            (
                if handle.is_finished() {
                    ComponentState::Stopped
                } else {
                    ComponentState::Alive
                },
                Some(last),
            )
        }
        // F3: a handle without its tick channel is a wiring bug — the honest
        // word is `not_started`, and it is NOT ready (it can never be
        // observed making progress).
        (Some(_), None) => (ComponentState::NotStarted, None),
    };
    components.push(("ceiling_sweep", sweep_state));
    components.push((
        "ceiling_sweep_last_tick",
        last_tick.unwrap_or(ComponentState::Disabled),
    ));
    // The retention sweep, the ceiling sweep's exact mirror (wiring review
    // b): liveness from the shared handle, the last tick's outcome word as
    // its own diagnostic component — and the outcome NEVER fails readiness
    // (F1's rule holds for both sweeps).
    let retention = app.as_ref().and_then(|a| a.retention_sweep.as_ref());
    let retention_tick = app.as_ref().and_then(|a| a.retention_tick.as_ref());
    let (retention_state, retention_last_tick) = match (retention, retention_tick) {
        (None, _) => (ComponentState::Disabled, None),
        (Some(handle), Some(receiver)) => {
            let last = match &*receiver.borrow() {
                bootstrap::RetentionSweepTick::Swept(_)
                | bootstrap::RetentionSweepTick::PeerSwept(_)
                | bootstrap::RetentionSweepTick::ExecutionSwept(_) => {
                    ComponentState::Tick(TickOutcome::Swept)
                }
                bootstrap::RetentionSweepTick::Refused("unstarted") => ComponentState::Unstarted,
                bootstrap::RetentionSweepTick::Refused("busy") => {
                    ComponentState::Tick(TickOutcome::RefusedBusy)
                }
                bootstrap::RetentionSweepTick::Refused("outcome_unknown") => {
                    ComponentState::Tick(TickOutcome::RefusedOutcomeUnknown)
                }
                bootstrap::RetentionSweepTick::Refused(_) => {
                    ComponentState::Tick(TickOutcome::Refused)
                }
            };
            (
                if handle.is_finished() {
                    ComponentState::Stopped
                } else {
                    ComponentState::Alive
                },
                Some(last),
            )
        }
        // F3's rule, the same shape: a handle without its tick channel is a
        // wiring bug — honest word `not_started`, never ready.
        (Some(_), None) => (ComponentState::NotStarted, None),
    };
    components.push(("retention_sweep", retention_state));
    components.push((
        "retention_sweep_last_tick",
        retention_last_tick.unwrap_or(ComponentState::Disabled),
    ));
    // Optional owners report their own state words; only the settled
    // failure words fail readiness. `stopped` fails too: a finished owner
    // is not serving, and 503 during shutdown is the honest answer (on
    // /ready; /health keeps 200).
    let owner_state = |configured: bool, state: &'static str| -> ComponentState {
        if !configured || state == "awaiting_import" {
            // No Palpo fleet imported yet: nothing to serve, not a failure.
            ComponentState::Disabled
        } else if matches!(
            state,
            "refresh_refused" | "awaiting_operator" | "approval_refused"
        ) {
            // ADR-183: a live owner retrying or parked on a Matrix or
            // component refusal. Not serving, so readiness refuses — and
            // not a settled failure either: the word says it is still there.
            ComponentState::Refusing
        } else if matches!(state, "unavailable" | "outcome_unknown" | "stopped") {
            match state {
                "unavailable" => ComponentState::Unavailable,
                "outcome_unknown" => ComponentState::OutcomeUnknown,
                _ => ComponentState::Stopped,
            }
        } else if state == "running" {
            ComponentState::Running
        } else {
            ComponentState::Ready
        }
    };
    if let Some(status) = app.as_ref().and_then(|a| a.development.as_ref()) {
        components.push(("development_driver", owner_state(true, status.state())));
    }
    if let Some(status) = app.as_ref().and_then(|a| a.palpo.as_ref()) {
        components.push(("palpo_transport", owner_state(true, status.state())));
    }
    if let Some(fleet) = app.as_ref().and_then(|a| a.fleet.as_ref()) {
        components.push((
            "factory_service",
            match fleet.state() {
                "running" => ComponentState::Running,
                "stopped" => ComponentState::Stopped,
                "outcome_unknown" => ComponentState::OutcomeUnknown,
                _ => ComponentState::NotStarted,
            },
        ));
    }
    let all_ready = components.iter().all(|(_, state)| state.is_ready());
    // TS /health rollup (task #46, backend-v2.js:7586-7629). The retained
    // native contract (`status`/`implementation`/`components`) stays; the TS
    // fields are added BESIDE it, each from real SYNCHRONOUS state — /health
    // never enqueues a writer job (the `bounded_work_keeps_health_responsive`
    // invariant), so nothing here reads the store. Native has no multi-server
    // heartbeat registry nor a request-path message counter; those are stated
    // as their honest native value, not fabricated.
    let fleet = app
        .as_ref()
        .and_then(|a| a.fleet.as_ref())
        .map(|f| f.snapshot());
    let (agents, online_agents, blocked_agents) = match &fleet {
        Some(snap) => {
            let registered = snap["registered_backends"].as_u64().unwrap_or(0) as usize;
            let rows = snap["agents"].as_array().cloned().unwrap_or_default();
            // A fleet agent is "online" while its worker is alive — not one
            // of the settled terminal/failure words. Parked/refusing words
            // (ADR-182/183) count as alive-and-blocked, exactly the split
            // the TS agentFlowHealth reports as `blocked`.
            let online = rows
                .iter()
                .filter(|r| {
                    !matches!(
                        r["status"]["state"].as_str(),
                        Some("closed" | "unavailable" | "outcome_unknown" | "stopped")
                    )
                })
                .count();
            let blocked = rows
                .iter()
                .filter(|r| {
                    matches!(
                        r["status"]["state"].as_str(),
                        Some(
                            "fenced" | "awaiting_operator" | "refresh_refused" | "approval_refused"
                        )
                    )
                })
                .count();
            (registered, online, blocked)
        }
        None => (0, 0, 0),
    };
    // Native talks to exactly one homeserver transport (palpo); there is no
    // multi-server registry, so `servers` is 1 when configured and
    // `onlineServers` is 1 when running.
    let palpo_state = app
        .as_ref()
        .and_then(|a| a.palpo.as_ref())
        .map(|p| p.state());
    let servers = usize::from(palpo_state.is_some());
    let online_servers = usize::from(palpo_state == Some("running"));
    // The TS rollup's `messages` is the in-process delivery queue length.
    // Native keeps that count in the bounded store, not on the request path;
    // reporting a synchronous 0 (and saying so) beats blocking /health on a
    // writer read to fetch a figure this boundary must not depend on.
    let messages = 0usize;
    let auth = serde_json::json!({
        "agentTokens": {
            "mode": "audit",
            "configuredMode": "audit",
            "behavior": "log-only",
            "managedAgentCount": agents,
            "loadedManagedAgentTokenCount": 0,
            "missingManagedAgentTokenCount": agents,
            "missingManagedAgentNames": [],
            "missingManagedAgentNamesTruncated": false,
            "failClosedReady": agents == 0,
        },
        "serverCredential": {
            "boundary": "operator-bearer",
            "behavior": "operator-bearer-required",
            "operatorBearerConfigured": true,
            "serverTokenConfigured": false,
            "serverTokenAccepted": false,
            "serverTokenEnforced": false,
            "serverOwnedRoutes": [],
            "operatorOwnedRoutes": [],
            "relayReadRoutes": [],
            "futureCredential": "HAGENCY_SERVER_TOKEN",
        },
    });
    // flow-health port (lib/backend/flow-health.js) over the same sync
    // signals: servers/agents from the fleet + palpo above, the other four
    // components stated as their honest native value (no sync alert/runtime
    // read exists at this boundary).
    let components_health = serde_json::json!({
        "servers": {
            "status": if servers == 0 { "unknown" }
                else if online_servers < servers { "degraded" }
                else { "healthy" },
            "total": servers,
            "online": online_servers,
            "offline": servers - online_servers,
            "maintenance": 0,
            "stale": 0,
        },
        "agents": {
            "status": if agents == 0 { "unknown" }
                else if online_agents == 0 { "unhealthy" }
                else if online_agents < agents || blocked_agents > 0 { "degraded" }
                else { "healthy" },
            "total": agents,
            "online": online_agents,
            "offline": agents.saturating_sub(online_agents),
            "blocked": blocked_agents,
        },
        "runtime": {
            "status": if agents == 0 { "unknown" }
                else if blocked_agents > 0 { "degraded" }
                else { "healthy" },
            "total": agents,
            "blocked": blocked_agents,
            "stale": 0,
            "staleAfterMs": 120000,
        },
        "alerts": {
            "status": "unknown",
            "actionable": {"total": 0, "critical": 0, "warning": 0,
                "byStatus": {"open": 0, "acknowledged": 0, "assigned": 0}},
        },
        "auth": {
            "status": "healthy",
            "agentTokenMode": "audit",
            "missingManagedAgentTokenCount": agents,
            "serverCredentialBoundary": "operator-bearer",
        },
        "deliveryEvents": {
            "status": "unknown",
            "enabled": false,
            "recentCount": 0,
            "lastEventAt": null,
            "recentTypes": {},
        },
    });
    // aggregateFlowHealthStatus: unhealthy wins, then degraded, then unknown
    // when no primary signals, else healthy. Primary signals = fleet or
    // palpo present.
    let health_status = if components_health["servers"]["status"] == "unhealthy"
        || components_health["agents"]["status"] == "unhealthy"
        || components_health["runtime"]["status"] == "unhealthy"
    {
        "unhealthy"
    } else if components_health["servers"]["status"] == "degraded"
        || components_health["agents"]["status"] == "degraded"
        || components_health["runtime"]["status"] == "degraded"
    {
        "degraded"
    } else if servers == 0 && agents == 0 {
        "unknown"
    } else {
        "healthy"
    };
    let health_reasons: Vec<String> = components_health
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, v)| matches!(v["status"].as_str(), Some("unhealthy" | "degraded")))
                .map(|(name, v)| format!("{name}:{}", v["status"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    let value = serde_json::json!({
        "status": if all_ready { "ok" } else { "unavailable" },
        "implementation": "rust",
        "components": components
            .iter()
            .map(|(name, state)| serde_json::json!({"name": name, "state": state.word()}))
            .collect::<Vec<_>>(),
        // ── TS /health rollup fields (task #46) ──
        "agents": agents,
        "onlineAgents": online_agents,
        "servers": servers,
        "onlineServers": online_servers,
        "messages": messages,
        "auth": auth,
        "health": serde_json::json!({
            "status": health_status,
            "generatedAt": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok())
                .unwrap_or_default(),
            "components": components_health,
            "reasons": health_reasons,
        }),
    });
    // F2: /health is ALWAYS 200 while the process is live (the retained
    // contract); /ready is the 503-when-not-ready boundary. Never a silent
    // 200 on /ready.
    if refuse && !all_ready {
        res.status_code(StatusCode::SERVICE_UNAVAILABLE);
    }
    res.render(Json(value));
}

#[handler]
async fn capabilities(depot: &mut Depot, res: &mut Response) {
    let management = depot
        .get_typed::<App>()
        .is_ok_and(|app| app.domain.is_some());
    let mut value = serde_json::json!({"custody":true, "agent_execution":false, "palpo_transport":false,
        "matrix_crypto":false, "resource_management":management, "runner_task_api":management, "usage_observations_read":management, "project_request_transport":false, "production_api_parity":false});
    if let Ok(app) = depot.get_typed::<App>()
        && let Some(fleet) = &app.fleet
    {
        value["factory_service"] = fleet.snapshot();
    }
    if let Ok(app) = depot.get_typed::<App>()
        && let Some(status) = &app.development
    {
        value["development_execution"] =
            serde_json::to_value(status.get()).expect("fixed status serializes");
    }
    if let Ok(app) = depot.get_typed::<App>()
        && let Some(status) = &app.palpo
    {
        value["palpo_publication"] =
            serde_json::to_value(status.get()).expect("fixed status serializes");
    }
    res.render(Json(value));
}

/// The human text paired with every refusal code (board #60 item 1).
///
/// The retained server always answers `{error: <message>, code}` — never a bare
/// token (`backend-v2.js:14953-14958` for engagements, `:10032` for the
/// registration body) — so an operator reading the JSON, a log, or the console
/// gets a sentence rather than `over_commit`. Codes with an exact retained
/// wording carry it; the rest say in one line what the code means. The console
/// localises from `code` and falls back to this text.
fn refusal_message(code: &str) -> &'static str {
    match code {
        // Authentication and authority.
        "operator_auth_required" => "the operator token is required",
        "local_authority_required" => "the request must reach this service directly",
        "console_origin_required" => "the request must originate from this console",
        "console_access_required" => "console access is required or has expired",
        "runner_auth_required" => "the runner token is required",
        "acl_unconfigured" => "no access control list is configured",
        "admin_required" => "an administrator credential is required",
        "operator_required" => "an operator credential is required",
        // Scopes.
        "task_scope_required" => "task management scope is required",
        "resource_publication_scope_required" => {
            "resource publication management scope is required"
        }
        "resource_configuration_scope_required" => {
            "resource configuration management scope is required"
        }
        "account_scope_required" => "account enrollment management scope is required",
        "agent_lifecycle_scope_required" => "agent lifecycle management scope is required",
        // Request bodies.
        "json_required" => "a JSON request body is required",
        "body_rejected" => "the request body was rejected",
        "body_timeout" => "the request body was not received in time",
        "body_too_large" => "the request body exceeds the limit",
        // Invalid input, by surface.
        "invalid" => "the request is invalid",
        "invalid_console_request" => "the console request is invalid",
        "invalid_usage_query" => "the usage query is invalid",
        "invalid_roster_query" => "the roster query is invalid",
        "invalid_alerts_query" => "the alerts query is invalid",
        "invalid_approvals_query" => "the approvals query is invalid",
        "invalid_bindings_query" => "the approval-bindings query is invalid",
        "invalid_agent_update" => "the agent update body is invalid",
        "invalid_side_query" => "the project-sides query is invalid",
        "invalid_stream_query" => "the stream query is invalid",
        "invalid_engagement_id" => "the engagement id is invalid",
        "invalid_resource_command" => "the resource command is invalid",
        "invalid_account_command" => "the account command is invalid",
        "invalid_registration_body" => "the registration body is invalid",
        "invalid_domain_command" => "the domain command is invalid",
        "invalid_task_operation" => "the task operation is invalid",
        "invalid_delivery" => "the delivery is invalid",
        "invalid_alert_transition" => "the alert transition is invalid",
        // Reads that could not be answered.
        "not_found" => "the requested record was not found",
        "project_side_not_found" => "project side not found",
        "approval_binding_not_found" => "approval binding not found",
        "engagement_unavailable" => "the engagement could not be read",
        "roster_unavailable" => "the agent roster could not be read",
        "usage_unavailable" => "the usage observation is unavailable",
        "alerts_unavailable" => "the alerts observation is unavailable",
        "alerts_corrupt" => "the alert store is corrupt",
        "approvals_unavailable" => "the approvals observation is unavailable",
        "bindings_unavailable" => "the approval-bindings observation is unavailable",
        "sides_unavailable" => "the project-sides observation is unavailable",
        "stream_unavailable" => "the change stream is unavailable",
        "registration_unavailable" => "the registration is unavailable",
        "palpo_fleet_conflict" => "another Palpo fleet is already connected to this Hagency",
        "palpo_import_unavailable" => {
            "the Palpo configuration could not be saved or connected; retry"
        }
        // Service state.
        "console_unavailable" => "the native console is unavailable",
        "native_unavailable" => "the native API is unavailable",
        "unavailable" => "the service is unavailable",
        "domain_unavailable" => "the domain store is unavailable",
        "busy" => "the service is busy; retry",
        "console_busy" => "the console is at capacity; retry",
        "outcome_unknown" => "the outcome is unknown",
        "clock_unavailable" => "the clock is unavailable",
        // Allocation.
        "over_commit" => "the request exceeds the remaining allocation",
        "no_ceiling" => "no remaining allocation is available",
        "insufficient_capacity" => "there is not enough capacity",
        // Lifecycle and concurrency conflicts.
        "engagement_not_pending" => "engagement is no longer pending",
        "engagement_not_live" => "the engagement is no longer live",
        "command_conflict" => "a different command is already in progress",
        "decision_conflict" => "the decision conflicts with the recorded one",
        "resource_in_use" => "the resource is in use",
        "resource_revision_conflict" => "the resource was changed by another writer",
        "account_revision_conflict" => "the account was changed by another writer",
        "account_state_conflict" => "the account is not in a state that allows this",
        "continuation_conflict" => "the dispatch cannot be continued",
        "recovery_conflict" => "the dispatch cannot be recovered",
        "resolution_conflict" => "the dispatch was already resolved",
        "stale_generation" => "the registration generation is stale",
        "registration_generation" => "the registration generation does not match",
        "bad_transition" => "the transition is not allowed",
        "dispatch_not_continuable" => "the dispatch cannot be continued",
        "dispatch_not_recoverable" => "the dispatch cannot be recovered",
        "dispatch_not_resolvable" => "the dispatch cannot be resolved",
        // Agents and roles.
        "agent_unavailable" => "the agent is unavailable",
        "agent_preset_unavailable" => "the agent preset is unavailable",
        "agent_start_unavailable" => "the agent cannot be started",
        "agent_record_not_writable" => {
            "the agent record is not writable from an agent-authenticated route"
        }
        "project_side_not_settable_here" => {
            "projectSide cannot be set here: this route is agent-authenticated; use PUT /api/agents/:name/project-side with the operator token"
        }
        "role_required" => "a role is required",
        "resource_required" => "a resource is required",
        "roles_are_model_derived" => "roles are derived from the model",
        _ => "the request was refused",
    }
}

fn refusal(res: &mut Response, status: StatusCode, code: &str) {
    res.status_code(status);
    res.render(Json(
        serde_json::json!({"ok":false,"code":code,"error":refusal_message(code)}),
    ));
}

/// A refusal whose store verdict carries its own human explanation (ADR-186
/// §A2: the over-commit message naming the binding limit). The code and the
/// generic `error` sentence stay exactly as `refusal` serves them; `message`
/// is added beside them, never in place of the code.
fn refusal_explained(res: &mut Response, status: StatusCode, code: &str, message: &str) {
    res.status_code(status);
    res.render(Json(serde_json::json!({
        "ok": false,
        "code": code,
        "error": refusal_message(code),
        "message": message,
    })));
}

fn local_authority(req: &Request, depot: &Depot, res: &mut Response) -> bool {
    res.headers_mut()
        .insert("cache-control", "no-store".parse().expect("static header"));
    let Ok(app) = depot.get_typed::<App>() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        return false;
    };
    let headers = req.headers();
    let forbidden = headers.get_all("host").iter().count() != 1
        || headers.contains_key("origin")
        || headers.contains_key("sec-fetch-site")
        || headers.contains_key("forwarded")
        || headers.contains_key("x-forwarded-for")
        || headers.get("host").and_then(|v| v.to_str().ok()) != Some(app.authority.as_str());
    if forbidden {
        refusal(res, StatusCode::FORBIDDEN, "local_authority_required");
        return false;
    }
    true
}

#[handler]
async fn authorize(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    if !local_authority(req, depot, res) {
        ctrl.skip_rest();
        return;
    }
    let Ok(app) = depot.get_typed::<App>() else {
        ctrl.skip_rest();
        return;
    };
    let headers = req.headers();
    let bearer = if headers.get_all("authorization").iter().count() == 1 {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
    } else {
        None
    };
    let valid = bearer.filter(|s| s.len() <= 256).is_some_and(|s| {
        let hash: [u8; 32] = Sha256::digest(s.as_bytes()).into();
        bool::from(hash.ct_eq(&app.token_hash))
    });
    if !valid {
        refusal(res, StatusCode::UNAUTHORIZED, "operator_auth_required");
        ctrl.skip_rest();
    }
}

#[handler]
async fn receive(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Ok(app) = depot.get_typed::<App>() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        return;
    };
    let Ok(_permit) = app.requests.clone().try_acquire_owned() else {
        refusal(res, StatusCode::SERVICE_UNAVAILABLE, "busy");
        return;
    };
    if req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or_default().trim())
        != Some("application/json")
    {
        refusal(res, StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
        return;
    }
    if req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > MAX_DELIVERY_BYTES)
    {
        refusal(res, StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
        return;
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(2),
        req.payload_with_max_size(MAX_DELIVERY_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            refusal(res, StatusCode::PAYLOAD_TOO_LARGE, "body_rejected");
            return;
        }
        Err(_) => {
            refusal(res, StatusCode::REQUEST_TIMEOUT, "body_timeout");
            return;
        }
    };
    let delivery: Delivery = match serde_json::from_slice(bytes) {
        Ok(delivery) => delivery,
        Err(_) => {
            refusal(res, StatusCode::BAD_REQUEST, "invalid_delivery");
            return;
        }
    };
    let now = match SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
    {
        Some(now) => now,
        None => {
            refusal(res, StatusCode::SERVICE_UNAVAILABLE, "clock_unavailable");
            return;
        }
    };
    match app.store.receive(delivery, now).await {
        Ok(receipt) => {
            res.status_code(StatusCode::ACCEPTED);
            res.render(Json(receipt));
        }
        Err(error) => {
            let (status, code) = match error {
                Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_delivery"),
                Error::Conflict => (StatusCode::CONFLICT, "idempotency_conflict"),
                Error::Generation => (StatusCode::CONFLICT, "generation_mismatch"),
                Error::Capacity => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "custody_capacity_exhausted",
                ),
                Error::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
                Error::OutcomeUnknown => (StatusCode::GATEWAY_TIMEOUT, "outcome_unknown"),
                _ => (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            };
            refusal(res, status, code);
        }
    }
}

#[handler]
impl App {
    async fn handle(&self, depot: &mut Depot) {
        depot.insert_typed(self.clone());
    }
}
