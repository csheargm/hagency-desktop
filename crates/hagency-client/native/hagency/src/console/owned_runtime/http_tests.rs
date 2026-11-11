//! Isolated real TCP/typed-transport runtime test. Provider is an explicit fake
//! subprocess; no production provider credential or Matrix room is used.
use super::super::native::{MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner};
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const OWNER: &str = "@alice:test";
struct Source;
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            Ok(MatrixAccessToken {
                access_token: "fixture-sdk-only".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}
fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn event(id: &str, epoch: i64) -> Value {
    json!({"id":id,"bindingId":"binding-a","agentId":"agent-a","eventId":format!("${id}:test"),"roomId":"!room-a:test","requesterMxid":"@bob:test","threadRoot":"$root:test","body":"isolated fixture request","state":"offered","bindingGeneration":1,"dispatchEpoch":epoch,"dispatchDeviceId":"device-a","executionId":null,"outcome":null})
}
#[derive(Default)]
struct WireState {
    epoch: i64,
    restore_scope: bool,
    binding_states: BTreeMap<String, String>,
    no_messages: bool,
    device_generation: i64,
    restart_busy_remaining: usize,
    renewal_failures: usize,
    events: Vec<Value>,
    paths: Vec<String>,
    replies: usize,
    processing: usize,
    processing_input: Option<Value>,
    first_reply: Option<Value>,
    continuous: bool,
    matrix_device_keys: BTreeMap<String, Value>,
}
impl WireState {
    fn history(&self) -> Value {
        let mut entries = vec![];
        let mut witness = Sha256::new();
        witness.update(b"hagency-started-executions-v1\n");
        for d in self.events.iter().filter(|d| d["executionId"].is_string()) {
            witness.update(serde_json::to_vec(&json!([d["id"], d["executionId"]])).unwrap());
            witness.update(b"\n");
            entries.push(json!({"dispatchId":d["id"],"executionId":d["executionId"],"bindingId":d["bindingId"],"agentId":d["agentId"],"eventId":d["eventId"],"roomId":d["roomId"],"requesterMxid":d["requesterMxid"],"threadRoot":d["threadRoot"],"bindingGeneration":1,"dispatchEpoch":d["dispatchEpoch"],"dispatchDeviceId":"device-a","immutableDigest":hash(&json!([d["id"],d["bindingId"],d["agentId"],d["eventId"],d["roomId"],d["requesterMxid"],d["threadRoot"],d["body"],1]))}));
        }
        json!({"history":{"agentId":"agent-a","snapshot":{"count":entries.len(),"digest":format!("{:x}",witness.finalize())},"executions":entries,"nextCursor":null}})
    }
    fn respond(&mut self, path: &str, input: &Value, origin: &str, unknown: bool) -> (u16, Value) {
        self.paths.push(path.into());
        let path = path.split('?').next().unwrap();
        let until = wall_ms() + 20_000;
        let generation = self.device_generation.max(1);
        let lease = |epoch| json!({"lease":{"agentId":"agent-a","ownerUserId":"uid","deviceId":"device-a","deviceGeneration":generation,"epoch":epoch,"expiresAtMs":until}});
        match path {
            "/hagency/api/v1/discovery" => (
                200,
                json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":origin,"issuer":format!("{origin}auth/")}),
            ),
            "/hagency/api/v1/agents/agent-a" if self.restore_scope => (
                200,
                json!({"agent":{"id":"agent-a","ownerUserId":"uid","puppetMxid":"@agent:test","displayName":"Fixture","state":"active","generation":1,"executionDeviceId":"device-a","ownerDirectRoomId":"!room-a:test"}}),
            ),
            "/hagency/api/v1/agents/agent-a/bindings" if self.restore_scope => (
                200,
                if self.binding_states.is_empty() {
                    json!({"bindings":[{"id":"binding-a","agentId":"agent-a","projectId":null,"scopeKind":"owner_direct","roomId":"!room-a:test","state":"active","generation":1}]})
                } else {
                    json!({"bindings":self.binding_states.iter().map(|(id,state)|json!({"id":id,"agentId":"agent-a","projectId":null,"scopeKind":if id=="binding-a"{"owner_direct"}else{"project"},"roomId":format!("!room-{}:test",id.trim_start_matches("binding-")),"state":state,"generation":1})).collect::<Vec<_>>()})
                },
            ),
            "/hagency/api/v1/sessions/pasion" => (
                200,
                json!({"token":"a".repeat(64),"userId":"uid","mxid":OWNER,"validUntilMs":wall_ms()+30_000}),
            ),
            "/hagency/api/v1/identity" => (
                200,
                json!({"userId":"uid","mxid":OWNER,"subject":"sub","clientId":"sdk-client","issuer":format!("{origin}auth/"),"validUntilMs":wall_ms()+30_000}),
            ),
            "/hagency/api/v1/devices" => {
                self.device_generation += 1;
                (
                    200,
                    json!({"token":"b".repeat(64),"deviceId":"device-a","generation":self.device_generation,"validUntilMs":wall_ms()+30_000}),
                )
            }
            "/hagency/api/v1/devices/current" => (
                200,
                json!({"userId":"uid","mxid":OWNER,"subject":"sub","clientId":"sdk-client","issuer":format!("{origin}auth/"),"deviceId":"device-a","generation":generation,"validUntilMs":wall_ms()+30_000}),
            ),
            "/hagency/api/v1/sessions/current/renew" => {
                (200, json!({"validUntilMs":wall_ms()+30_000}))
            }
            "/hagency/api/v1/sessions/current" => (200, json!({})),
            "/hagency/api/v1/execution/leases/release" => (200, json!({"released":true})),
            "/hagency/api/v1/execution/history" => (200, self.history()),
            // Typed executors always prepare their Agent device and check
            // encrypted candidates before polling ordinary dispatches. Keep
            // this plaintext fixture on that real device/bootstrap path.
            "/hagency/api/v1/execution/e2ee-device" => (
                200,
                json!({"userId":"@agent:test","deviceId":format!("HAGE2EEFIXTURE{generation}"),"accessToken":"fixture-agent-sdk-only","homeserver":origin}),
            ),
            "/hagency/api/v1/execution/e2ee-candidates" => {
                assert_eq!(input["lease"]["epoch"], self.epoch);
                (200, json!({"events":[]}))
            }
            "/_matrix/client/versions" => (200, json!({"versions":["v1.11","v1.12"]})),
            "/_matrix/client/v3/keys/upload" => {
                if let Some(device) = input.get("device_keys") {
                    assert_eq!(device["user_id"], "@agent:test");
                    let id = device["device_id"].as_str().unwrap();
                    assert!(id.starts_with("HAGE2EEFIXTURE"));
                    assert!(device["keys"].as_object().unwrap().len() >= 2);
                    self.matrix_device_keys.insert(id.into(), device.clone());
                }
                (200, json!({"one_time_key_counts":{"signed_curve25519":50}}))
            }
            "/_matrix/client/v3/keys/query" => (
                200,
                json!({"device_keys":{"@agent:test":self.matrix_device_keys},"master_keys":{},"self_signing_keys":{},"user_signing_keys":{},"failures":{}}),
            ),
            "/_matrix/client/v3/sync" => (
                200,
                json!({"next_batch":"fixture-empty","rooms":{"join":{}},"to_device":{"events":[]},"device_one_time_keys_count":{"signed_curve25519":50}}),
            ),
            "/hagency/api/v1/execution/leases/acquire" => {
                assert_eq!(input["takeover"], false);
                if self.restart_busy_remaining > 0 {
                    self.restart_busy_remaining -= 1;
                    return (409, json!({"code":"agent_leased_to_another_device"}));
                }
                assert_eq!(
                    input["historySnapshot"],
                    self.history()["history"]["snapshot"]
                );
                self.epoch += 1;
                if self.events.is_empty() && !self.no_messages {
                    self.events.push(event("dispatch-a", self.epoch));
                }
                (200, lease(self.epoch))
            }
            "/hagency/api/v1/execution/leases/renew" => {
                if self.renewal_failures > 0 {
                    self.renewal_failures -= 1;
                    (503, json!({"code":"server_unavailable"}))
                } else {
                    (200, lease(self.epoch))
                }
            }
            "/hagency/api/v1/execution/events/poll" => {
                assert!(
                    input["bindingId"] == "binding-a"
                        || self
                            .binding_states
                            .contains_key(input["bindingId"].as_str().unwrap())
                );
                (
                    200,
                    json!({"events":self.events.iter().filter(|d|d["state"]=="offered").take(1).cloned().collect::<Vec<_>>()}),
                )
            }
            "/hagency/api/v1/execution/events/ack" => (200, json!({"acknowledged":true})),
            "/hagency/api/v1/execution/events/start" => {
                let d = self
                    .events
                    .iter_mut()
                    .find(|d| d["id"] == input["dispatchId"])
                    .unwrap();
                d["executionId"] = input["executionId"].clone();
                d["state"] = "running".into();
                (200, json!({"execution":{"dispatch":d,"newlyStarted":true}}))
            }
            "/hagency/api/v1/execution/events/processing" => {
                let dispatch = self
                    .events
                    .iter()
                    .find(|d| d["id"] == input["dispatchId"])
                    .unwrap();
                assert_eq!(dispatch["state"], "running");
                assert_eq!(dispatch["executionId"], input["executionId"]);
                assert_eq!(input["lease"]["epoch"], self.epoch);
                self.processing += 1;
                if self.processing == 1 {
                    self.processing_input = Some(input.clone());
                    return (503, json!({"code":"fixture_processing_response_lost"}));
                }
                if self.processing_input.as_ref().unwrap()["dispatchId"] == input["dispatchId"] {
                    assert_eq!(self.processing_input.as_ref().unwrap(), input);
                }
                (
                    200,
                    json!({"processing":{"id":"processing-a","dispatchId":input["dispatchId"],"executionId":input["executionId"],"state":"pending","matrixEventId":null}}),
                )
            }
            "/hagency/api/v1/execution/events/authorize-tool" => (
                200,
                json!({"dispatch":self.events.iter().find(|d|d["id"]==input["dispatchId"]).unwrap()}),
            ),
            "/hagency/api/v1/execution/events/finish" => {
                let d = self
                    .events
                    .iter_mut()
                    .find(|d| d["id"] == input["dispatchId"])
                    .unwrap();
                d["state"] = "finished".into();
                d["outcome"] = input["outcome"].clone();
                if unknown && self.events.len() == 1 {
                    self.events.push(event("dispatch-b", self.epoch));
                }
                (200, json!({"finished":true}))
            }
            "/hagency/api/v1/execution/replies" => {
                self.replies += 1;
                if self.replies == 1 {
                    self.first_reply = Some(input.clone());
                    return (503, json!({"code":"fixture_reply_response_lost"}));
                }
                if self.first_reply.as_ref().unwrap()["reply"]["dispatchId"] == input["reply"]["dispatchId"] {
                    assert_eq!(self.first_reply.as_ref(), Some(input), "retry preserves exact reply and execution identity");
                }
                let reply = &input["reply"];
                let d = self
                    .events
                    .iter_mut()
                    .find(|d| d["id"] == reply["dispatchId"])
                    .unwrap();
                let new_reply = d["state"] != "replied";
                d["state"] = "replied".into();
                let digest = format!(
                    "{:x}",
                    Sha256::digest(
                        format!(
                            "{{\"dispatchId\":{},\"executionId\":{},\"body\":{}}}",
                            reply["dispatchId"], reply["executionId"], reply["body"]
                        )
                        .as_bytes()
                    )
                );
                let response = json!({"reply":{"id":format!("reply-{}",d["id"].as_str().unwrap()),"ownerEventId":d["id"],"agentId":"agent-a","bindingId":"binding-a","ownerUserId":"uid","roomId":"!room-a:test","puppetMxid":"@agent:test","bindingGeneration":1,"dispatchEpoch":d["dispatchEpoch"],"deliveryEpoch":self.epoch,"requesterMxid":d["requesterMxid"],"threadRoot":"$root:test","body":reply["body"],"payloadDigest":digest,"matrixTxnId":format!("hagency_{:x}",Sha256::digest(d["id"].as_str().unwrap().as_bytes())),"state":"sent","matrixEventId":"$fixture-sent:test"}});
                if self.continuous && new_reply && self.events.len() < 3 {
                    let mut next = event(if self.events.len() == 1 { "dispatch-b" } else { "dispatch-c" }, self.epoch);
                    if self.events.len() == 2 { next["requesterMxid"] = "@carol:test".into(); }
                    self.events.push(next);
                }
                (200, response)
            }
            _ => (404, json!({"code":"fixture_unknown_path"})),
        }
    }
}
async fn server(
    unknown: bool,
    processing_failure: u8,
) -> (
    String,
    Arc<StdMutex<WireState>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let base = origin.clone();
    let state = Arc::new(StdMutex::new(WireState::default()));
    let records = state.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let state = records.clone();
            let origin = base.clone();
            tokio::spawn(async move {
                let mut bytes = vec![];
                let mut chunk = [0; 4096];
                let split = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..split]).into_owned();
                let length = headers
                    .lines()
                    .find_map(|s| {
                        s.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|s| s.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < split + length {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let path = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body: Value =
                    serde_json::from_slice(&bytes[split..split + length]).unwrap_or(Value::Null);
                if path.starts_with("/hagency/api/v1/execution/") {
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains(&format!("authorization: bearer {}", "b".repeat(64)))
                    );
                }
                if path.starts_with("/_matrix/client/") && path != "/_matrix/client/versions" {
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains("authorization: bearer fixture-agent-sdk-only")
                    );
                }
                // Model the homeserver's empty long poll without a busy loop;
                // the initial sync still returns immediately for bootstrap.
                if path.starts_with("/_matrix/client/v3/sync?") && path.contains("timeout=30000") {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                let (mut code, mut value) =
                    state.lock().unwrap().respond(path, &body, &origin, unknown);
                if path.ends_with("/events/processing") {
                    if processing_failure == 1 {
                        code = 404;
                        value = json!({"code":"fixture_processing_unavailable"});
                    } else if processing_failure == 2 {
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }
                }
                let body = value.to_string();
                let _=stream.write_all(format!("HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    (origin, state, task)
}
fn provider(
    script: &std::path::Path,
    marker: &std::path::Path,
    ledger: &std::path::Path,
    unknown: bool,
) {
    let source=r##"#!/usr/bin/python3
import sys,json,time,sqlite3,os
marker=MARKER
ledger=LEDGER
unknown=UNKNOWN
for raw in sys.stdin:
 f=json.loads(raw);m=f.get('method');ident=f.get('id');p=f.get('params',{})
 def reply(v): print(json.dumps({'id':ident,'result':v}),flush=True)
 def emit(m,p): print(json.dumps({'method':m,'params':p}),flush=True)
 if m=='initialize': reply({'userAgent':'fixture/0.167.0'})
 elif m=='initialized': pass
 elif m=='account/read': reply({'requiresOpenaiAuth':True,'account':{'type':'chatgpt'}})
 elif m=='config/read':
  c={'project_doc_max_bytes':0,'skills':{'include_instructions':False,'bundled':{'enabled':False}},'include_apps_instructions':False,'include_environment_context':False,'developer_instructions':'','web_search':'live','mcp_servers':{},'features':{}}
  for n in ['shell_tool','unified_exec','code_mode_host','code_mode','hooks','plugins','multi_agent','multi_agent_v2','skill_search','skill_mcp_dependency_install','shell_snapshot','view_image','browser_use','browser_use_external','browser_use_full_cdp_access','browser_annotation_api','computer_use','in_app_browser','goals','image_generation','apps','tool_search','tool_suggest','web_search','web_search_cached','web_search_request','standalone_web_search','memory_tool']:c['features'][n]=False
  c['features']['standalone_web_search']=True
  c['features']['code_mode_host']=True
  reply({'config':c})
 elif m in ['thread/start','thread/resume']:
  thread=p.get('threadId','thread-'+str(os.getpid()));cwd=p['cwd']
  reply({'thread':{'id':thread,'cwd':cwd},'cwd':cwd,'model':p['model'],'approvalPolicy':'untrusted','approvalsReviewer':'user','sandbox':{'type':{'read-only':'readOnly','workspace-write':'workspaceWrite'}[p['sandbox']],'networkAccess':False}})
 elif m=='thread/name/set': reply({})
 elif m=='thread/read': reply({'thread':{'id':thread,'cwd':cwd,'status':{'type':'idle'}}})
 elif m=='turn/start':
  db=sqlite3.connect('file:'+ledger+'?mode=ro',uri=True)
  assert db.execute("select count(*) from calls where state='pending' and reserved=0").fetchone()[0]==1
  assert db.execute("select count(*) from accounts where held!=0").fetchone()[0]==0
  db.close()
  with open(marker,'a') as out:out.write(json.dumps({'pid':os.getpid(),'thread':thread})+'\n')
  turn='turn-'+str(len(open(marker).readlines()))
  reply({'turn':{'id':turn,'status':'inProgress'}})
  time.sleep(6)
  first_unknown = unknown and len(open(marker).readlines()) == 1
  if not first_unknown:emit('rawResponse/completed',{'threadId':thread,'turnId':turn,'responseId':'response-'+turn,'usage':{'inputTokens':3,'outputTokens':2,'cachedInputTokens':0,'reasoningOutputTokens':0,'totalTokens':5}})
  emit('item/completed',{'threadId':thread,'turnId':turn,'item':{'id':'message-a','type':'agentMessage','text':'isolated fixture answer'}})
  emit('turn/completed',{'threadId':thread,'turn':{'id':turn,'status':'invalid' if first_unknown else 'completed'}})
  if os.path.exists(marker+'.exit_once') and len(open(marker).readlines())==1:sys.exit(0)
 else: raise Exception(m)
"##.replace("MARKER",&serde_json::to_string(marker).unwrap()).replace("LEDGER",&serde_json::to_string(ledger).unwrap()).replace("UNKNOWN",if unknown{"True"}else{"False"});
    std::fs::write(script, source).unwrap();
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
}
async fn host_round(
    console: Console,
    config: StartConfig,
    path: PathBuf,
    state: Arc<StdMutex<WireState>>,
    unknown: bool,
    restart: bool,
) {
    let device = console.authorized_device().await.unwrap();
    let (initial, _binding_handle, _) = binding_work(config);
    let local_path = path.clone();
    let device_profile = profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let baseline_polls = state
        .lock()
        .unwrap()
        .paths
        .iter()
        .filter(|p| p.ends_with("/events/poll"))
        .count();
    let (commands, rx) = mpsc::unbounded_channel();
    let (stop, cancel) = watch::channel(false);
    let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
    let worker = tokio::spawn(run_host(
        console,
        device,
        path,
        initial,
        rx,
        cancel,
        stop.clone(),
        status,
    ));
    let wait_seconds = if state.lock().unwrap().continuous { 50 } else { 20 };
    let ready = tokio::time::timeout(Duration::from_secs(wait_seconds), async {
        loop {
            let done = {
                let s = state.lock().unwrap();
                if restart {
                    s.paths
                        .iter()
                        .filter(|p| p.ends_with("/leases/acquire"))
                        .count()
                        >= 2
                        && s.paths
                            .iter()
                            .filter(|p| p.ends_with("/events/poll"))
                            .count()
                            >= baseline_polls + 2
                } else if unknown {
                    s.events.len() == 2 && s.events[1]["state"] == "replied"
                } else if s.continuous {
                    s.events.len() == 3 && s.events.iter().all(|d| d["state"] == "replied")
                        && Ledger::open_scoped(&local_path, OWNER, &device_profile).unwrap()
                            .inbox_record(OWNER, "dispatch-c").is_ok_and(|r| r.state == State::Replied)
                } else {
                    s.events.first().is_some_and(|d| d["state"] == "replied")
                        && Ledger::open_scoped(&local_path, OWNER, &device_profile)
                            .unwrap()
                            .inbox_record(OWNER, "dispatch-a")
                            .is_ok_and(|r| r.state == State::Replied)
                }
            };
            if done {
                break true;
            }
            if worker.is_finished() {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    stop.send_replace(true);
    drop(commands);
    let result = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(ready, Ok(true)),
        "runtime ended before completion: {result:?}; binding={:?}; paths={:?}",
        _binding_handle.status.borrow(),
        state.lock().unwrap().paths
    );
    assert!(
        result.is_ok() || matches!(result, Err(RuntimeError::Stopped)),
        "Runtime result: {result:?}; paths={:?}",
        state.lock().unwrap().paths
    );
}
async fn fixture(unknown: bool, budget: u64) {
    fixture_with_processing_failure(unknown, budget, 0).await;
}
async fn fixture_with_processing_failure(unknown: bool, budget: u64, processing_failure: u8) {
    fixture_options(unknown, budget, processing_failure, false).await;
}
async fn fixture_options(unknown: bool, budget: u64, processing_failure: u8, continuous: bool) {
    fixture_modes(unknown, budget, processing_failure, continuous, false).await;
}
async fn fixture_modes(unknown: bool, budget: u64, processing_failure: u8, continuous: bool, exit_before_followup: bool) {
    // Agent device opens share a process-wide lock. Separate these fixtures
    // before starting their clocks so another test's real SDK bootstrap cannot
    // consume the two-turn case's fixed execution/lease-renewal deadline.
    static DEVICE_FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _device_fixture = DEVICE_FIXTURE.lock().await;
    use hagency_agent_local::{
        Budget, Layer, Limit, ModelProfile, Period, Policy, RequestPolicy, ToolPolicy,
    };
    let (origin, state, server) = server(unknown, processing_failure).await;
    state.lock().unwrap().continuous = continuous;
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let native =
        NativeOwner::open_with_matrix(&root.join("state"), &origin, OWNER, Arc::new(Source))
            .await
            .unwrap();
    let console = native.test_console().clone();
    let device = console.authorized_device().await.unwrap();
    let path = ledger_path(
        &root.join("state"),
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let profile_id = profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let directory = path.parent().unwrap();
    let home = directory.join("provider-home");
    let codex_home = directory.join("codex-home");
    let workspace = root.join("workspace");
    for d in [&home, &codex_home, &workspace] {
        std::fs::create_dir(d).unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let script = root.join("fake-provider");
    let marker = root.join("turns");
    std::fs::write(&marker, "").unwrap();
    if exit_before_followup { std::fs::write(marker.with_extension("exit_once"), "").unwrap(); }
    provider(&script, &marker, &path, unknown);
    let scope = inbox::Dispatch {
        id: "dispatch-a".into(),
        binding_id: "binding-a".into(),
        agent_id: "agent-a".into(),
        event_id: "$dispatch-a:test".into(),
        room_id: "!room-a:test".into(),
        requester_mxid: "@bob:test".into(),
        thread_root: "$root:test".into(),
        body: "isolated fixture request".into(),
        state: "offered".into(),
        binding_generation: 1,
        dispatch_epoch: Some(1),
        dispatch_device_id: Some("device-a".into()),
        execution_id: None,
        outcome: None,
    }
    .scope();
    let mut ledger = Ledger::open_scoped(&path, OWNER, &profile_id).unwrap();
    ledger.register_binding(OWNER, &scope).unwrap();
    ledger
        .set_model_profile(
            OWNER,
            &scope,
            &ModelProfile {
                model: "test-model".into(),
                credential_ref: "keychain:fixture".into(),
                workspace_root: workspace.to_string_lossy().into(),
                reasoning_effort: String::new(),
                web_search: true,
            },
        )
        .unwrap();
    ledger
        .set_policy(
            OWNER,
            &scope,
            Layer::Room,
            0,
            &Policy {
                budget: Budget {
                    limit: Limit::Tokens(budget),
                    period: Period::Lifetime,
                },
                requests: RequestPolicy::Allow,
                high_risk: ToolPolicy::Deny,
            },
        )
        .unwrap();
    if continuous {
        let mut stranger = scope.clone();
        stranger.requester = "@carol:test".into();
        ledger.set_policy(OWNER, &stranger, Layer::Requester, 0, &Policy {
            budget: Budget { limit: Limit::Unlimited, period: Period::Lifetime },
            requests: RequestPolicy::Allow, high_risk: ToolPolicy::Deny,
        }).unwrap();
    }
    drop(ledger);
    let config = || StartConfig {
        agent_id: "agent-a".into(),
        binding_id: "binding-a".into(),
        profile: Profile {
            executable: script.clone(),
            home: home.clone(),
            codex_home: codex_home.clone(),
            cwd: workspace.clone(),
            model: "test-model".into(),
            effort: "low".into(),
            shared_auth: false,
            web_search: true,
        },
        credential_ref: "keychain:fixture".into(),
        takeover: Takeover::Never,
        host_files: continuous,
        shared_workspace: false,
        shared_workspace_pin: None,
        owner_direct: false,
        recovery_device_id: None,
        recovery_consent_id: None,
    };
    host_round(
        console.clone(),
        config(),
        path.clone(),
        state.clone(),
        unknown,
        false,
    )
    .await;
    let ledger = Ledger::open_scoped(&path, OWNER, &profile_id).unwrap();
    let expected_turns = if continuous { 3 } else if unknown { 2 } else { usize::from(budget > 0) };
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().lines().count(),
        expected_turns
    );
    if continuous {
        let turns: Vec<Value> = std::fs::read_to_string(&marker).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        if exit_before_followup {
            assert_ne!(turns[0]["pid"], turns[1]["pid"], "a dead process must be replaced before the new model call");
        } else {
            assert_eq!(turns[0]["pid"], turns[1]["pid"], "same conversation must reuse the native process");
        }
        assert_eq!(turns[0]["thread"], turns[1]["thread"]);
        assert_ne!(turns[1]["pid"], turns[2]["pid"], "a new requester must get its own process");
        assert_ne!(turns[1]["thread"], turns[2]["thread"]);
    }
    let account = ledger
        .account(&scope, Layer::Room, Period::Lifetime, now())
        .unwrap();
    if unknown {
        assert_eq!(account, (5, 0));
        let outstanding = ledger.outstanding_calls(OWNER).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].state, "unknown");
        assert_eq!(outstanding[0].reserved, 0);
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-b").unwrap().state,
            State::Replied
        );
        let reply = state.lock().unwrap().first_reply.as_ref().unwrap()["reply"]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(reply.starts_with("isolated fixture answer"));
        assert!(!reply.contains("Token"));
        assert!(!reply.contains("历史请求的费用尚待确认"));
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Unknown
        );
    } else {
        assert_eq!(account, (if budget > 0 { 5 * expected_turns as u64 } else { 0 }, 0));
        let reply = state.lock().unwrap().first_reply.as_ref().unwrap()["reply"]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        if budget > 0 {
            // The reply carries only the answer, like shared Agents.
            assert!(reply.starts_with("isolated fixture answer"));
            assert!(!reply.contains("本次用量"));
        } else {
            assert!(reply.contains("已结算用量达到本期额度"));
            assert!(reply.contains("本次未调用模型"));
            assert!(!reply.contains("本轮处理已结束"));
        }
        assert!(!reply.contains("Agent 配额"));
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Replied
        );
        drop(ledger);
        host_round(console, config(), path, state.clone(), false, true).await;
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().lines().count(),
            expected_turns
        );
    }
    {
        let s = state.lock().unwrap();
        assert!(!s.matrix_device_keys.is_empty());
        assert!(s.paths.iter().any(|p| p.ends_with("/e2ee-candidates")));
        assert!(s.paths.iter().any(|p| p.ends_with("/events/ack")));
        assert!(s.paths.iter().any(|p| p.ends_with("/events/start")));
        assert_eq!(
            s.processing,
            if continuous { 4 } else if unknown {
                3
            } else if budget > 0 {
                2
            } else {
                0
            }
        );
        if budget > 0 {
            let processing = s
                .paths
                .iter()
                .position(|p| p.ends_with("/events/processing"))
                .unwrap();
            let finish = s
                .paths
                .iter()
                .position(|p| {
                    p.ends_with(if unknown {
                        "/events/finish"
                    } else {
                        "/replies"
                    })
                })
                .unwrap();
            assert!(
                processing < finish,
                "even a fast model turn must enqueue eyes before terminal delivery"
            );
        }
        if budget > 0 {
            assert!(s.paths.iter().any(|p| p.ends_with("/leases/renew")));
        }
        assert!(
            s.paths
                .iter()
                .filter(|p| p.ends_with("/events/authorize-tool"))
                .count()
                >= if budget > 0 { 2 } else { 1 }
        );
        assert!(s.paths.iter().any(|p| p.ends_with("/leases/release")));
        if !unknown {
            assert_eq!(s.replies, if continuous { 4 } else { 2 });
        }
    }
    native.shutdown().await;
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn typed_http_runtime_replies_retries_and_restart_never_repeat_provider_turn() {
    fixture(false, 100).await;
}
#[tokio::test]
async fn typed_http_postpaid_overrun_finishes_and_recovers_without_replay() {
    // The fake native turn reports five tokens against a one-token budget.
    // Completion, delivery retry and restart must retain that exact charge.
    fixture(false, 1).await;
}
#[tokio::test]
async fn typed_http_continuous_native_session_retains_process_but_isolates_requesters() {
    fixture_options(false, 100, 0, true).await;
}
#[tokio::test]
async fn typed_http_continuous_native_session_replaces_dead_process_before_reserving() {
    fixture_modes(false, 100, 0, true, true).await;
}
#[tokio::test]
async fn typed_http_runtime_unreported_failure_stays_unresolved_and_next_provider_turn_succeeds() {
    fixture(true, 100).await;
}
#[tokio::test]
async fn typed_http_runtime_budget_notice_is_durable_without_provider_turn_or_restart_replay() {
    fixture(false, 0).await;
}

#[tokio::test]
async fn typed_http_processing_404_or_timeout_preserves_completed_reply_and_charge() {
    fixture_with_processing_failure(false, 100, 1).await;
    fixture_with_processing_failure(false, 100, 2).await;
}

#[tokio::test]
async fn typed_http_runtime_intent_recovery_rechecks_scope_and_explicit_stop_survives_fresh_manager()
 {
    let (origin, state, server) = server(false, 0).await;
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let native = NativeOwner::open_with_matrix(
        &temp.path().canonicalize().unwrap().join("state"),
        &origin,
        OWNER,
        Arc::new(Source),
    )
    .await
    .unwrap();
    let console = native.test_console();
    let device = console.authorized_device().await.unwrap();
    let (path, identity) = intent_location(console, &device).unwrap();
    let mut intent = RuntimeIntent {
        agent_id: "agent-a".into(),
        binding_id: "binding-a".into(),
        device_id: "different-device".into(),
        consent_id: new_consent_id().unwrap(),

        effort: "low".into(),
        host_files: false,
        shared_workspace: false,
        shared_workspace_pin: None,
    };
    change_intent(
        &path,
        &identity,
        "agent-a",
        "binding-a",
        Some(intent.clone()),
    )
    .unwrap();
    assert!(
        native.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    intent.device_id = device.device_id().into();
    change_intent(&path, &identity, "agent-a", "binding-a", Some(intent)).unwrap();
    // This fixture grants a device but no current Agent/binding scope. Restoring
    // consent must therefore fail before acquiring a lease or running a model.
    let recovered = native.restore_runtime_intents().await.unwrap();
    assert_eq!(recovered["recoveries"][0]["restored"], false);
    assert_eq!(read_intents(&path, &identity).unwrap().intents.len(), 1);
    assert!(
        state
            .lock()
            .unwrap()
            .paths
            .iter()
            .all(|p| !p.ends_with("/leases/acquire") && !p.ends_with("/events/start"))
    );
    let fresh = OwnedRuntime::default();
    assert_eq!(fresh.recovery_intents(console).await.unwrap().len(), 1);
    let saved = read_intents(&path, &identity).unwrap().intents.remove(0);
    let config = StartConfig {
        agent_id: saved.agent_id.clone(),
        binding_id: saved.binding_id.clone(),
        profile: Profile {
            executable: PathBuf::from("never-spawn"),
            home: PathBuf::new(),
            codex_home: PathBuf::new(),
            cwd: PathBuf::new(),
            model: "m".into(),
            effort: saved.effort.clone(),
            shared_auth: false,
            web_search: true,
        },
        credential_ref: "none".into(),
        takeover: Takeover::Never,
        host_files: saved.host_files,
        shared_workspace: saved.shared_workspace,
        shared_workspace_pin: saved.shared_workspace_pin.clone(),
        owner_direct: false,
        recovery_device_id: Some(saved.device_id.clone()),
        recovery_consent_id: Some(saved.consent_id.clone()),
    };
    assert!(verify_recovery_intent(console, &device, &config).is_ok());
    fresh.stop(console, "agent-a", "binding-a").await.unwrap();
    assert!(matches!(
        verify_recovery_intent(console, &device, &config),
        Err(RuntimeError::Stopped)
    ));
    assert!(
        OwnedRuntime::default()
            .recovery_intents(console)
            .await
            .unwrap()
            .is_empty()
    );
    native.shutdown().await;
    server.abort();
    let _ = server.await;
}

// Run the explicit binary override in an isolated child test process. Never
// mutate the parent test suite's environment or touch an installed Codex login.
#[test]
fn typed_http_runtime_intent_positive_restore_uses_isolated_fake_provider() {
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let script = root.join("fake-provider");
    let marker = root.join("model-turns");
    std::fs::write(&marker, "").unwrap();
    provider(&script, &marker, &root.join("never-opened.db"), false);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "console::owned_runtime::http_tests::typed_http_runtime_intent_positive_restore_child",
            "--ignored",
            "--nocapture",
        ])
        .env("HAGENCY_CODEX_BINARY", &script)
        .env("HAGENCY_RESTORE_TEST_MARKER", &marker)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "");
}
#[tokio::test]
#[ignore = "isolated child of typed_http_runtime_intent_positive_restore_uses_isolated_fake_provider"]
async fn typed_http_runtime_intent_positive_restore_child() {
    use hagency_agent_local::ModelProfile;
    assert!(std::env::var_os("HAGENCY_RESTORE_TEST_MARKER").is_some());
    let (origin, state, server) = server(false, 0).await;
    {
        let mut wire = state.lock().unwrap();
        wire.restore_scope = true;
        wire.no_messages = true;
    }
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("state");
    let native = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    let device = native.test_console().authorized_device().await.unwrap();
    let paths = super::super::owner_provider::paths(
        &root,
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let path = ledger_path(
        &root,
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let identity = profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let workspace = path.parent().unwrap().join("workspace");
    hagency_store::private::directory(&workspace).unwrap();
    let mut ledger = Ledger::open_scoped(&path, OWNER, &identity).unwrap();
    ledger
        .register_binding(
            OWNER,
            &Scope {
                agent: "agent-a".into(),
                binding: "binding-a".into(),
                room: "!room-a:test".into(),
                requester: OWNER.into(),
                thread: "profile".into(),
            },
        )
        .unwrap();
    ledger
        .set_agent_model_profile(
            OWNER,
            "agent-a",
            &ModelProfile {
                model: "fixture-model".into(),
                credential_ref: paths.credential_ref,
                workspace_root: workspace.to_string_lossy().into(),
                reasoning_effort: String::new(),
                web_search: true,
            },
        )
        .unwrap();
    drop(ledger);
    let input = json!({"bindingId":"binding-a","effort":"low","hostFiles":false,"takeover":false});
    native
        .execute(super::super::native::Command::Local {
            agent_id: "agent-a".into(),
            action: super::super::native::LocalAction::RuntimeStart,
            input: Some(input),
            binding_id: Some("binding-a".into()),
            requester: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.lock().unwrap().epoch < 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    native.suspend().await.unwrap();
    drop(native);
    // Normal restart preserves the exact device generation. A temporary central
    // lease conflict still uses the existing bounded retry, without model work.
    state.lock().unwrap().restart_busy_remaining = 2;
    let restarted = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    let recovery = restarted.restore_runtime_intents().await.unwrap();
    assert_eq!(recovery["recoveries"][0]["restored"], true, "{recovery}");
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.lock().unwrap().epoch < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(state.lock().unwrap().events.is_empty());
    assert_eq!(state.lock().unwrap().restart_busy_remaining, 0);
    assert_eq!(state.lock().unwrap().device_generation, 1);
    assert!(
        state
            .lock()
            .unwrap()
            .paths
            .iter()
            .all(|p| !p.ends_with("/events/start"))
    );
    // Legacy per-Room configuration is copied to preferences before global Stop
    // clears consent. The owner private Room wins over a differing project value.
    let device = restarted.test_console().authorized_device().await.unwrap();
    let (intent_path, profile_id) = intent_location(restarted.test_console(), &device).unwrap();
    let mut intents = read_intents(&intent_path, &profile_id).unwrap().intents;
    let mut direct = intents.remove(0);

    let mut project = direct.clone();
    project.binding_id = "binding-other".into();

    change_intent(
        &intent_path,
        &profile_id,
        "agent-a",
        "binding-a",
        Some(direct),
    )
    .unwrap();
    change_intent(
        &intent_path,
        &profile_id,
        "agent-a",
        "binding-other",
        Some(project),
    )
    .unwrap();
    restarted
        .test_console()
        .0
        .owned_runtime
        .stop_agent_service(restarted.test_console(), "agent-a")
        .await
        .unwrap();
    restarted.suspend().await.unwrap();
    drop(restarted);
    let stopped = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    assert!(
        stopped.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let saved = stopped
        .test_console()
        .0
        .owned_runtime
        .saved_service_options(stopped.test_console(), "agent-a")
        .await
        .unwrap()
        .unwrap();

    // Agent-wide intent starts all active Rooms under one lease, discovers a
    // newly joined Room, preserves pause, and Stop defeats restart recovery.
    use super::super::native::{Command, LocalAction};
    state.lock().unwrap().binding_states = BTreeMap::from([
        ("binding-a".into(), "active".into()),
        ("binding-b".into(), "active".into()),
        ("binding-c".into(), "suspended".into()),
    ]);
    let epoch_before = state.lock().unwrap().epoch;
    // An initial lease conflict must recover through bounded watcher retry,
    // without an owner Stop/Start or an additional model execution.
    state.lock().unwrap().restart_busy_remaining = 1;
    stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StartAgentService,
            input: Some(json!({"effort":"low"})),
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    async fn wait_binding(
        native: &NativeOwner,
        state: &Arc<StdMutex<WireState>>,
        binding: &str,
        online: bool,
    ) {
        let mut status = Value::Null;
        let ready = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                status = native
                    .execute(Command::Local {
                        agent_id: "agent-a".into(),
                        action: LocalAction::AgentServiceStatus,
                        input: None,
                        binding_id: None,
                        requester: None,
                    })
                    .await
                    .unwrap();
                let found = status["runtime"]["bindings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|child| child["bindingId"] == binding);
                if online && found.is_some_and(|child| child["phase"] == "online_chat_only")
                    || !online && found.is_none_or(|child| child["phase"] == "stopped")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await;
        assert!(
            ready.is_ok(),
            "binding {binding} did not reach online={online}: status={status}; paths={:?}",
            state.lock().unwrap().paths
        );
    }
    wait_binding(&stopped, &state, "binding-a", true).await;
    wait_binding(&stopped, &state, "binding-b", true).await;
    assert_eq!(
        state.lock().unwrap().epoch,
        epoch_before + 1,
        "Rooms must share one Agent lease"
    );
    let status = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::AgentServiceStatus,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert_eq!(status["service"]["enabled"], true);
    assert!(
        status["runtime"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|child| child["bindingId"] != "binding-c" && child["hostFileTools"] == false)
    );
    state
        .lock()
        .unwrap()
        .binding_states
        .insert("binding-d".into(), "active".into());
    wait_binding(&stopped, &state, "binding-d", true).await;
    state
        .lock()
        .unwrap()
        .binding_states
        .insert("binding-b".into(), "suspended".into());
    wait_binding(&stopped, &state, "binding-b", false).await;
    state
        .lock()
        .unwrap()
        .binding_states
        .insert("binding-b".into(), "active".into());
    wait_binding(&stopped, &state, "binding-b", true).await;
    // Restart recovers one owner/device-pinned Agent intent, rather than
    // independently replaying every Room snapshot that discovery published.
    stopped.suspend().await.unwrap();
    drop(stopped);
    let stopped = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    let recovery = stopped.restore_runtime_intents().await.unwrap();
    assert_eq!(recovery["recoveries"].as_array().unwrap().len(), 1);
    assert_eq!(recovery["recoveries"][0]["bindingId"], "*");
    assert_eq!(recovery["recoveries"][0]["restored"], true, "{recovery}");
    wait_binding(&stopped, &state, "binding-a", true).await;
    wait_binding(&stopped, &state, "binding-b", true).await;
    wait_binding(&stopped, &state, "binding-d", true).await;
    wait_binding(&stopped, &state, "binding-c", false).await;
    stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StopAgentService,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    let status = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::AgentServiceStatus,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert_eq!(status["service"]["enabled"], false);
    assert!(status["runtime"].is_null());
    let epoch_after_stop = state.lock().unwrap().epoch;
    state
        .lock()
        .unwrap()
        .binding_states
        .insert("binding-e".into(), "active".into());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(state.lock().unwrap().epoch, epoch_after_stop);
    assert!(
        stopped.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(status["service"].get("reservation").is_none());
    assert_eq!(status["service"]["effort"], "low");
    // Preferences survive a fresh host, but never represent execution consent.
    stopped.suspend().await.unwrap();
    drop(stopped);
    let stopped = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    assert!(
        stopped.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let status = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StartAgentService,
            input: Some(json!({})),
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert!(status["service"].get("reservation").is_none());
    assert_eq!(status["service"]["effort"], "low");
    wait_binding(&stopped, &state, "binding-a", true).await;
    let preferences_path = intent_path.with_file_name("agent-service-options.json");
    let saved_preferences = std::fs::read(&preferences_path).unwrap();
    hagency_store::private::replace(&preferences_path, b"{corrupt-preferences").unwrap();
    let result = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StopAgentService,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert_eq!(result["stopped"], true);
    assert_eq!(result["warning"], "service_preferences_not_preserved");
    let status = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::AgentServiceStatus,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert_eq!(status["service"]["enabled"], false);
    assert_eq!(
        status["service"]["warning"],
        "service_preferences_unavailable"
    );
    assert!(status["runtime"].is_null());
    assert!(
        stopped.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A preference write failure must have the same cancellation semantics.
    hagency_store::private::replace(&preferences_path, &saved_preferences).unwrap();
    stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StartAgentService,
            input: Some(json!({})),
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    wait_binding(&stopped, &state, "binding-a", true).await;
    std::fs::remove_file(&preferences_path).unwrap();
    let options_lock = hagency_store::private::open(
        &intent_path.with_file_name("agent-service-options.lock"),
        false,
    )
    .unwrap();
    options_lock.try_lock().unwrap();
    let result = stopped
        .execute(Command::Local {
            agent_id: "agent-a".into(),
            action: LocalAction::StopAgentService,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await
        .unwrap();
    assert_eq!(result["stopped"], true);
    assert_eq!(result["warning"], "service_preferences_not_preserved");
    assert!(
        stopped
            .test_console()
            .0
            .owned_runtime
            .status(stopped.test_console(), "agent-a")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        stopped.restore_runtime_intents().await.unwrap()["recoveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(options_lock);
    stopped.shutdown().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn heartbeat_retries_transport_failure_but_never_extends_unconfirmed_lease() {
    for recover in [true, false] {
        let (origin, state, server) = server(false, 0).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            OWNER,
            Arc::new(Source),
        )
        .await
        .unwrap();
        let console = native.test_console().clone();
        let device = console.authorized_device().await.unwrap();
        {
            let mut wire = state.lock().unwrap();
            wire.epoch = 1;
            wire.renewal_failures = if recover { 1 } else { 100 };
        }
        let lease = Lease {
            agent_id: "agent-a".into(),
            owner_user_id: "uid".into(),
            device_id: "device-a".into(),
            device_generation: 1,
            epoch: 1,
            expires_at_ms: wall_ms() + 6_000,
        };
        let (stop, receiver) = watch::channel(false);
        let failed = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let task = tokio::spawn(heartbeat(
            console,
            device,
            lease,
            stop.clone(),
            receiver,
            failed.clone(),
        ));
        if recover {
            tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    if state
                        .lock()
                        .unwrap()
                        .paths
                        .iter()
                        .filter(|p| p.ends_with("/leases/renew"))
                        .count()
                        >= 2
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("heartbeat must retry transient failure within existing lease");
            stop.send_replace(true);
            task.await.unwrap();
            assert_eq!(failed.load(std::sync::atomic::Ordering::Acquire), 0);
        } else {
            tokio::time::timeout(Duration::from_secs(7), task)
                .await
                .expect("unconfirmed lease must expire")
                .unwrap();
            assert!(*stop.borrow());
            assert_eq!(
                failed.load(std::sync::atomic::Ordering::Acquire),
                2,
                "transport deadline remains recoverable, not permanent credential revocation"
            );
        }
        native.shutdown().await;
        server.abort();
    }
}
