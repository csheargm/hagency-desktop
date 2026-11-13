//! Actual TCP + SDK source checks; only explicit policy/observation tests POST.
//! No provider process or real execution device is invoked.
use super::*;
use crate::native_owner::{ManagementPackage, MatrixAccessToken, MatrixTokenSource, NativeOwner};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
                access_token: "fixture".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}
fn configuration_fixture() -> Value {
    let mut v = super::tests::snapshot();
    v["configuration"]["environment"]["providers"] =
        json!(["codex", "claude", "octos"].map(|runtime| json!({
            "runtime":runtime,"installed":false,"version":null,
            "versionSupported":false,"state":"disconnected",
            "authenticated":false,"accountVerified":false,
            "credentialRef":null,"credentialSource":"none",
            "shared":false,"strictTokenCap":false,"nativeTools":false,
            "models":null,"defaultModel":null
        })));
    v
}
fn editable_configuration_fixture() -> Value {
    let mut value=configuration_fixture();
    value["configuration"]["deviceGeneration"]=json!(1);
    let env=&mut value["configuration"]["environment"];
    env["configuration"]["modelProfile"]["credential_ref"]=json!("keychain:codex:private-owner");
    env["providers"][0]=json!({"runtime":"codex","installed":true,"version":"0.1","versionSupported":true,"state":"ready","authenticated":true,"accountVerified":true,"credentialRef":"keychain:codex:private-owner","credentialSource":"keychain","shared":false,"strictTokenCap":false,"nativeTools":true,"defaultModel":"fixture-model","models":[{"id":"fixture-model","model":"fixture-model","displayName":"Fixture","description":"Fixture model","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}]}]});
    value
}
#[derive(Default)]
struct State {
    reads: Vec<String>,
    invalidate: bool,
    unsupported: bool,
    changed: bool,
    lose_reply: bool,
    fail_write: bool,
    /// The code the device reports with `fail_write`.
    fail_code: Option<&'static str>,
    invalidate_write: bool,
    drop_capability_after_post: bool,
    requests: std::collections::BTreeMap<String, Value>,
    policy_effects: usize,
    observation_effects: usize,
    observation_missing: bool,
    observation: Option<Value>,
    current_policy: Option<Value>,
    editable: bool,
    model_effects: usize,
    workspace_effects: usize,
    agent_generation: Option<u64>,
}
struct Fixture {
    root: tempfile::TempDir,
    owner: NativeOwner,
    current: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", socket.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let current = Arc::new(AtomicBool::new(true));
        let (records, source, base) = (state.clone(), current.clone(), origin.clone());
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = socket.accept().await else {
                    break;
                };
                let (records, source, base) = (records.clone(), source.clone(), base.clone());
                tokio::spawn(async move {
                    let mut raw = vec![];
                    let mut buf = [0; 4096];
                    let end = loop {
                        let n = stream.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&buf[..n]);
                        assert!(raw.len() < 32768);
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&raw[..end]);
                    let path = headers
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned();
                    let method = headers
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap();
                    let method = method.to_owned();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while raw.len() < end + length {
                        let n = stream.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&buf[..n]);
                        assert!(raw.len() < 32768);
                    }
                    let input: Value = if length == 0 {
                        Value::Null
                    } else {
                        serde_json::from_slice(&raw[end..end + length]).unwrap()
                    };
                    let until = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                        + 30000) as u64;
                    let v = {
                        let mut s = records.lock().unwrap();
                        s.reads.push(format!("{method} {path}"));
                        match path.as_str() {
                            "/hagency/api/v1/discovery" => {
                                let mut caps = vec![
                                    "pasion-oauth",
                                    "owner-agent-appservice-v1",
                                    "global-agent-identity-v2",
                                    "execution-device-v1",
                                    "owner-direct-v1",
                                ];
                                if !s.unsupported {
                                    caps.push("execution-configuration-v1");
                                }
                                json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":caps,"homeserver":base,"issuer":format!("{base}auth/")})
                            }
                            "/hagency/api/v1/sessions/pasion" => {
                                json!({"token":"a".repeat(64),"userId":"uid","mxid":"@owner:test","validUntilMs":until})
                            }
                            "/hagency/api/v1/identity" => {
                                json!({"userId":"uid","mxid":"@owner:test","subject":"subject","clientId":"sdk-client","issuer":format!("{base}auth/"),"validUntilMs":until})
                            }
                            "/hagency/api/v1/devices" if method == "POST" => {
                                json!({"deviceId":"device_a","token":"b".repeat(64),"generation":1,"validUntilMs":until})
                            }
                            "/hagency/api/v1/devices" => json!({"devices":[]}),
                            "/hagency/api/v1/projects" => json!({"projects":[]}),
                            "/hagency/api/v1/agents" => {
                                json!({"agents":[{"id":"agent_a","ownerUserId":"uid","puppetMxid":"@agent:test","displayName":"Personal A","state":"suspended","generation":s.agent_generation.unwrap_or(3),"executionDeviceId":"device_a","ownerDirectRoomId":null}]})
                            }
                            "/hagency/api/v1/agents/agent_a/configuration" => {
                                if s.invalidate {
                                    source.store(false, Ordering::Release);
                                }
                                let mut v = if s.editable {editable_configuration_fixture()} else {configuration_fixture()};
                                if let Some(observation) = &s.observation {
                                    v["configuration"]["environment"] = observation.clone();
                                }
                                if s.observation_missing {
                                    v["configuration"]["environment"] = Value::Null;
                                    v["configuration"]["observedAtMs"] = Value::Null;
                                }
                                v["configuration"]["agentGeneration"] =
                                    json!(s.agent_generation.unwrap_or(3));
                                if s.changed {
                                    v["configuration"]["agentGeneration"] = json!(4);
                                }
                                v
                            }
                            "/hagency/api/v1/agents/agent_a/configuration/requests"
                                if method == "POST" =>
                            {
                                let op = input["operation"].as_str().unwrap();
                                assert!(matches!(op, "save_agent_policy" | "environment" | "save_agent_model" | "prepare_workspace"));
                                if op == "environment" {
                                    assert_eq!(input["input"], json!({}));
                                }
                                assert_eq!(input["expectedGeneration"], 3);
                                let key = input["idempotencyKey"].as_str().unwrap().to_owned();
                                let failed = s.fail_write;
                                let fail_code = s.fail_code.unwrap_or("local_revision_conflict");
                                let current_policy = s.current_policy.clone();
                                if let std::collections::btree_map::Entry::Vacant(entry) =
                                    s.requests.entry(key.clone())
                                {
                                    let mut result =
                                        configuration_fixture()["configuration"]["environment"]
                                            .clone();
                                    if let Some(policy) = current_policy {
                                        result["configuration"]["policy"] = policy;
                                    }
                                    if op == "save_agent_policy" {
                                        result = result["configuration"].clone();
                                        result["policy"] = json!({"revision":input["input"]["expectedRevision"].as_i64().unwrap()+1,"policy":input["input"]["policy"]});
                                    }
                                    if op=="save_agent_model" {
                                        result=result["configuration"].clone();
                                        result["modelProfile"]=input["input"]["profile"].clone();
                                        if result["modelProfile"]["workspace_root"]=="" {result["modelProfile"]["workspace_root"]=json!("/private/default-workspace");}
                                    }
                                    if op=="prepare_workspace" {result=json!({"agentId":"agent_a","workspace":"/private/default-workspace"});}
                                    let work = json!({"request":{"id":"cfg_original","agentId":"agent_a","agentGeneration":3,
                                        "executionDeviceId":"device_a","deviceGeneration":1,"operation":op,"input":input["input"],
                                        "state":if failed {"failed"}else{"succeeded"},"createdAtMs":1,"expiresAtMs":until,
                                        "claimedAtMs":1,"completedAtMs":2,"result":if failed {Value::Null}else{result.clone()},
                                        "errorCode":if failed {json!(fail_code)}else{Value::Null}}});
                                    entry.insert(work);
                                    if !failed {
                                        if op == "environment" {
                                            s.observation_effects += 1;
                                            s.observation = Some(result);
                                            s.observation_missing = false;
                                        } else if op=="save_agent_model" {
                                            s.model_effects+=1;
                                            s.observation_missing=true;
                                        } else if op=="prepare_workspace" {
                                            s.workspace_effects+=1;
                                        } else {
                                            s.current_policy = Some(result["policy"].clone());
                                            s.policy_effects += 1;
                                            s.observation_missing = true;
                                        }
                                    }
                                }
                                if s.lose_reply {
                                    s.lose_reply = false;
                                    return;
                                }
                                if s.invalidate_write {
                                    source.store(false, Ordering::Release);
                                }
                                if s.drop_capability_after_post {
                                    s.drop_capability_after_post = false;
                                    s.unsupported = true;
                                    let mut pending = s.requests[&key].clone();
                                    pending["request"]["state"] = json!("pending");
                                    for key in ["result", "completedAtMs", "claimedAtMs"] {
                                        pending["request"][key] = Value::Null;
                                    }
                                    pending
                                } else {
                                    s.requests[&key].clone()
                                }
                            }
                            "/hagency/api/v1/device-configuration/poll" => json!({"request":null}),
                            "/hagency/api/v1/sessions/current/renew" => {
                                json!({"validUntilMs":until})
                            }
                            "/hagency/api/v1/sessions/current"
                            | "/hagency/api/v1/devices/current" => json!({}),
                            _ => panic!("unexpected fixture route {path}"),
                        }
                    };
                    let bytes = serde_json::to_vec(&v).unwrap();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&bytes).await;
                });
            }
        });
        let owner = NativeOwner::open_with_matrix(
            &root.path().join("native"),
            &origin,
            "@owner:test",
            Arc::new(Source(current.clone())),
        )
        .await
        .unwrap();
        let tasks = std::mem::take(&mut *owner.0.configuration_tasks.lock().unwrap());
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        Self {
            root,
            owner,
            current,
            state,
            server,
        }
    }
    async fn manager(&self, allow: bool) -> Arc<NativeManagement> {
        self.owner
            .management(ManagementPackage {
                digest: "f".repeat(64),
                instance: if allow { 1 } else { 2 },
                account_epoch: 1,
                permissions: if allow {
                    vec![
                        Permission::ReadPersonalAgentSettings,
                        Permission::EditPersonalAgentPolicy,
                        Permission::EditPersonalAgentModel,
                        Permission::PreparePersonalAgentWorkspace,
                        Permission::RequestPersonalAgentSettings,
                        Permission::ReadIntents,
                        Permission::AcknowledgeIntents,
                    ]
                } else {
                    vec![Permission::ReadResources]
                },
            })
            .await
            .unwrap()
    }
    async fn stop(self) {
        self.owner.shutdown().await;
        self.server.abort();
    }
}
#[tokio::test]
async fn personal_settings_http_read_scope_and_no_execution_request() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    let denied = f.manager(false).await;
    f.state.lock().unwrap().reads.clear();
    assert_eq!(
        denied
            .personal_agents(PersonalAgentQuery {
                after: None,
                limit: Some(1)
            })
            .await
            .unwrap_err()
            .code,
        "management_permission_required"
    );
    assert!(
        !f.state
            .lock()
            .unwrap()
            .reads
            .iter()
            .any(|p| p.ends_with("/agents"))
    );
    let page = m
        .personal_agents(PersonalAgentQuery {
            after: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(page["items"][0]["id"], "agent_a");
    let got = m.personal_agent_settings("agent_a".into()).await.unwrap();
    assert_eq!(got["settings"]["observation"]["knownConsumedTokens"], 17);
    assert_eq!(
        got["settings"]["observation"]["model"]["workspaceConfigured"],
        true
    );
    assert!(!got.to_string().contains("private-"));
    assert!(!got.to_string().contains("/private"));
    f.state.lock().unwrap().changed = true;
    assert_eq!(
        m.personal_agent_settings("agent_a".into())
            .await
            .unwrap_err()
            .code,
        "personal_agent_settings_changed"
    );
    f.state.lock().unwrap().unsupported = true;
    let got = m.personal_agent_settings("agent_a".into()).await.unwrap();
    assert_eq!(got["settings"]["supported"], false);
    assert!(got["settings"]["observation"].is_null());
    assert!(
        !f.state
            .lock()
            .unwrap()
            .reads
            .iter()
            .any(|p| p.contains("/configuration/requests"))
    );
    fn no_personal(path: &std::path::Path) {
        for e in std::fs::read_dir(path).unwrap() {
            let e = e.unwrap();
            assert_ne!(e.file_name(), "agent-local.sqlite");
            if e.file_type().unwrap().is_dir() {
                no_personal(&e.path());
            }
        }
    }
    no_personal(f.root.path());
    m.retire();
    assert!(
        m.personal_agents(PersonalAgentQuery {
            after: None,
            limit: None
        })
        .await
        .is_err()
    );
    f.stop().await;
}
#[tokio::test]
async fn personal_settings_http_rechecks_source_after_observation() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().invalidate = true;
    assert!(m.personal_agent_settings("agent_a".into()).await.is_err());
    assert!(!f.current.load(Ordering::Acquire));
    assert!(
        !f.state
            .lock()
            .unwrap()
            .reads
            .iter()
            .any(|p| p.contains("/configuration/requests"))
    );
    f.stop().await;
}

