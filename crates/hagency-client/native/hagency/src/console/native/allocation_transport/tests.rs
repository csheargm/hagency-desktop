use super::*;
#[path = "../resource_alert_tests.rs"]
mod resource_alerts;
#[path = "../approval_binding_tests.rs"]
mod approval_bindings;
#[path = "../tool_approval_tests.rs"]
mod tool_approvals;
#[path = "../execution_policy_tests.rs"]
mod execution_policy;
#[path = "../account_retirement_tests.rs"]
mod account_retirement;
#[path = "../account_preparation_tests.rs"]
mod account_preparation;
#[cfg(unix)]
#[path = "../account_login_tests.rs"]
mod account_login;
#[path = "../management_server/agents_http_tests.rs"]
mod admission;
#[path = "../management_server/agent_settlement_http_tests.rs"]
mod human_settlement;
#[path = "../managed_agent_transport/tests.rs"]
mod managed_agents;
#[path = "../management_scope_http_tests.rs"]
mod management_scopes;
#[cfg(feature="shared-intake")]
#[path="../management_server/agent_run_control_tests.rs"]
mod run_control;
#[path = "../shutdown_tests.rs"]
mod owner_shutdown;
use crate::native_owner::{ManagementPackage, MatrixAccessToken, MatrixTokenSource, NativeOwner};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
struct Source(Arc<AtomicBool>);
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            if !self.0.load(Ordering::Acquire) {
                return Err(failure(401, "matrix_account_changed"));
            }
            Ok(MatrixAccessToken {
                access_token: "fixture-sdk".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}
// Only generation-change fixtures represent a new explicit SDK sign-in.
// Ordinary owner restoration uses Source, whose default permission is false.
struct ExplicitSignInSource(Source, AtomicBool);
impl MatrixTokenSource for ExplicitSignInSource {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        self.0.access_token()
    }
    fn permits_explicit_reenrollment(&self) -> bool {
        self.1.load(Ordering::Acquire)
    }
    fn consume_explicit_reenrollment(&self) -> bool {
        self.1.swap(false, Ordering::AcqRel)
    }
}
async fn explicit_new_generation(f: &Fixture, generation: u64) {
    let owner = &f.owner;
    let login = &owner.0.host.console.0.server_login;
    login.revoke_sdk_resume().unwrap();
    login.revoke_native_all().await;
    login.flush_native_revocations().await;
    f.state.lock().unwrap().generation = generation;
    let cookie = login
        .authorize_matrix(
            &owner.0.host.console,
            &owner.0.expected_origin,
            &owner.0.expected_owner,
            Arc::new(ExplicitSignInSource(
                Source(f.current.clone()),
                AtomicBool::new(true),
            )),
        )
        .await
        .unwrap();
    *owner.0.cookie.lock().await = format!("hagency_console={cookie}");
    assert_eq!(
        login.authorized_device().await.unwrap().generation(),
        generation
    );
}
#[derive(Default)]
struct State {
    settlement: managed_agents::settlements::SettlementState,
    human_settlement: human_settlement::SettlementState,
    #[cfg(feature="shared-intake")]
    control: run_control::ControlState,
    missing_demand_capability: bool,
    #[cfg(all(feature="agent-budget", feature="shared-execution"))]
    budget: managed_agents::budgets::BudgetState,
    #[cfg(feature = "shared-execution")]
    shared: managed_agents::shared::State,
    admission: admission::State,
    agent: Option<Value>,
    agent_receipts: BTreeMap<String, Value>,
    agent_writes: usize,
    agent_assignment: Option<Value>,
    matrix_challenge: Option<Value>,
    matrix_observation: Option<Value>,
    matrix_writes: usize,
    runtime_command: Option<ManagedRuntimeCommand>,
    runtime_receipts: BTreeMap<String, ManagedRuntimeReceipt>,
    runtime_writes: usize,
    grant: Option<NativeGrantReceipt>,
    allocation: Option<AllocationCommand>,
    parent: Option<GrantClosureCommand>,
    allocation_receipt: Option<NativeAllocationReceipt>,
    parent_receipt: Option<NativeGrantSettlementReceipt>,
    publications: BTreeMap<String, NativeGrantReceipt>,
    publication_writes: usize,
    result_writes: usize,
    lose_publication: bool,
    lose_result: bool,
    historical_executable: bool,
    tamper_digest: bool,
    expired_ticket: bool,
    wrong_ack: bool,
    revoked_grant: bool,
    short_ticket: bool,
    delay_result: bool,
    delay_contribution_until: Option<Instant>,
    #[cfg(feature="shared-intake")]
    unavailable_contribution: bool,
    generation: u64,
    source: Option<Arc<AtomicBool>>,
    invalidate_at_contribution: bool,
    invalidate_at_result: bool,
    lose_before_publication: bool,
    reject_publication: Option<&'static str>,
    changed_coordinator: bool,
    owner_coordinator: bool,
}
fn admission(g: &NativeGrantReceipt) -> Value {
    let at = now();
    json!({"admission":{"contribution":{"id":g.contribution_id,"ownerMxid":g.owner_mxid,"executorDeviceId":g.executor_device_id,"executorGeneration":g.executor_generation,"resourceId":g.resource_id,"resourceRevision":g.resource_revision,"label":"Fixture","runtime":"codex","capabilities":["coding"],"coordinatorMxid":g.coordinator_mxid,"state":"admitted","revision":1,"requestId":"admission_request","createdAtMs":at,"updatedAtMs":at,"verification":null},"request":{"id":"admission_request","kind":"resource_admission","contributionId":g.contribution_id,"projectId":null,"allocationId":null,"projectName":null,"resourceLabel":null,"requesterMxid":g.owner_mxid,"approverMxid":"@admin:test","state":"approved","revision":2,"createdAtMs":at,"expiresAtMs":at+60000,"decidedAtMs":at,"canDecide":false}},"observedAtMs":at})
}
fn grant_view(g: &NativeGrantReceipt) -> Value {
    json!({"id":g.grant_id,"receipt":g,"receiptDigest":digest(g).unwrap(),"state":"active","revision":1,"observedAtMs":now()})
}
fn allocation_view(c: &AllocationCommand, r: &NativeAllocationReceipt) -> Value {
    json!({"id":c.allocation_id,"projectId":c.project_id,"projectRevision":c.project_revision,"grantId":c.grant_receipt.grant_id,"requestedTokens":c.requested_tokens,"state":r.state,"revision":2,"requestId":c.request_id,"requesterMxid":c.manager_mxid,"coordinatorMxid":c.decision_actor,"periodKey":r.period_key,"expiresAtMs":c.expires_at_ms,"canDecide":false,"canRelease":true,"blockingReason":null,"result":r,"balance":r})
}
fn settlement_view(g: &NativeGrantReceipt, r: &NativeGrantSettlementReceipt) -> Value {
    json!({"grantId":g.grant_id,"grantRevision":2,"revision":1,"state":r.state,"originalGrantedTokens":g.allocated_tokens,"heldTokens":r.retained_tokens,"knownConsumedTokens":r.known_consumed_tokens,"unresolvedTokens":r.unresolved_tokens,"knownUnusedReleasedTokens":g.allocated_tokens.saturating_sub(r.retained_tokens),"overrunTokens":r.retained_tokens.saturating_sub(g.allocated_tokens),"unreconciledChildTokens":0,"canClose":false,"canSettle":true,"receipt":r,"receiptDigest":digest(r).unwrap()})
}
// None deliberately closes the real HTTP connection after the simulated
// central commit. Tests then recover through the production pinned adapter.
fn management(path: &str, body: &Value, state: &mut State) -> Option<Value> {
    if path.ends_with("/agent-requests/human_request/settlement") {return human_settlement::management(body,state);}
    if path.contains("/agent-settlement-executor/") { return managed_agents::settlements::management(path,body,state); }
    #[cfg(all(feature="agent-budget", feature="shared-execution"))]
    if path.contains("/agent-budget-executor/") { return managed_agents::budgets::management(path,body,state); }
    #[cfg(feature="shared-intake")]
    if path.ends_with("/management/session") || path.contains("/assignments/assignment_control/control") || path.ends_with("/control/readiness") {return run_control::management(path,body,state);}
    #[cfg(feature = "shared-execution")]
    if path.contains("/shared-execution/") {
        return managed_agents::shared::management(path, body, state);
    }
    if path.contains("/allocations/")
        || path.contains("/allocation-requests/")
        || path.ends_with("/inbox")
        || path.contains("/allocation-commands/")
            && body["operation"]
                .as_str()
                .is_some_and(|s| s.starts_with("allocation."))
    {
        return admission::management(path, body, state);
    }
    if path.ends_with("/agent-removals/list")
        || path.ends_with("/agent-work/list")
        || path.contains("/agent-requests/")
        || path.contains("/agent-topups/")
        || path.contains("/agent-topup-commands/")
        || path.contains("/agent-commands/")
        || path.ends_with("/agent-requests")
    {
        return admission::management(path, body, state);
    }
    if path.contains("/agent-executor/")
        || path.contains("/agent-matrix/")
        || path.contains("/agent-runtime/")
    {
        return managed_agents::management(path, body, state);
    }
    let at = now();
    if path.ends_with("/contributions/con_fixture") {
        #[cfg(feature="shared-intake")]
        if state.unavailable_contribution { return Some(json!({"fixtureStatus":503,"error":"contribution_unavailable"})); }
        if state.invalidate_at_contribution {
            state
                .source
                .as_ref()
                .unwrap()
                .store(false, Ordering::Release);
        }
        let mut result = admission(state.grant.as_ref().unwrap());
        result["admission"]["contribution"]["executorGeneration"] = json!(state.generation.max(1));
        if state.changed_coordinator {
            result["admission"]["contribution"]["coordinatorMxid"] = json!("@new-coordinator:test");
        }
        if state.owner_coordinator {
            result["admission"]["contribution"]["coordinatorMxid"] = json!(state.grant.as_ref().unwrap().owner_mxid);
        }
        return Some(result);
    }
    if path.ends_with("/contributions/con_fixture/grants") {
        if std::mem::take(&mut state.lose_before_publication) {
            return None;
        }
        if let Some(code) = state.reject_publication {
            return Some(json!({"code":code}));
        }
        let input: Publication = serde_json::from_value(body.clone()).unwrap();
        if let Some(old) = state.publications.get(&input.command_id) {
            assert_eq!(digest(old).unwrap(), digest(&input.receipt).unwrap());
        } else {
            state.publication_writes += 1;
            state
                .publications
                .insert(input.command_id, input.receipt.clone());
        }
        if std::mem::take(&mut state.lose_publication) {
            return None;
        }
        return Some(json!({"grant":grant_view(&input.receipt),"observedAtMs":at}));
    }
    if path.contains("/allocation-commands/") && path.ends_with("/recover") {
        let id = path.split('/').rev().nth(1).unwrap();
        return Some(match state.publications.get(id) {
            Some(g) => {
                let mut grant = grant_view(g);
                if state.revoked_grant {
                    grant["state"] = json!("revoked");
                    grant["revision"] = json!(2);
                }
                json!({"recorded":true,"grant":grant,"observedAtMs":at})
            }
            None => json!({"recorded":false,"observedAtMs":at}),
        });
    }
    let parent = path.contains("/grant-executor-commands/");
    if path.ends_with("/poll") || path.ends_with("/recover") {
        let historical = path.ends_with("/recover");
        let (command, recorded) = if parent {
            (
                serde_json::to_value(&state.parent).unwrap(),
                serde_json::to_value(&state.parent_receipt).unwrap(),
            )
        } else {
            (
                serde_json::to_value(&state.allocation).unwrap(),
                serde_json::to_value(&state.allocation_receipt).unwrap(),
            )
        };
        if command.is_null() {
            return Some(json!({"delivery":null,"observedAtMs":at}));
        }
        let hash = if state.tamper_digest {
            "f".repeat(64)
        } else {
            digest(&command).unwrap()
        };
        let allowed = if historical {
            state.historical_executable
        } else {
            recorded.is_null()
        };
        return Some(
            json!({"delivery":{"command":command,"commandDigest":hash,"token":"c".repeat(64),"leaseUntilMs":if state.expired_ticket {at}else if state.short_ticket {at+1000}else{at+30000},"executionAllowed":allowed,"recorded":recorded},"observedAtMs":at}),
        );
    }
    if path.ends_with("/result") {
        assert_eq!(body["token"], "c".repeat(64));
        if state.invalidate_at_result {
            state
                .source
                .as_ref()
                .unwrap()
                .store(false, Ordering::Release);
        }
        state.result_writes += 1;
        let ack = if parent {
            let receipt: NativeGrantSettlementReceipt =
                serde_json::from_value(body["receipt"].clone()).unwrap();
            state.parent_receipt = Some(receipt.clone());
            json!({"commandId":receipt.command_id,"commandDigest":body["commandDigest"],"receiptDigest":digest(&receipt).unwrap(),"settlement":settlement_view(state.grant.as_ref().unwrap(),&receipt),"observedAtMs":at})
        } else {
            let receipt: NativeAllocationReceipt =
                serde_json::from_value(body["receipt"].clone()).unwrap();
            state.allocation_receipt = Some(receipt.clone());
            json!({"commandId":receipt.command_id,"commandDigest":body["commandDigest"],"receiptDigest":if state.wrong_ack {"f".repeat(64)} else {digest(&receipt).unwrap()},"allocation":allocation_view(state.allocation.as_ref().unwrap(),&receipt),"observedAtMs":at})
        };
        if std::mem::take(&mut state.lose_result) {
            return None;
        }
        return Some(ack);
    }
    panic!("unexpected fixed management route");
}
async fn server(state: Arc<Mutex<State>>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let server_origin = origin.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            let origin = server_origin.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut chunk = [0_u8; 4096];
                let split = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    assert!(raw.len() < 128 * 1024);
                    if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&raw[..split]).to_string();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while raw.len() < split + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    assert!(raw.len() < 128 * 1024);
                }
                let path = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body = serde_json::from_slice::<Value>(&raw[split..split + length])
                    .unwrap_or(Value::Null);
                let at = now();
                let issuer = format!("{origin}auth/");
                let value = if path.starts_with("/hagency/api/v1/management/") {
                    assert!(headers.lines().any(|line| {
                        line.split_once(':').is_some_and(|(k, v)| {
                            k.eq_ignore_ascii_case("authorization")
                                && v.trim() == format!("Bearer {}", "b".repeat(64))
                        })
                    }));
                    management(path, &body, &mut state.lock().unwrap())
                } else {
                    Some(match path {
                        "/hagency/api/v1/discovery" => {
                            let mut caps = vec![
                                "pasion-oauth",
                                "owner-agent-appservice-v1",
                                "global-agent-identity-v2",
                                "execution-device-v1",
                                "owner-direct-v1",
                            ];
                            #[cfg(all(feature="agent-budget", feature="shared-execution"))]
                            if !state.lock().unwrap().budget.missing_capability { caps.push("managed-agent-budget-delta-v1"); }
                            {
                                let s=state.lock().unwrap();
                                if s.admission.budget.enabled && !caps.contains(&"managed-agent-budget-delta-v1") { caps.push("managed-agent-budget-delta-v1"); }
                                if s.admission.budget.invalidate_discovery {s.source.as_ref().unwrap().store(false,Ordering::Release);}
                            }
                            if !state.lock().unwrap().settlement.missing_capability { caps.push("agent-settlement-v1"); }
                            if !state.lock().unwrap().missing_demand_capability {
                                caps.push("shared-durable-demand-v1");
                            }
                            #[cfg(feature = "shared-execution")]
                            if managed_agents::shared::claude_enabled(&state.lock().unwrap()) {
                                caps.push("shared-claude-execution-v1");
                            }
                            #[cfg(feature = "shared-execution")]
                            if managed_agents::shared::demand_retry_enabled(&state.lock().unwrap()) {
                                caps.push("shared-demand-retry-v1");
                            }
                            #[cfg(feature="shared-budget-execution")]
                            caps.push("shared-budget-execution-v1");
                            #[cfg(feature = "shared-execution")]
                            if managed_agents::shared::context_v3_enabled(&state.lock().unwrap()){caps.push("shared-native-context-v3");}
                            json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":caps,"homeserver":origin,"issuer":issuer})
                        }
                        "/hagency/api/v1/sessions/pasion" => {
                            json!({"token":"a".repeat(64),"userId":"uid","mxid":"@owner:test","validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/identity" => {
                            json!({"userId":"uid","mxid":"@owner:test","issuer":issuer,"subject":"sub","clientId":"sdk-client","validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/devices/current" => {
                            json!({"deviceId":"device","userId":"uid","mxid":"@owner:test","issuer":issuer,"subject":"sub","clientId":"sdk-client","generation":state.lock().unwrap().generation.max(1),"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/devices" => {
                            json!({"deviceId":"device","token":"b".repeat(64),"generation":state.lock().unwrap().generation.max(1),"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/sessions/current/renew" => {
                            json!({"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/sessions/current" => json!({}),
                        _ => panic!("unexpected authentication route"),
                    })
                };
                #[cfg(feature = "shared-execution")]
                let idle_delay = managed_agents::shared::idle_boundary_delay(
                    path,
                    &state.lock().unwrap().shared,
                );
                #[cfg(feature = "shared-execution")]
                if let Some(delay) = idle_delay {
                    tokio::time::sleep(delay).await;
                }
                let contribution_delay = if path.ends_with("/contributions/con_fixture") {
                    state.lock().unwrap().delay_contribution_until
                } else {
                    None
                };
                if let Some(until) = contribution_delay {
                    tokio::time::sleep_until(until.into()).await;
                }
                let delay = path.ends_with("/result") && state.lock().unwrap().delay_result;
                if delay {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                if let Some(value) = value {
                    let status =
                        if let Some(status) = value.get("fixtureStatus").and_then(Value::as_u64) {
                            status
                        } else if value.get("code").is_some() {
                            409
                        } else {
                            200
                        };
                    let body = value.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    });
    (origin, task)
}

#[tokio::test]
async fn native_allocation_transport_late_upload_keeps_receipt_and_permission_blocks_poll() {
    let f = Fixture::new().await;
    let command = f.allocation();
    {
        let mut s = f.state.lock().unwrap();
        s.allocation = Some(command.clone());
        s.short_ticket = true;
        s.delay_result = true;
    }
    let error = f
        .transport
        .poll_allocation(&f.registry, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, "allocation_ticket_expired");
    assert_eq!(f.state.lock().unwrap().result_writes, 1);
    {
        let mut s = f.state.lock().unwrap();
        s.short_ticket = false;
        s.delay_result = false;
    }
    assert!(matches!(
        f.transport
            .recover_allocation(
                &f.registry,
                command.command_id.clone(),
                digest(&command).unwrap()
            )
            .await
            .unwrap(),
        AllocationProgress::Recorded { .. }
    ));
    assert_eq!(f.state.lock().unwrap().result_writes, 1);
    let mut denied = package(2);
    denied.permissions = vec![Permission::ReadIntents];
    let denied = AllocationTransport::new(f.owner.management(denied).await.unwrap());
    assert!(denied.poll_allocation(&f.registry, None).await.is_err());
    assert_eq!(f.state.lock().unwrap().result_writes, 1);
    f.stop().await;
}
struct Fixture {
    _root: tempfile::TempDir,
    owner: NativeOwner,
    transport: AllocationTransport,
    registry: OwnerResourceRegistry,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
    current: Arc<AtomicBool>,
}
fn package(instance: u64) -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
            Permission::PublishGrants,
            Permission::ExecuteAllocations,
            Permission::ExecuteManagedAgents,
            Permission::SettleGrants,
        ],
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_provider("https://provider.test", "synthetic-provider-subject").await
    }
    async fn with_provider(provider: &str, subject: &str) -> Self {
        Self::with_provider_budget(provider, subject, 1000, 700).await
    }
    async fn with_provider_budget(
        provider: &str,
        subject: &str,
        total: u64,
        grant_tokens: u64,
    ) -> Self {
        Self::with_provider_budget_lifetime(provider, subject, total, grant_tokens, 60000).await
    }
    async fn with_provider_budget_lifetime(
        provider: &str, subject: &str, total: u64, grant_tokens: u64, lifetime_ms: u64,
    ) -> Self {
        Self::with_provider_budget_calendar(provider, subject, total, grant_tokens, lifetime_ms, None).await
    }
    async fn with_provider_budget_calendar(
        provider: &str, subject: &str, total: u64, grant_tokens: u64, lifetime_ms: u64,
        calendar: Option<hagency_core::recurring_quota::Calendar>,
    ) -> Self {
        Self::with_provider_budget_limits(provider, subject, total, grant_tokens, lifetime_ms, calendar, None).await
    }
    async fn with_provider_budget_limits(
        provider: &str, subject: &str, total: u64, grant_tokens: u64, lifetime_ms: u64,
        calendar: Option<hagency_core::recurring_quota::Calendar>, per_request_tokens: Option<u64>,
    ) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (origin, task) = server(state.clone()).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let current = Arc::new(AtomicBool::new(true));
        state.lock().unwrap().source = Some(current.clone());
        let owner = NativeOwner::open_with_matrix(
            &root.path().join("host"),
            &origin,
            "@owner:test",
            Arc::new(Source(current.clone())),
        )
        .await
        .unwrap();
        let transport = AllocationTransport::new(owner.management(package(1)).await.unwrap());
        let registry =
            OwnerResourceRegistry::isolated_for_tests(&root.path().join("registry")).unwrap();
        let scope = transport.scope(Permission::PublishGrants).await.unwrap();
        // Use the same canonical initializer as a fresh current account. The
        // fixture never constructs a writable profile-domain management session.
        let mut setup_package=package(90);
        setup_package.permissions.push(Permission::ReadResources);
        let setup=owner.management(setup_package).await.unwrap();
        let journal=setup.current_setup_journal().await.unwrap();
        let profile=crate::console::owned_agents::profile_identity(scope.device.origin(),scope.device.issuer(),scope.device.subject(),scope.device.owner_mxid()).unwrap();
        let authority=hagency_store::owner_resources::adoption::ProfileInitializationAuthority::new(registry.clone(),scope.profile.clone(),scope.deadline,||true).unwrap();
        registry.initialize_current_profile(authority,"current_fixture".into(),journal).await.unwrap();
        let selection=super::super::CurrentProfileSelection::completed(&owner,registry.clone(),"current_fixture".into(),profile,scope.device.device_id().into());
        selection.persist(&owner).unwrap();
        drop(scope);drop(setup);drop(transport);
        owner.suspend().await.unwrap();drop(owner);
        let owner=selection.open_with_matrix_guarded(&root.path().join("host"),&origin,"@owner:test",Arc::new(Source(current.clone())),|_|Ok(())).await.unwrap();
        let transport=AllocationTransport::new(owner.management(package(1)).await.unwrap());
        let scope=transport.scope(Permission::PublishGrants).await.unwrap();
        let claude = provider == hagency_runtime::claude::account::PROVIDER;
        let resource = registry
            .prepare_enrollment(
                scope.profile.clone(),
                "enroll_fixture".into(),
                VerifiedProviderAccount::new(
                    if claude { "claude" } else { "codex" },
                    provider,
                    subject,
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap(),
                if claude { "claude-opus-5" } else { "gpt-5.6-sol" }.into(),
                Some("medium".into()),
                total,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap()
            .approve()
            .await
            .unwrap();
        let revision = registry
            .resource(&scope.profile, resource.resource_id.clone())
            .await
            .unwrap()
            .revision;
        let coordinator = if calendar.is_some() { "@owner:test" } else { "@coordinator:test" };
        let mut enrollment = VerifiedManagementEnrollment::new(
            scope.profile.clone(),
            "enrollment_fixture".into(),
            "con_fixture".into(),
            resource.resource_id,
            revision,
            coordinator.into(),
            "device".into(),
            1,
            scope.deadline,
        )
        .unwrap();
        if calendar.is_some() || per_request_tokens.is_some() { enrollment = enrollment.with_admission_request("admission_request".into()).unwrap(); }
        let delegation_end = calendar.as_ref().map(|c|c.period_at(now()).unwrap().end_ms).unwrap_or_else(||now()+lifetime_ms);
        let grant = registry
            .prepare_capacity_grant(
                enrollment,
                OwnerGrantSelection { renewal_mode: None, per_request_tokens,
                    calendar,
                    command_id: "grant_consent".into(),
                    grant_id: "grant_fixture".into(),
                    coordinator_mxid: coordinator.into(),
                    delegation_expires_at_ms: delegation_end,
                    allow_self_approval: false,
                    eligible_managers: vec!["@manager:test".into()],
                    allocated_tokens: grant_tokens,
                },
                scope.deadline,
            )
            .unwrap()
            .approve()
            .await
            .unwrap();
        state.lock().unwrap().grant = Some(grant);
        Self {
            _root: root,
            owner,
            transport,
            registry,
            state,
            task,
            current,
        }
    }
    async fn reopen(self) -> Self {
        let Self { _root, owner, transport, registry, state, task, current } = self;
        let path = _root.path().join("host");
        let origin = owner.0.expected_origin.clone();
        owner.suspend().await.unwrap();
        registry.shutdown().await.unwrap();
        drop(transport);
        drop(owner);
        drop(registry);
        let registry = OwnerResourceRegistry::isolated_for_tests(&_root.path().join("registry")).unwrap();
        let selection = crate::native_owner::CurrentProfileSelection::load_for_isolated_tests(
            &path, &origin, "@owner:test", registry.clone(),
        ).unwrap().unwrap();
        let owner = selection.open_with_matrix_guarded(
            &path, &origin, "@owner:test", Arc::new(Source(current.clone())), |_| Ok(()),
        ).await.unwrap();
        let transport = AllocationTransport::new(owner.management(package(1)).await.unwrap());
        Self { _root, owner, transport, registry, state, task, current }
    }
    fn allocation(&self) -> AllocationCommand {
        let grant = self.state.lock().unwrap().grant.clone().unwrap();
        AllocationCommand {
            version: 1,
            command_id: "reserve_fixture".into(),
            operation: "project.reserve".into(),
            allocation_id: "allocation_fixture".into(),
            project_id: "project_fixture".into(),
            project_revision: 1,
            project_definition_digest: "d".repeat(64),
            request_id: "request_fixture".into(),
            request_revision: 2,
            decision_actor: grant.coordinator_mxid.clone(),
            manager_mxid: "@manager:test".into(),
            grant_receipt: grant,
            requested_tokens: 300,
            reserve_command_id: None,
            issued_at_ms: now() - 1,
            expires_at_ms: now() + 60000,
        }
    }
    async fn stop(self) {
        self.registry.shutdown().await.unwrap();
        self.owner.retire();
        self.task.abort();
    }
}

#[test]
fn native_allocation_transport_closed_wire_matches_canonical_fixtures() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../hagency-store/src/domain/owner_resources/management-allocation-v1.json"
    )))
    .unwrap();
    let command: AllocationCommand = wire::decode(fixture["command"].clone()).unwrap();
    assert_eq!(digest(&command).unwrap(), fixture["commandDigest"]);
    let receipt: NativeAllocationReceipt = wire::decode(fixture["receipt"].clone()).unwrap();
    assert_eq!(digest(&receipt).unwrap(), fixture["receiptDigest"]);
    let parent: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../hagency-store/src/domain/owner_resources/management-parent-settlement-v1.json"
    )))
    .unwrap();
    let command: GrantClosureCommand = wire::decode(parent["command"].clone()).unwrap();
    assert_eq!(digest(&command).unwrap(), parent["commandDigest"]);
    let receipt: NativeGrantSettlementReceipt = wire::decode(parent["receipt"].clone()).unwrap();
    assert_eq!(digest(&receipt).unwrap(), parent["receiptDigest"]);
    let mut extra = fixture["command"].clone();
    extra["approved"] = json!(true);
    assert!(wire::decode::<AllocationCommand>(extra).is_err());
    let mut unsafe_number = fixture["command"].clone();
    unsafe_number["requestedTokens"] = json!(9_007_199_254_740_992_u64);
    assert!(wire::decode::<AllocationCommand>(unsafe_number).is_err());
}

/// An answered publication is kept history: it leaves the saved-command list
/// (and the eight-command bound) but is still found by its own command ID.
async fn publication_intent(f: &Fixture, id: &str) -> hagency_store::management::PendingIntent {
    assert!(
        !f.transport
            .manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|saved| saved.command_id == id)
    );
    let scope = f.transport.scope(Permission::ReadIntents).await.unwrap();
    scope.session.intent(&scope.context, id.into()).await.unwrap().unwrap()
}

#[tokio::test]
async fn native_allocation_transport_split_pool_limits_publication_recovers_original() {
    use hagency_core::recurring_quota::{Calendar, Cadence};
    for calendar in [None, Some(Calendar { cadence: Cadence::Monthly, time_zone: "America/Los_Angeles".into() })] {
        let f = Fixture::with_provider_budget_limits(
            "https://provider.test", "split-pool-account", 20_000_000, 20_000_000,
            60_000, calendar, Some(5_000_000),
        ).await;
        f.state.lock().unwrap().lose_publication = true;
        assert!(f.transport.publish_grant(&f.registry, "publish_split".into(), "grant_fixture".into(), 1).await.is_err());
        assert_eq!(f.state.lock().unwrap().publication_writes, 1);
        let recovered = f.transport.retry_publication(&f.registry, "publish_split".into()).await.unwrap();
        assert_eq!(recovered.receipt.version, 5);
        assert_eq!(recovered.receipt.allocated_tokens, 20_000_000);
        assert_eq!(recovered.receipt.per_request_tokens, Some(5_000_000));
        assert_eq!(f.state.lock().unwrap().publication_writes, 1);
        assert!(publication_intent(&f, "publish_split").await.reply.is_some());
        f.stop().await;
    }
}

#[tokio::test]
async fn native_allocation_transport_publication_reply_loss_recovers_original_journal() {
    let mut f = Fixture::new().await;
    f.state.lock().unwrap().lose_publication = true;
    assert!(
        f.transport
            .publish_grant(
                &f.registry,
                "publish_once".into(),
                "grant_fixture".into(),
                1
            )
            .await
            .is_err()
    );
    assert_eq!(f.state.lock().unwrap().publication_writes, 1);
    let saved = f.transport.manager.saved_commands().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].operation,
        hagency_store::management::IntentOperation::PublishGrant
    );
    assert!(
        f.transport
            .publish_grant(&f.registry, "replacement".into(), "grant_fixture".into(), 2)
            .await
            .is_err()
    );
    assert_eq!(f.transport.manager.saved_commands().await.unwrap().len(), 1);
    f.transport.manager.retire();
    f.transport = AllocationTransport::new(f.owner.management(package(2)).await.unwrap());
    let original = f
        .transport
        .retry_publication(&f.registry, "publish_once".into())
        .await
        .unwrap();
    assert_eq!(original.id, "grant_fixture");
    assert_eq!(f.state.lock().unwrap().publication_writes, 1);
    assert_eq!(original.receipt.allocated_tokens, 700);
    let saved_reply = publication_intent(&f, "publish_once").await.reply;
    assert!(saved_reply.is_some());
    f.state.lock().unwrap().revoked_grant = true;
    let latest = f
        .transport
        .retry_publication(&f.registry, "publish_once".into())
        .await
        .unwrap();
    assert_eq!(latest.state, "revoked");
    assert_eq!(latest.revision, 2);
    assert_eq!(publication_intent(&f, "publish_once").await.reply, saved_reply);
    assert_eq!(f.state.lock().unwrap().publication_writes, 1);
    assert!(
        f.transport
            .publish_grant(
                &f.registry,
                "publish_once".into(),
                "grant_fixture".into(),
                2
            )
            .await
            .is_err()
    );
    assert_eq!(f.state.lock().unwrap().publication_writes, 1);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_definite_rejection_allows_corrected_publication() {
    let f = Fixture::new().await;
    f.state.lock().unwrap().reject_publication = Some("management_revision_changed");
    assert_eq!(
        f.transport
            .publish_grant(&f.registry, "stale_cas".into(), "grant_fixture".into(), 1)
            .await
            .err()
            .unwrap()
            .code,
        "management_revision_changed"
    );
    let saved = publication_intent(&f, "stale_cas").await;
    assert!(
        matches!(&saved.reply, Some(hagency_store::management::IntentReply::Failure { code, .. }) if code=="management_revision_changed")
    );
    f.transport
        .manager
        .acknowledge_command("stale_cas".into())
        .await
        .unwrap();
    f.state.lock().unwrap().reject_publication = None;
    let result = f
        .transport
        .publish_grant(
            &f.registry,
            "corrected_cas".into(),
            "grant_fixture".into(),
            2,
        )
        .await
        .unwrap();
    assert_eq!(result.receipt.allocated_tokens, 700);
    assert_eq!(f.state.lock().unwrap().publication_writes, 1);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_first_preflight_mismatch_can_be_acknowledged() {
    let f = Fixture::new().await;
    f.state.lock().unwrap().changed_coordinator = true;
    let error = f
        .transport
        .publish_grant(
            &f.registry,
            "stale_enrollment".into(),
            "grant_fixture".into(),
            1,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "allocation_enrollment_changed");
    assert_eq!(f.state.lock().unwrap().publication_writes, 0);
    let saved = publication_intent(&f, "stale_enrollment").await;
    assert!(
        matches!(&saved.reply, Some(hagency_store::management::IntentReply::Failure { code, .. }) if code=="allocation_enrollment_changed")
    );
    f.transport
        .manager
        .acknowledge_command("stale_enrollment".into())
        .await
        .unwrap();
    let scope = f.transport.scope(Permission::PublishGrants).await.unwrap();
    assert_eq!(
        f.transport
            .original_grant(&scope, &f.registry, "grant_fixture")
            .await
            .unwrap()
            .allocated_tokens,
        700
    );
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_rejection_after_uncertainty_stays_pending() {
    let f = Fixture::new().await;
    f.state.lock().unwrap().lose_before_publication = true;
    assert!(
        f.transport
            .publish_grant(&f.registry, "uncertain".into(), "grant_fixture".into(), 1)
            .await
            .is_err()
    );
    f.state.lock().unwrap().reject_publication = Some("management_revision_changed");
    assert_eq!(
        f.transport
            .retry_publication(&f.registry, "uncertain".into())
            .await
            .err()
            .unwrap()
            .code,
        "management_revision_changed"
    );
    let saved = f.transport.manager.saved_commands().await.unwrap();
    assert!(saved[0].reply.is_none());
    assert!(
        f.transport
            .manager
            .acknowledge_command("uncertain".into())
            .await
            .is_err()
    );
    assert!(
        f.transport
            .publish_grant(
                &f.registry,
                "replacement_uncertain".into(),
                "grant_fixture".into(),
                2
            )
            .await
            .is_err()
    );
    assert_eq!(f.state.lock().unwrap().publication_writes, 0);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_owner_coordination_keeps_original_grant_binding() {
    let f = Fixture::new().await;
    let mut command = f.allocation();
    let original = digest(&command.grant_receipt).unwrap();
    command.decision_actor = command.grant_receipt.owner_mxid.clone();
    {
        let mut state = f.state.lock().unwrap();
        state.owner_coordinator = true;
        state.allocation = Some(command.clone());
    }
    f.transport.poll_allocation(&f.registry, None).await.unwrap();
    let scope = f.transport.scope(Permission::ExecuteAllocations).await.unwrap();
    let receipt = f.registry.recover_project_allocation(scope.profile.clone(), "device".into(), command.command_id.clone(), digest(&command).unwrap()).await.unwrap().unwrap();
    assert_eq!(receipt.state, "reserved");
    assert_eq!(digest(&f.state.lock().unwrap().grant.clone().unwrap()).unwrap(), original);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_committed_child_survives_restart_and_parent_cleanup() {
    let mut f = Fixture::new().await;
    let command = f.allocation();
    let command_digest = digest(&command).unwrap();
    {
        let mut s = f.state.lock().unwrap();
        s.allocation = Some(command.clone());
        s.lose_result = true;
    }
    assert!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .is_err()
    );
    let scope = f
        .transport
        .scope(Permission::ExecuteAllocations)
        .await
        .unwrap();
    let receipt = f
        .registry
        .recover_project_allocation(
            scope.profile.clone(),
            "device".into(),
            command.command_id.clone(),
            command_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state, "reserved");
    drop(scope);
    f = f.reopen().await;
    let result = f
        .transport
        .recover_allocation(&f.registry, command.command_id.clone(), command_digest)
        .await
        .unwrap();
    assert!(
        matches!(result,AllocationProgress::Recorded {ledger_revision,..} if ledger_revision==receipt.ledger_revision)
    );
    assert_eq!(
        f.state.lock().unwrap().result_writes,
        1,
        "already recorded central receipt must not be uploaded again"
    );
    let grant = f.state.lock().unwrap().grant.clone().unwrap();
    let close = GrantClosureCommand {
        version: 1,
        command_id: "close_fixture".into(),
        operation: "grant.close".into(),
        grant_id: grant.grant_id.clone(),
        grant_receipt_digest: digest(&grant).unwrap(),
        previous_settlement_digest: None,
        reason: "owner_withdrawal".into(),
        requested_at_ms: now(),
    };
    f.state.lock().unwrap().parent = Some(close);
    assert!(matches!(
        f.transport
            .poll_parent_settlement(&f.registry, None)
            .await
            .unwrap(),
        AllocationProgress::Recorded { .. }
    ));
    let first = f.state.lock().unwrap().parent_receipt.clone().unwrap();
    assert_eq!(first.state, "retained");
    assert_eq!(first.retained_tokens, 300);
    assert!(first.ledger_revision > receipt.ledger_revision);
    let mut release = command.clone();
    release.operation = "project.release".into();
    release.command_id = "release_fixture".into();
    release.reserve_command_id = Some(command.command_id);
    {
        let mut s = f.state.lock().unwrap();
        s.allocation = Some(release);
        s.allocation_receipt = None;
    }
    assert!(matches!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .unwrap(),
        AllocationProgress::Recorded { .. }
    ));
    {
        let mut s = f.state.lock().unwrap();
        s.parent = Some(GrantClosureCommand {
            version: 1,
            command_id: "settle_fixture".into(),
            operation: "grant.settle".into(),
            grant_id: grant.grant_id.clone(),
            grant_receipt_digest: digest(&grant).unwrap(),
            previous_settlement_digest: Some(digest(&first).unwrap()),
            reason: "settlement_progress".into(),
            requested_at_ms: now(),
        });
        s.parent_receipt = None;
    }
    assert!(matches!(
        f.transport
            .poll_parent_settlement(&f.registry, None)
            .await
            .unwrap(),
        AllocationProgress::Recorded { .. }
    ));
    let final_receipt = f.state.lock().unwrap().parent_receipt.clone().unwrap();
    assert_eq!(final_receipt.state, "closed");
    assert_eq!(final_receipt.retained_tokens, 0);
    assert_eq!(final_receipt.fence_revision, first.fence_revision);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_history_never_acquires_execution_authority() {
    let f = Fixture::new().await;
    let command = f.allocation();
    let hash = digest(&command).unwrap();
    f.state.lock().unwrap().allocation = Some(command.clone());
    assert!(matches!(
        f.transport
            .recover_allocation(&f.registry, command.command_id.clone(), hash.clone())
            .await
            .unwrap(),
        AllocationProgress::HistoricalReceiptMissing { .. }
    ));
    f.state.lock().unwrap().historical_executable = true;
    assert!(
        f.transport
            .recover_allocation(&f.registry, command.command_id.clone(), hash.clone())
            .await
            .is_err()
    );
    f.state.lock().unwrap().historical_executable = false;
    f.state.lock().unwrap().tamper_digest = true;
    assert!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .is_err()
    );
    f.state.lock().unwrap().tamper_digest = false;
    f.state.lock().unwrap().expired_ticket = true;
    assert!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .is_err()
    );
    let scope = f
        .transport
        .scope(Permission::ExecuteAllocations)
        .await
        .unwrap();
    assert!(
        f.registry
            .recover_project_allocation(scope.profile, "device".into(), command.command_id, hash)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.state.lock().unwrap().result_writes, 0);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_wrong_ack_retains_receipt_and_scope_loss_stops_work() {
    let f = Fixture::new().await;
    let command = f.allocation();
    {
        let mut s = f.state.lock().unwrap();
        s.allocation = Some(command.clone());
        s.wrong_ack = true;
    }
    assert_eq!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .unwrap_err()
            .code,
        "allocation_acknowledgement_conflict"
    );
    let scope = f
        .transport
        .scope(Permission::ExecuteAllocations)
        .await
        .unwrap();
    assert!(
        f.registry
            .recover_project_allocation(
                scope.profile,
                "device".into(),
                command.command_id.clone(),
                digest(&command).unwrap()
            )
            .await
            .unwrap()
            .is_some()
    );
    f.current.store(false, Ordering::Release);
    let count = f.state.lock().unwrap().result_writes;
    assert!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .is_err()
    );
    assert_eq!(f.state.lock().unwrap().result_writes, count);
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_account_change_during_reply_never_loses_custody() {
    for after_effect in [false, true] {
        let f = Fixture::new().await;
        let command = f.allocation();
        {
            let mut s = f.state.lock().unwrap();
            s.allocation = Some(command.clone());
            s.invalidate_at_contribution = !after_effect;
            s.invalidate_at_result = after_effect;
        }
        let error = f
            .transport
            .poll_allocation(&f.registry, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            if after_effect {
                "management_outcome_unknown"
            } else {
                "matrix_account_changed"
            }
        );
        assert_eq!(
            f.state.lock().unwrap().result_writes,
            usize::from(after_effect)
        );
        f.current.store(true, Ordering::Release);
        let scope = f
            .transport
            .scope(Permission::ExecuteAllocations)
            .await
            .unwrap();
        let receipt = f
            .registry
            .recover_project_allocation(
                scope.profile,
                "device".into(),
                command.command_id.clone(),
                digest(&command).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.is_some(), after_effect);
        f.stop().await;
    }
}

#[tokio::test]
async fn native_allocation_transport_reauthorization_allows_cleanup_but_not_old_generation_reserve()
{
    let f = Fixture::new().await;
    let command = f.allocation();
    f.state.lock().unwrap().allocation = Some(command.clone());
    f.transport
        .poll_allocation(&f.registry, None)
        .await
        .unwrap();
    let original = f.state.lock().unwrap().allocation_receipt.clone().unwrap();
    explicit_new_generation(&f, 2).await;
    let scope = f
        .transport
        .scope(Permission::ExecuteAllocations)
        .await
        .unwrap();
    assert_eq!(scope.device.generation(), 2);
    let mut fresh_id = command.clone();
    fresh_id.command_id = "stale_generation_reserve".into();
    fresh_id.allocation_id = "stale_generation_allocation".into();
    fresh_id.project_id = "different_project".into();
    {
        let mut s = f.state.lock().unwrap();
        s.allocation = Some(fresh_id.clone());
        s.allocation_receipt = None;
    }
    assert_eq!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .unwrap_err()
            .code,
        "allocation_enrollment_changed"
    );
    assert!(
        f.registry
            .recover_project_allocation(
                scope.profile,
                "device".into(),
                fresh_id.command_id.clone(),
                digest(&fresh_id).unwrap()
            )
            .await
            .unwrap()
            .is_none()
    );
    let mut release = command.clone();
    release.command_id = "cleanup_after_reauthorization".into();
    release.operation = "project.release".into();
    release.reserve_command_id = Some(command.command_id.clone());
    f.state.lock().unwrap().allocation = Some(release);
    f.transport
        .poll_allocation(&f.registry, None)
        .await
        .unwrap();
    let released = f.state.lock().unwrap().allocation_receipt.clone().unwrap();
    assert_eq!(released.state, "released");
    assert_eq!(released.retained_tokens, 0);
    assert!(released.ledger_revision > original.ledger_revision);
    let grant = f.state.lock().unwrap().grant.clone().unwrap();
    f.state.lock().unwrap().parent = Some(GrantClosureCommand {
        version: 1,
        command_id: "close_after_reauthorization".into(),
        operation: "grant.close".into(),
        grant_id: grant.grant_id.clone(),
        grant_receipt_digest: digest(&grant).unwrap(),
        previous_settlement_digest: None,
        reason: "owner_withdrawal".into(),
        requested_at_ms: now(),
    });
    f.transport
        .poll_parent_settlement(&f.registry, None)
        .await
        .unwrap();
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .parent_receipt
            .as_ref()
            .unwrap()
            .state,
        "closed"
    );
    f.stop().await;
}

#[tokio::test]
async fn native_allocation_transport_retired_session_fences_a_queued_ledger_write() {
    let f = Fixture::new().await;
    let command = f.allocation();
    let hash = digest(&command).unwrap();
    let scope = f
        .transport
        .scope(Permission::ExecuteAllocations)
        .await
        .unwrap();
    let enrollment = f
        .transport
        .enrollment(&scope, &command.grant_receipt, true)
        .await
        .unwrap();
    let delivery = VerifiedAllocationDelivery::new(
        enrollment,
        command.clone(),
        hash.clone(),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    // A competing SQLite writer holds BEGIN IMMEDIATE. The actual captured
    // NativeManagement session, not a stand-in flag, retires while ours waits.
    let mut db =
        rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let lock = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let mut pending = Box::pin(f.registry.apply_project_allocation(delivery));
    tokio::select! {
        biased;
        result=&mut pending=>panic!("writer unexpectedly completed: {result:?}"),
        _=tokio::time::sleep(Duration::from_millis(50))=>{}
    }
    f.transport.manager.retire();
    lock.commit().unwrap();
    assert!(matches!(
        pending.await,
        Err(hagency_store::Error::LocalAuthority)
    ));
    let current = AllocationTransport::new(f.owner.management(package(2)).await.unwrap());
    let scope = current.scope(Permission::ExecuteAllocations).await.unwrap();
    assert!(
        f.registry
            .recover_project_allocation(scope.profile, "device".into(), command.command_id, hash)
            .await
            .unwrap()
            .is_none()
    );
    f.stop().await;
}

#[cfg(not(feature = "shared-intake"))]
#[tokio::test]
async fn native_run_control_disabled_before_journal() {
    use crate::native_owner::{AgentRunMode, ManagementCommand};
    let f=Fixture::new().await;
    let m=f.owner.management(ManagementPackage{
        digest:"7".repeat(64),instance:991,account_epoch:1,
        permissions:vec![Permission::ReadAgentRunControl,Permission::ChangeAgentRunControl,Permission::ReadIntents],
    }).await.unwrap();
    for command in [
        ManagementCommand::AgentRunReadiness{assignment_id:"assignment_control".into()},
        ManagementCommand::AgentRunControl{command_id:"disabled_original".into(),assignment_id:"assignment_control".into(),expected_revision:1,mode:AgentRunMode::Paused},
    ] {
        assert_eq!(m.central(command).await.unwrap_err().code,"shared_intake_disabled");
    }
    assert!(m.saved_commands().await.unwrap().is_empty());
    f.stop().await;
}