fn change() -> super::super::personal_agent_policy::SavePersonalPolicy {
    serde_json::from_value(
        json!({"commandId":"policy_original","agentId":"agent_a","expectedGeneration":3,
        "expectedRevision":4,"policy":{"budget":{"kind":"tokens","tokens":250,"period":"utc_month"},
        "requests":"ask_owner","highRiskTools":"deny"}}),
    )
    .unwrap()
}
#[tokio::test]
async fn personal_policy_http_original_command_recovery_and_compare_and_swap() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().lose_reply = true;
    assert!(m.save_personal_policy(change()).await.is_err());
    let pending = m.saved_commands().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].reply.is_none());
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    // Recover without a script draft. Original private requester/input/key stay.
    let got = m
        .retry_saved_command("policy_original".into())
        .await
        .unwrap();
    assert_eq!(got["data"]["saved"], true);
    assert_eq!(got["data"]["policyRevision"], 5);
    assert!(!got.to_string().contains("private-"));
    assert!(!got.to_string().contains("subject"));
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    let replay = m.save_personal_policy(change()).await.unwrap();
    assert_eq!(replay["policyRevision"], 5);
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    let mut other = change();
    other.expected_revision = 5;
    assert_eq!(
        m.save_personal_policy(other).await.unwrap_err().code,
        "management_command_conflict"
    );
    m.acknowledge_command("policy_original".into())
        .await
        .unwrap();
    f.state.lock().unwrap().fail_write = true;
    let mut next = change();
    next.command_id = "policy_conflict".into();
    assert_eq!(
        m.save_personal_policy(next).await.unwrap_err().code,
        "local_revision_conflict"
    );
    assert!(m.saved_commands().await.unwrap()[0].reply.is_some());
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.stop().await;
}
#[tokio::test]
async fn personal_policy_http_post_commit_scope_loss_keeps_original_pending() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().invalidate_write = true;
    assert!(m.save_personal_policy(change()).await.is_err());
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.current.store(true, Ordering::Release);
    f.state.lock().unwrap().invalidate_write = false;
    let saved = m.saved_commands().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].reply.is_none());
    let v = m
        .retry_saved_command("policy_original".into())
        .await
        .unwrap();
    assert_eq!(v["data"]["saved"], true);
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.stop().await;
}

#[tokio::test]
async fn personal_policy_http_permission_and_original_requester_block_before_post() {
    let f = Fixture::new().await;
    let denied = f.manager(false).await;
    assert_eq!(
        denied
            .save_personal_policy(change())
            .await
            .unwrap_err()
            .code,
        "management_permission_required"
    );
    let mut wrong = super::super::configuration::pin(&f.owner).await.unwrap();
    wrong.generation += 1;
    let error=super::super::configuration::remote_pinned(&f.owner,
        Command::AgentConfiguration {agent_id:"agent_a".into(),expected_generation:3,idempotency_key:"original".into(),
            command:Box::new(Command::Local {agent_id:"agent_a".into(),action:super::super::LocalAction::SaveAgentPolicy,
                input:Some(json!({"expectedRevision":4,"policy":{"budget":{"limit":{"tokens":1},"period":"utc_month"},"requests":"ask_owner","high_risk":"deny"}})),binding_id:None,requester:None})},Some(&wrong)).await.unwrap_err();
    assert_eq!(error.code, "personal_policy_original_device_required");
    assert_eq!(f.state.lock().unwrap().policy_effects, 0);
    assert!(
        !f.state
            .lock()
            .unwrap()
            .reads
            .iter()
            .any(|p| p.starts_with("POST ") && p.contains("/configuration/requests"))
    );
    f.stop().await;
}
#[tokio::test]
async fn personal_policy_http_unknown_then_revision_change_is_not_false_failure() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().lose_reply = true;
    assert!(m.save_personal_policy(change()).await.is_err());
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.state.lock().unwrap().changed = true;
    assert_eq!(
        m.retry_saved_command("policy_original".into())
            .await
            .unwrap_err()
            .code,
        "management_outcome_unknown"
    );
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    assert!(
        m.acknowledge_command("policy_original".into())
            .await
            .is_err()
    );
    f.state.lock().unwrap().changed = false;
    assert_eq!(
        m.retry_saved_command("policy_original".into())
            .await
            .unwrap()["data"]["saved"],
        true
    );
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.stop().await;
}

#[tokio::test]
async fn personal_policy_http_saved_result_recovery_precedes_new_target_generation() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().lose_reply = true;
    assert!(m.save_personal_policy(change()).await.is_err());
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    f.state.lock().unwrap().agent_generation = Some(4);
    let got = m
        .retry_saved_command("policy_original".into())
        .await
        .unwrap();
    assert_eq!(got["data"]["policyRevision"], 5);
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    m.acknowledge_command("policy_original".into())
        .await
        .unwrap();
    // An unrelated fresh draft cannot reuse the old target generation.
    let mut fresh = change();
    fresh.command_id = "policy_fresh_stale".into();
    let posts = f
        .state
        .lock()
        .unwrap()
        .reads
        .iter()
        .filter(|p| p.starts_with("POST ") && p.contains("/configuration/requests"))
        .count();
    assert_eq!(
        m.save_personal_policy(fresh).await.unwrap_err().code,
        "personal_agent_settings_changed"
    );
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .reads
            .iter()
            .filter(|p| p.starts_with("POST ") && p.contains("/configuration/requests"))
            .count(),
        posts
    );
    f.stop().await;
}

#[tokio::test]
async fn personal_policy_http_discovery_loss_after_post_keeps_original_pending() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().drop_capability_after_post = true;
    assert_eq!(
        m.save_personal_policy(change()).await.unwrap_err().code,
        "configuration_unsupported"
    );
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    assert!(
        m.acknowledge_command("policy_original".into())
            .await
            .is_err()
    );
    f.state.lock().unwrap().unsupported = false;
    let recovered = m
        .retry_saved_command("policy_original".into())
        .await
        .unwrap();
    assert_eq!(recovered["data"]["saved"], true);
    assert_eq!(f.state.lock().unwrap().policy_effects, 1);
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    f.stop().await;
}

fn observation() -> super::super::personal_agent_observation::ObservePersonalSettings {
    serde_json::from_value(
        json!({"commandId":"observation_original","agentId":"agent_a","expectedGeneration":3}),
    )
    .unwrap()
}
#[tokio::test]
async fn personal_observation_http_explicit_request_bootstraps_and_refresh_stays_readonly() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().observation_missing = true;
    assert!(
        m.personal_agent_settings("agent_a".into()).await.unwrap()["settings"]["observation"]
            .is_null()
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 0);
    let result = m.observe_personal_settings(observation()).await.unwrap();
    assert_eq!(result["observed"], true);
    assert!(result.get("environment").is_none());
    assert!(!result.to_string().contains("private"));
    assert!(
        m.personal_agent_settings("agent_a".into()).await.unwrap()["settings"]["observation"]
            .is_object()
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 1);
    assert_eq!(f.state.lock().unwrap().policy_effects, 0);
    m.acknowledge_command("observation_original".into())
        .await
        .unwrap();
    m.save_personal_policy(change()).await.unwrap();
    assert!(
        m.personal_agent_settings("agent_a".into()).await.unwrap()["settings"]["observation"]
            .is_null()
    );
    m.acknowledge_command("policy_original".into())
        .await
        .unwrap();
    let mut next = observation();
    next.command_id = "observation_after_edit".into();
    m.observe_personal_settings(next).await.unwrap();
    let current = m.personal_agent_settings("agent_a".into()).await.unwrap();
    assert_eq!(current["settings"]["observation"]["policyRevision"], 5);
    assert_eq!(current["settings"]["observation"]["budget"]["tokens"], 250);
    assert_eq!(f.state.lock().unwrap().observation_effects, 2);
    f.stop().await;
}
#[tokio::test]
async fn personal_observation_http_lost_reply_recovers_original_after_generation_change() {
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    f.state.lock().unwrap().lose_reply = true;
    assert!(m.observe_personal_settings(observation()).await.is_err());
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    assert!(
        m.acknowledge_command("observation_original".into())
            .await
            .is_err()
    );
    f.state.lock().unwrap().agent_generation = Some(4);
    let got = m
        .retry_saved_command("observation_original".into())
        .await
        .unwrap();
    assert_eq!(got["data"]["observed"], true);
    assert_eq!(f.state.lock().unwrap().observation_effects, 1);
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    let mut fresh = observation();
    fresh.command_id = "observation_stale".into();
    assert_eq!(
        m.observe_personal_settings(fresh).await.unwrap_err().code,
        "personal_agent_settings_changed"
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 1);
    f.stop().await;
}
fn closed_reply(saved: &[hagency_store::management::PendingIntent]) -> Option<(u16, String)> {
    match &saved[0].reply {
        Some(hagency_store::management::IntentReply::Failure { status, code }) => {
            Some((*status, code.clone()))
        }
        _ => None,
    }
}
#[tokio::test]
async fn personal_observation_http_device_failure_is_the_final_answer() {
    // The device answers "failed": the request is over on the first attempt.
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    {
        let mut s = f.state.lock().unwrap();
        s.fail_write = true;
        s.fail_code = Some("configuration_provider_unavailable");
    }
    let error = m.observe_personal_settings(observation()).await.unwrap_err();
    assert_eq!(
        (error.status, error.code.as_str()),
        (409, "configuration_provider_unavailable")
    );
    assert_eq!(
        closed_reply(&m.saved_commands().await.unwrap()),
        Some((409, "configuration_provider_unavailable".into()))
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 0);
    m.acknowledge_command("observation_original".into())
        .await
        .unwrap();
    assert!(m.saved_commands().await.unwrap().is_empty());
    f.stop().await;
}
#[tokio::test]
async fn personal_observation_http_retry_reads_the_original_failure_and_settles() {
    // The first reply is lost, so the request is saved without an answer.
    // Asking again under the original key reads the device's own "failed":
    // that settles it instead of leaving a request no retry can finish.
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    {
        let mut s = f.state.lock().unwrap();
        s.fail_write = true;
        s.fail_code = Some("configuration_provider_unavailable");
        s.lose_reply = true;
    }
    assert!(m.observe_personal_settings(observation()).await.is_err());
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    let error = m.observe_personal_settings(observation()).await.unwrap_err();
    assert_eq!(
        (error.status, error.code.as_str()),
        (409, "configuration_provider_unavailable")
    );
    assert_eq!(
        closed_reply(&m.saved_commands().await.unwrap()),
        Some((409, "configuration_provider_unavailable".into()))
    );
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    assert_eq!(f.state.lock().unwrap().observation_effects, 0);
    m.acknowledge_command("observation_original".into())
        .await
        .unwrap();
    f.stop().await;
}
#[tokio::test]
async fn personal_policy_http_retry_of_a_failed_save_stays_unknown() {
    // A save changes the device, so the rule for a later attempt is kept:
    // a failure seen on a retry does not prove the first attempt did nothing.
    let f = Fixture::new().await;
    let m = f.manager(true).await;
    {
        let mut s = f.state.lock().unwrap();
        s.fail_write = true;
        s.fail_code = Some("configuration_provider_unavailable");
        s.lose_reply = true;
    }
    assert!(m.save_personal_policy(change()).await.is_err());
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    assert!(
        m.retry_saved_command("policy_original".into())
            .await
            .is_err()
    );
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    f.stop().await;
}
#[tokio::test]
async fn personal_observation_http_permission_and_post_commit_scope_loss() {
    let f = Fixture::new().await;
    let denied = f.manager(false).await;
    assert_eq!(
        denied
            .observe_personal_settings(observation())
            .await
            .unwrap_err()
            .code,
        "management_permission_required"
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 0);
    let m = f.manager(true).await;
    f.state.lock().unwrap().invalidate_write = true;
    assert!(m.observe_personal_settings(observation()).await.is_err());
    assert_eq!(f.state.lock().unwrap().observation_effects, 1);
    f.current.store(true, Ordering::Release);
    f.state.lock().unwrap().invalidate_write = false;
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    assert_eq!(
        m.retry_saved_command("observation_original".into())
            .await
            .unwrap()["data"]["observed"],
        true
    );
    assert_eq!(f.state.lock().unwrap().observation_effects, 1);
    f.stop().await;
}

fn confirmed() -> std::time::Instant { std::time::Instant::now()+std::time::Duration::from_secs(30) }
async fn model_change(m:&NativeManagement,id:&str)->super::super::SavePersonalModel {
    let choices=m.personal_model_choices("agent_a".into()).await.unwrap();
    let public=serde_json::to_string(&choices).unwrap();
    assert!(!public.contains("private")&&!public.contains("credential")&&!public.contains("subject"));
    assert_eq!(choices.models.len(),1);
    assert!(choices.can_choose_workspace);
    serde_json::from_value(json!({"commandId":id,"agentId":"agent_a","expectedGeneration":choices.generation,"expectedProfileDigest":choices.profile_digest,"runtime":"codex","model":"fixture-model","reasoningEffort":"high","webSearch":false,"workspace":"preserve"})).unwrap()
}
#[tokio::test]
async fn personal_model_http_distinguishes_octos_profiles_with_the_same_model() {
    let f=Fixture::new().await;
    let first=format!("octos-managed:shared-home:{}:first","a".repeat(64));
    let second=format!("octos-managed:shared-home:{}:second","b".repeat(64));
    let row=|id:&str,reference:&str| json!({"id":id,"model":"local-model","displayName":format!("{id} · local-model"),"description":"Local","isDefault":id=="first","defaultReasoningEffort":"none","supportedReasoningEfforts":[{"reasoningEffort":"none","description":""}],"credentialRef":reference});
    let mut env=editable_configuration_fixture()["configuration"]["environment"].clone();
    env["configuration"]["modelProfile"]["credential_ref"]=json!(first);
    env["configuration"]["modelProfile"]["model"]=json!("local-model");
    env["configuration"]["modelProfile"]["reasoning_effort"]=json!("none");
    env["providers"][2]=json!({"runtime":"octos","installed":true,"version":"1.0","versionSupported":true,"state":"ready","authenticated":true,"accountVerified":false,"credentialRef":null,"credentialSource":"local","shared":true,"strictTokenCap":false,"nativeTools":false,"defaultModel":"local-model","models":[row("first",&first),row("second",&second)]});
    { let mut state=f.state.lock().unwrap();state.editable=true;state.observation=Some(env); }
    let m=f.manager(true).await;
    let choices=m.personal_model_choices("agent_a".into()).await.unwrap();
    let octos=choices.models.iter().filter(|v|v.runtime=="octos").collect::<Vec<_>>();
    assert_eq!(octos.len(),2);
    assert_eq!(octos[0].profile_id.as_deref(),Some("first"));
    assert_eq!(octos[1].profile_id.as_deref(),Some("second"));
    assert_eq!(choices.current.as_ref().unwrap().profile_id.as_deref(),Some("first"));
    let public=serde_json::to_string(&choices).unwrap();
    assert!(!public.contains("credentialRef") && !public.contains(&first) && !public.contains(&second));
    let mut draft:super::super::SavePersonalModel=serde_json::from_value(json!({"commandId":"octos_original","agentId":"agent_a","expectedGeneration":choices.generation,"expectedProfileDigest":choices.profile_digest,"runtime":"octos","model":"local-model","reasoningEffort":"none","webSearch":false,"workspace":"preserve"})).unwrap();
    assert!(matches!(m.review_personal_model(&draft,None).await,Err(e) if e.code=="configuration_model_unavailable"));
    draft.profile_id=Some("second".into());
    let review=m.review_personal_model(&draft,None).await.unwrap();
    assert_eq!(review.summary.model.profile_id.as_deref(),Some("second"));
    m.save_personal_model(review,confirmed()).await.unwrap();
    let saved=m.saved_commands().await.unwrap();
    assert_eq!(saved[0].input["profile"]["credential_ref"],second);
    f.stop().await;
}
#[tokio::test]
async fn personal_model_http_native_workspace_exact_recovery_and_private_projection() {
    let f=Fixture::new().await;f.state.lock().unwrap().editable=true;
    let m=f.manager(true).await;
    let draft=model_change(&m,"model_original").await;
    let selected=f.root.path().join("selected-native-workspace");std::fs::create_dir(&selected).unwrap();
    let review=m.review_personal_model(&draft,Some(selected.clone())).await.unwrap();
    assert_eq!(review.summary.workspace,"selected");
    assert!(!serde_json::to_string(&review.summary).unwrap().contains("selected-native"));
    f.state.lock().unwrap().lose_reply=true;
    assert!(m.save_personal_model(review,confirmed()).await.is_err());
    let original=m.saved_commands().await.unwrap();
    assert_eq!(original[0].input["profile"]["workspace_root"],std::fs::canonicalize(&selected).unwrap().to_str().unwrap());
    assert!(original[0].reply.is_none());
    assert_eq!(f.state.lock().unwrap().model_effects,1);
    // Reopened management instance and changed target generation recover the
    // same historical command, including its private selected native folder.
    m.retire();let m=f.manager(true).await;
    f.state.lock().unwrap().agent_generation=Some(4);
    let got=m.retry_saved_command("model_original".into()).await.unwrap();
    assert_eq!(got["data"]["saved"],true);
    assert!(!got.to_string().contains("private")&&!got.to_string().contains("workspace_root"));
    assert_eq!(f.state.lock().unwrap().model_effects,1);
    assert_eq!(m.saved_commands().await.unwrap()[0].input,original[0].input);
    assert_eq!(f.state.lock().unwrap().requests.len(),1);
    f.stop().await;
}
#[tokio::test]
async fn personal_model_http_permission_stale_digest_and_retired_review_never_write() {
    let f=Fixture::new().await;f.state.lock().unwrap().editable=true;
    let m=f.manager(true).await;let denied=f.manager(false).await;
    assert!(denied.personal_model_choices("agent_a".into()).await.is_err());
    let mut draft=model_change(&m,"model_denied").await;
    assert!(denied.review_personal_model(&draft,None).await.is_err());
    draft.expected_profile_digest="0".repeat(64);
    assert!(matches!(m.review_personal_model(&draft,None).await,Err(e) if e.code=="personal_model_changed"));
    let draft=model_change(&m,"model_retired").await;
    let review=m.review_personal_model(&draft,None).await.unwrap();
    m.retire();assert!(m.save_personal_model(review,confirmed()).await.is_err());
    assert_eq!(f.state.lock().unwrap().model_effects,0);
    assert_eq!(f.state.lock().unwrap().requests.len(),0);
    f.stop().await;
}
#[tokio::test]
async fn personal_rules_http_preserves_other_policy_and_recovers_exact_native_rules() {
    let f=Fixture::new().await;f.state.lock().unwrap().editable=true;
    let m=f.manager(true).await;
    let review=m.review_personal_rules("agent_a".into(),3).await.unwrap();
    assert_eq!(review.tools,vec!["private-command"]);
    let selected=f.root.path().join("native-rules");std::fs::create_dir(&selected).unwrap();
    f.state.lock().unwrap().lose_reply=true;
    assert!(m.save_personal_rules(review,"rules_original".into(),vec!["execute".into()],vec![selected.clone()],confirmed()).await.is_err());
    let saved=m.saved_commands().await.unwrap();let policy=&saved[0].input["policy"];
    assert_eq!(policy["budget"],json!({"limit":{"Tokens":100},"period":"UtcMonth"}));
    assert_eq!(policy["requests"],"AskOwner");
    assert_eq!(policy["high_risk"]["AllowWithRules"]["tools"],json!(["execute"]));
    std::fs::remove_dir(selected).unwrap(); // retry must not rebuild/re-canonicalize original rules
    let got=m.retry_saved_command("rules_original".into()).await.unwrap();
    assert_eq!(got["data"]["policyRevision"],5);
    assert!(!got.to_string().contains("native-rules")&&!got.to_string().contains("high_risk"));
    assert_eq!(f.state.lock().unwrap().policy_effects,1);
    f.stop().await;
}
#[tokio::test]
async fn personal_workspace_http_original_request_recovery_and_permission() {
    let f=Fixture::new().await;let m=f.manager(true).await;let denied=f.manager(false).await;
    let input=super::super::PreparePersonalWorkspace{command_id:"workspace_original".into(),agent_id:"agent_a".into(),expected_generation:3};
    assert!(denied.prepare_personal_workspace(input.clone(),confirmed()).await.is_err());
    f.state.lock().unwrap().lose_reply=true;
    assert!(m.prepare_personal_workspace(input,confirmed()).await.is_err());
    let got=m.retry_saved_command("workspace_original".into()).await.unwrap();
    assert_eq!(got["data"]["workspacePrepared"],true);
    assert!(!got.to_string().contains("/private"));
    assert_eq!(f.state.lock().unwrap().workspace_effects,1);
    f.stop().await;
}
