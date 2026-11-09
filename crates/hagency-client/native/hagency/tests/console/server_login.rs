use super::*;
use salvo::http::Method;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
#[path = "daemon_lifecycle.rs"]
mod daemon_lifecycle;

#[derive(Clone)]
struct OAuthFixture {
    origin: String,
    state: Arc<Mutex<OAuthState>>,
}
struct OAuthState {
    owner: String,
    active: bool,
    challenge: Option<String>,
    enrollments: usize,
    lifetime: u64,
    refresh: bool,
    refreshes: usize,
    revocations: usize,
    renewals: usize,
    token: String,
    issuer_override: Option<String>,
    revoke_unavailable: bool,
    binding_state: String,
    agent_state: String,
    management_calls: usize,
    commands: std::collections::BTreeSet<String>,
    execution_device: Option<String>,
    agent_generation: i64,
    direct_room: Option<String>,
    direct_mapping: Option<String>,
    direct_index: Value,
    direct_creates: usize,
    direct_unknown: bool,
    direct_deny: bool,
    direct_foreign: bool,
    direct_journal: Option<std::path::PathBuf>,
}
#[handler]
impl OAuthFixture {
    async fn handle(&self, depot: &mut Depot) {
        depot.insert_typed(self.clone());
    }
}
#[handler]
async fn oauth(req: &mut Request, depot: &Depot, res: &mut Response) {
    let f = depot.get_typed::<OAuthFixture>().unwrap();
    let path = req.uri().path().to_owned();
    let result = match path.as_str() {
        "/hagency/api/v1/discovery" => {
            json!({"product":"hagency-server","version":"0.1.0","capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"protocolVersion":3,"issuer":format!("{}/auth/",f.origin),"homeserver":format!("{}/",f.origin)})
        }
        "/auth/oauth2/registration" => {
            let value: Value = req.parse_json().await.unwrap();
            assert_eq!(value["token_endpoint_auth_method"], "none");
            // Pasion's Matrix DCR policy requires HTTPS application metadata,
            // even when a native client's callback uses HTTP loopback.
            assert_eq!(
                value["client_uri"],
                "https://github.com/chrislearn/hagency-client"
            );
            assert_eq!(value["application_type"], "native");
            let redirect =
                reqwest::Url::parse(value["redirect_uris"][0].as_str().unwrap()).unwrap();
            assert_eq!(redirect.port(), None);
            assert_eq!(redirect.path(), "/console/server-login/callback");
            json!({"client_id":"fixture-public-client"})
        }
        "/auth/oauth2/token" => {
            use base64::Engine;
            let payload = String::from_utf8(req.payload().await.unwrap().to_vec()).unwrap();
            let fields: std::collections::HashMap<_, _> =
                reqwest::Url::parse(&format!("http://fixture/?{payload}"))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
            if fields["grant_type"] == "refresh_token" {
                assert_eq!(fields["client_id"], "fixture-public-client");
                assert_eq!(fields["refresh_token"], "oauth-refresh-secret");
                let mut state = f.state.lock().unwrap();
                if !state.active {
                    res.status_code(StatusCode::BAD_REQUEST);
                    res.render(Json(json!({"error":"invalid_grant"})));
                    return;
                }
                state.refreshes += 1;
                state.token = "rotated-oauth-secret".into();
                return res.render(Json(json!({"access_token":state.token,"refresh_token":"oauth-refresh-secret","token_type":"Bearer","expires_in":900})));
            }
            assert_eq!(fields["code"], "valid-code");
            let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(fields["code_verifier"].as_bytes()));
            assert_eq!(
                f.state.lock().unwrap().challenge.as_deref(),
                Some(challenge.as_str())
            );
            let state = f.state.lock().unwrap();
            let mut value = json!({"access_token":state.token,"expires_in":state.lifetime,"token_type":"Bearer"});
            if state.refresh {
                value["refresh_token"] = json!("oauth-refresh-secret");
            }
            value
        }
        "/_matrix/client/v3/joined_rooms" => {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", f.state.lock().unwrap().token)
            );
            let mut ids = (0..33)
                .map(|i| format!("!s{i:02}:example.test"))
                .collect::<Vec<_>>();
            ids.extend([
                "!0bad:example.test".into(),
                "!1room:example.test".into(),
                "!space:example.test".into(),
            ]);
            json!({"joined_rooms":ids})
        }
        p if p.starts_with("/_matrix/client/v3/rooms/")
            && p.ends_with("/state")
            && !p.contains("direct") =>
        {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", f.state.lock().unwrap().token)
            );
            let id = percent_encoding::percent_decode_str(
                p.strip_prefix("/_matrix/client/v3/rooms/")
                    .unwrap()
                    .strip_suffix("/state")
                    .unwrap(),
            )
            .decode_utf8()
            .unwrap();
            if id.starts_with("!0bad:") {
                res.status_code(StatusCode::SERVICE_UNAVAILABLE);
                return res.render(Json(json!({"code":"unavailable"})));
            }
            let owner = f.state.lock().unwrap().owner.clone();
            json!([
                {"type":"m.room.create","state_key":"","content":if id.starts_with("!1room:"){json!({})}else{json!({"type":"m.space"})}},
                {"type":"m.room.member","state_key":owner,"content":{"membership":"join"}},
                {"type":"m.room.name","state_key":"","content":{"name":"Existing Space"}}
            ])
        }
        "/hagency/api/v1/identity" => {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", "d".repeat(64))
            );
            let state = f.state.lock().unwrap();
            if !state.active {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(json!({"code":"sign_in_required"})));
                return;
            }
            json!({"userId":"stable-user-id","mxid":state.owner,"subject":state.owner,"clientId":"fixture-public-client","issuer":state.issuer_override.clone().unwrap_or_else(||format!("{}/auth/",f.origin)),"validUntilMs":until()})
        }
        "/hagency/api/v1/sessions/pasion" | "/hagency/api/v1/sessions/current/renew" => {
            let body: Value = req.parse_json().await.unwrap();
            assert_eq!(body["accessToken"], f.state.lock().unwrap().token);
            if !f.state.lock().unwrap().active {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(json!({"code":"authentication_required"})));
                return;
            }
            let mut state = f.state.lock().unwrap();
            if path.ends_with("renew") {
                state.renewals += 1;
            }
            json!({"token":"d".repeat(64),"userId":"stable-user-id","mxid":state.owner,"validUntilMs":until()})
        }
        "/hagency/api/v1/devices" => {
            if req.method() == Method::GET {
                json!({"devices":[{"id":"dev_fixture","name":"Current installation","generation":1,"revoked":false,"token":"unexpected-sensitive-field"}]})
            } else {
                let input: Value = req.parse_json().await.unwrap();
                assert!(input["ownerMxid"].is_null());
                assert!(!input["installationId"].as_str().unwrap().is_empty());
                f.state.lock().unwrap().enrollments += 1;
                json!({"deviceId":"dev_fixture","token":"e".repeat(64),"generation":1,"validUntilMs":until()})
            }
        }
        "/_matrix/client/v3/createRoom" => {
            let input: Value = req.parse_json().await.unwrap();
            assert_eq!(input["is_direct"], true);
            assert_eq!(input["visibility"], "private");
            assert_eq!(input["room_alias_name"], "hagency-agent-agt_fixture");
            assert_eq!(
                input["invite"],
                json!(["@_hagency_agt_fixture:example.test"])
            );
            let mut state = f.state.lock().unwrap();
            assert!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    == format!("Bearer {}", state.token)
            );
            let journal: Value = serde_json::from_slice(
                &std::fs::read(state.direct_journal.as_ref().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                journal["phase"], "creating",
                "intent must be durable before remote creation"
            );
            if state.direct_deny {
                state.direct_deny = false;
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(json!({"errcode":"M_FORBIDDEN"})));
                return;
            }
            state.direct_creates += 1;
            state.direct_room = Some("!direct:example.test".into());
            if state.direct_unknown {
                state.direct_unknown = false;
                res.status_code(StatusCode::SERVICE_UNAVAILABLE);
                res.render(Json(json!({"errcode":"M_UNKNOWN"})));
                return;
            }
            json!({"room_id":"!direct:example.test"})
        }
        p if p.starts_with("/_matrix/client/v3/directory/room/") => {
            let alias = percent_encoding::percent_decode_str(
                p.strip_prefix("/_matrix/client/v3/directory/room/")
                    .unwrap(),
            )
            .decode_utf8()
            .unwrap();
            assert_eq!(alias, "#hagency-agent-agt_fixture:example.test");
            let state = f.state.lock().unwrap();
            if let Some(room) = &state.direct_room {
                json!({"room_id":room})
            } else {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(json!({"errcode":"M_NOT_FOUND"})));
                return;
            }
        }
        p if p.starts_with("/_matrix/client/v3/rooms/") && p.ends_with("/state") => {
            let state = f.state.lock().unwrap();
            let owner = &state.owner;
            let marker_owner = if state.direct_foreign {
                "@foreign:example.test"
            } else {
                owner
            };
            json!([
              {"type":"m.room.create","state_key":"","sender":owner,"content":{}},
              {"type":"im.hagency.agent.owner_direct","state_key":"","sender":marker_owner,"content":{"version":1,"agentId":"agt_fixture","ownerMxid":marker_owner}},
              {"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"joined"}},
              {"type":"m.room.guest_access","state_key":"","content":{"guest_access":"forbidden"}},
              {"type":"m.room.join_rules","state_key":"","content":{"join_rule":"invite"}},
              {"type":"m.room.member","state_key":owner,"content":{"membership":"join"}},
              {"type":"m.room.member","state_key":"@_hagency_agt_fixture:example.test","content":{"membership":"invite"}}
            ])
        }
        p if p.starts_with("/_matrix/client/v3/user/") && p.ends_with("/account_data/m.direct") => {
            let input = if req.method() == Method::PUT {
                Some(req.parse_json::<Value>().await.unwrap())
            } else {
                None
            };
            let mut state = f.state.lock().unwrap();
            if let Some(index) = input {
                state.direct_index = index;
                json!({})
            } else {
                state.direct_index.clone()
            }
        }
        "/hagency/api/v1/agents/agt_fixture/owner-direct" => {
            let input = if req.method() == Method::POST {
                Some(req.parse_json::<Value>().await.unwrap())
            } else {
                None
            };
            let mut state = f.state.lock().unwrap();
            if let Some(input) = input {
                assert_eq!(
                    input["roomId"],
                    state.direct_room.as_ref().unwrap().as_str()
                );
                state.direct_mapping = Some(input["roomId"].as_str().unwrap().into());
            }
            let binding=state.direct_mapping.as_ref().map(|room|json!({"id":"bnd_direct","agentId":"agt_fixture","projectId":null,"scopeKind":"owner_direct","roomId":room,"state":"active","generation":1}));
            if req.method() == Method::POST {
                json!({"ownerDirectRoomId":state.direct_mapping,"creation":{"agent":{"id":"agt_fixture","ownerUserId":"stable-user-id","puppetMxid":"@_hagency_agt_fixture:example.test","displayName":"My Codex","state":"active","generation":1,"executionDeviceId":state.execution_device,"ownerDirectRoomId":state.direct_mapping},"binding":binding},"commandState":"active"})
            } else {
                json!({"ownerDirectRoomId":state.direct_mapping,"binding":binding})
            }
        }
        "/hagency/api/v1/agents/agt_fixture/execution-device" => {
            assert_eq!(req.method(), Method::PUT);
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", "e".repeat(64))
            );
            let input: Value = req.parse_json().await.unwrap();
            assert_eq!(input.as_object().unwrap().len(), 1);
            let mut state = f.state.lock().unwrap();
            let generation = state.agent_generation.max(1);
            if input["expectedGeneration"] != generation {
                res.status_code(StatusCode::CONFLICT);
                res.render(Json(json!({"code":"revision_conflict"})));
                return;
            }
            if state.execution_device.as_deref() != Some("dev_fixture") {
                state.agent_generation = generation + 1;
                state.execution_device = Some("dev_fixture".into());
            }
            json!({"agent":{"id":"agt_fixture","ownerUserId":"stable-user-id","puppetMxid":"@_hagency_agt_fixture:example.test","displayName":"My Codex","state":"active","executionDeviceId":state.execution_device,"generation":state.agent_generation.max(1)}})
        }
        "/hagency/api/v1/sessions/current" | "/auth/oauth2/revoke" => {
            if f.state.lock().unwrap().revoke_unavailable {
                res.status_code(StatusCode::SERVICE_UNAVAILABLE);
                res.render(Json(json!({"code":"unavailable"})));
                return;
            }
            f.state.lock().unwrap().revocations += 1;
            json!({})
        }
        "/hagency/api/v1/execution/history"
        | "/hagency/api/v1/execution/leases/acquire"
        | "/hagency/api/v1/execution/leases/renew"
        | "/hagency/api/v1/execution/leases/release"
        | "/hagency/api/v1/execution/events/poll"
        | "/hagency/api/v1/execution/events/ack"
        | "/hagency/api/v1/execution/events/start"
        | "/hagency/api/v1/execution/events/authorize-tool"
        | "/hagency/api/v1/execution/events/finish"
        | "/hagency/api/v1/execution/replies"
        | "/hagency/api/v1/execution/replies/reconcile-known" => {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", "e".repeat(64)),
                "execution requires scoped device token"
            );
            assert_eq!(req.method(), Method::POST);
            let input: Value = req.parse_json().await.unwrap();
            // A real server caps lease expiry by the device authorization.
            // Give this direct protocol fixture a short lease rather than
            // accidentally extending its earlier 30s grant under parallel load.
            let lease = json!({"agentId":"agt_fixture","ownerUserId":"stable-user-id","deviceId":"dev_fixture","deviceGeneration":1,"epoch":1,"expiresAtMs":until()-25_000});
            let mut dispatch = json!({"id":"evt_fixture","bindingId":"bnd_fixture","agentId":"agt_fixture","eventId":"$event:example.test","roomId":"!room:example.test","requesterMxid":"@requester:example.test","threadRoot":"$event:example.test","body":"Hello","state":"offered","bindingGeneration":1,"dispatchEpoch":1,"dispatchDeviceId":"dev_fixture","executionId":null,"outcome":null});
            if path.ends_with("/reconcile-known") {
                assert_eq!(input["lease"], json!({"agentId":"agt_fixture","epoch":2}));
            } else if !path.ends_with("/acquire") && !path.ends_with("/history") {
                assert_eq!(input["lease"], json!({"agentId":"agt_fixture","epoch":1}));
            }
            if path.ends_with("/history") {
                json!({"history":{"agentId":"agt_fixture","snapshot":{"count":0,"digest":"0".repeat(64)},"executions":[],"nextCursor":null}})
            } else if path.ends_with("/acquire") || path.ends_with("/renew") {
                if path.ends_with("/acquire") {
                    assert_eq!(
                        input["historySnapshot"],
                        json!({"count":0,"digest":"0".repeat(64)})
                    );
                }
                json!({"lease":lease})
            } else if path.ends_with("/release") {
                json!({"released":true})
            } else if path.ends_with("/poll") {
                assert_eq!(input["bindingId"], "bnd_fixture");
                if input["limit"] == 4 {
                    dispatch["bindingId"] = json!("bnd_other_room");
                }
                if input["limit"] == 2 {
                    dispatch["dispatchDeviceId"] = json!("dev_foreign");
                }
                if input["limit"] == 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                json!({"events":[dispatch]})
            } else if path.ends_with("/ack") {
                json!({"acknowledged":true})
            } else if path.ends_with("/start") {
                dispatch["state"] = json!("running");
                dispatch["executionId"] = input["executionId"].clone();
                json!({"execution":{"dispatch":dispatch,"newlyStarted":false}})
            } else if path.ends_with("/authorize-tool") {
                dispatch["state"] = json!("running");
                dispatch["executionId"] = input["executionId"].clone();
                if input["executionId"] == "wrong-proof" {
                    dispatch["executionId"] = json!("foreign");
                }
                json!({"dispatch":dispatch})
            } else if path.ends_with("/finish") {
                assert_eq!(input["outcome"], "unknown");
                json!({"finished":true})
            } else {
                let digest = format!(
                    "{:x}",
                    Sha256::digest(
                        format!(
                            "{{\"dispatchId\":{},\"executionId\":{},\"body\":{}}}",
                            input["reply"]["dispatchId"],
                            input["reply"]["executionId"],
                            input["reply"]["body"]
                        )
                        .as_bytes()
                    )
                );
                let body = &input["reply"]["body"];
                let reconcile = path.ends_with("/reconcile-known");
                let mut reply = json!({"id":"rpl_fixture","ownerEventId":"evt_fixture","agentId":"agt_fixture","bindingId":"bnd_fixture","ownerUserId":"stable-user-id","roomId":"!room:example.test","requesterMxid":"@requester:example.test","threadRoot":"$event:example.test","puppetMxid":"@_hagency_agt_fixture:example.test","bindingGeneration":1,"dispatchEpoch":1,"deliveryEpoch":if reconcile{2}else{1},"body":body,"payloadDigest":digest,"matrixTxnId":format!("hagency_{:x}",Sha256::digest(b"evt_fixture")),"state":"unknown","matrixEventId":null});
                if body == "Wrong epoch" {
                    reply["dispatchEpoch"] = json!(2);
                }
                if body == "Wrong delivery" {
                    reply["deliveryEpoch"] = json!(1);
                }
                if body == "Wrong body" {
                    reply["body"] = json!("Changed body");
                }
                if body == "Wrong room" {
                    reply["roomId"] = json!("!foreign:example.test");
                }
                if body == "Wrong txn" {
                    reply["matrixTxnId"] = json!("foreign");
                }
                if body == "Already sent" {
                    reply["state"] = json!("sent");
                    reply["deliveryEpoch"] = json!(1);
                    reply["matrixEventId"] = json!("$sent:example.test");
                }
                json!({"reply":reply})
            }
        }
        "/hagency/api/v1/projects/prj_fixture/service-state"
        | "/hagency/api/v1/projects/prj_fixture/rooms/%21room%3Aexample%2Etest/service-state"
        | "/hagency/api/v1/projects/prj_fixture/creation-policy"
        | "/hagency/api/v1/projects/prj_fixture/rooms/%21room%3Aexample%2Etest/creation-policy"
        | "/hagency/api/v1/projects/prj_fixture/rooms/%21room%3Aexample%2Etest/pause-service" => {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", "d".repeat(64))
            );
            if path.ends_with("/creation-policy") {
                assert_eq!(req.method(), Method::PUT);
                let input: Value = req.parse_json().await.unwrap();
                assert_eq!(input["expectedRevision"], 3);
                assert!(input["ownerMxid"].is_null());
                assert!(input["adminMxid"].is_null());
                assert!(
                    input["policy"].get("mode").is_some()
                        || input["policy"].get("defaultAllow").is_some()
                );
            } else if path.ends_with("pause-service") {
                assert_eq!(req.method(), Method::POST);
                assert!(req.payload().await.unwrap().is_empty());
            } else {
                assert_eq!(req.method(), Method::GET);
            }
            f.state.lock().unwrap().management_calls += 1;
            json!({"canManagePolicy":true,"revision":3,"creationPolicy":"{\"defaultAllow\":true,\"allow\":[],\"deny\":[]}","servicePaused":false})
        }
        p if p.starts_with("/hagency/api/v1/commands/") => {
            let suffix = p.strip_prefix("/hagency/api/v1/commands/").unwrap();
            let (operation, key) = suffix.split_once('/').unwrap();
            assert_eq!(req.method(), Method::GET);
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {}", "d".repeat(64))
            );
            let state = f.state.lock().unwrap();
            if !state.commands.contains(&format!("{operation}:{key}")) {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(json!({"code":"command_not_found"})));
                return;
            }
            let mut result = json!({"creation":{"agent":{"id":"agt_fixture","ownerUserId":"stable-user-id","puppetMxid":"@_hagency_agt_fixture:example.test","displayName":"My Codex","state":state.agent_state,"generation":1,"executionDeviceId":state.execution_device},"binding":{"id":"bnd_fixture","agentId":"agt_fixture","projectId":"prj_fixture","scopeKind":"project","roomId":"!room:example.test","state":state.binding_state,"generation":1}}});
            if operation == "agent.create" {
                result["creation"]
                    .as_object_mut()
                    .unwrap()
                    .remove("binding");
            }
            result
        }
        "/hagency/api/v1/agents"
        | "/hagency/api/v1/projects"
        | "/hagency/api/v1/agents/agt_fixture/bindings"
        | "/hagency/api/v1/agents/agt_fixture/pause"
        | "/hagency/api/v1/agents/agt_fixture/resume"
        | "/hagency/api/v1/agents/agt_fixture"
        | "/hagency/api/v1/agents/agt_other"
        | "/hagency/api/v1/agents/agt_other/bindings"
        | "/hagency/api/v1/bindings/bnd_fixture"
        | "/hagency/api/v1/bindings/bnd_fixture/pause"
        | "/hagency/api/v1/bindings/bnd_fixture/resume" => {
            assert_eq!(
                req.headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!(
                    "Bearer {}",
                    if path == "/hagency/api/v1/agents" && req.method() == Method::POST {
                        "e"
                    } else {
                        "d"
                    }
                    .repeat(64)
                ),
                "creation requires device bearer, other management uses owner session"
            );
            if path.contains("agt_other") {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(json!({"code":"owner_scope_required"})));
                return;
            }
            if req.method() == Method::POST
                && (path.ends_with("/agents") || path.ends_with("/bindings"))
            {
                let input: Value = req.parse_json().await.unwrap();
                if path.ends_with("/bindings") {
                    assert_eq!(input["projectId"], "prj_fixture");
                    assert_eq!(input["roomId"], "!room:example.test");
                } else {
                    assert!(input.get("projectId").is_none());
                    assert!(input.get("roomId").is_none());
                    assert!(input.get("deviceId").is_none());
                    f.state.lock().unwrap().execution_device = Some("dev_fixture".into());
                }
                assert!(input["ownerMxid"].is_null());
                assert!(input["modelKey"].is_null());
                let operation = if path.ends_with("/agents") {
                    "agent.create"
                } else {
                    "agent.bind"
                };
                f.state.lock().unwrap().commands.insert(format!(
                    "{operation}:{}",
                    input["idempotencyKey"].as_str().unwrap()
                ));
            }
            let mut state = f.state.lock().unwrap();
            state.management_calls += 1;
            if path.ends_with("/pause") && !path.contains("/bindings/") {
                state.agent_state = "suspended".into();
            }
            if path.ends_with("/resume") && !path.contains("/bindings/") {
                state.agent_state = "active".into();
            }
            if req.method() == Method::DELETE && !path.contains("/bindings/") {
                state.agent_state = "retiring".into();
            }
            if path.contains("/bindings/") {
                if path.ends_with("/pause") {
                    state.binding_state = "suspended".into();
                }
                if path.ends_with("/resume") {
                    state.binding_state = "active".into();
                }
                if req.method() == Method::DELETE {
                    state.binding_state = "leaving".into();
                }
            }
            let agent = json!({"id":"agt_fixture","ownerUserId":"stable-user-id","puppetMxid":"@_hagency_agt_fixture:example.test","displayName":"My Codex","state":state.agent_state,"generation":state.agent_generation.max(1),"executionDeviceId":state.execution_device,"token":"unexpected-sensitive-field"});
            let binding = json!({"id":"bnd_fixture","agentId":"agt_fixture","projectId":"prj_fixture","scopeKind":"project","roomId":"!room:example.test","state":state.binding_state,"generation":1,"token":"unexpected-sensitive-field"});
            if path.contains("/bindings/") {
                json!({"binding":binding})
            } else if path.ends_with("/projects") {
                json!({"projects":[{"id":"prj_fixture","spaceId":"!space:example.test","active":true,"revision":1,"token":"unexpected-sensitive-field"}]})
            } else if path.ends_with("/agents") && req.method() == Method::GET {
                json!({"agents":[agent]})
            } else if path.ends_with("/bindings") && req.method() == Method::GET {
                json!({"bindings":[binding]})
            } else if (path.ends_with("/agents") || path.ends_with("/bindings"))
                && req.method() == Method::POST
            {
                if path.ends_with("/agents") {
                    json!({"creation":{"agent":agent},"token":"unexpected-sensitive-field"})
                } else {
                    json!({"creation":{"agent":agent,"binding":binding},"token":"unexpected-sensitive-field"})
                }
            } else {
                json!({"agent":agent})
            }
        }
        _ => panic!("Unexpected OAuth fixture path {path}"),
    };
    res.render(Json(result));
}
fn until() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30_000
}
async fn start(service: &Service, f: &OAuthFixture, cookie: &str) -> (String, String) {
    let mut response = post("/console/server-login/start", cookie)
        .json(&json!({"server":f.origin,"name":"Workstation"}))
        .send(service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let nonce = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let url = response.take_json::<Value>().await.unwrap()["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let url = reqwest::Url::parse(&url).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["scope"].contains("urn:matrix:client:api:*"));
    f.state.lock().unwrap().challenge = Some(query["code_challenge"].clone());
    (query["state"].clone(), nonce)
}
async fn callback(service: &Service, state: &str, nonce: &str) -> Response {
    TestClient::get(format!(
        "{BASE}/console/server-login/callback?state={state}&code=valid-code"
    ))
    .add_header("host", "127.0.0.1:13300", true)
    .add_header("cookie", nonce, true)
    .send(service)
    .await
}
#[tokio::test]
async fn native_server_login_pkce_device_authorization_and_revocation() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: false,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let mut denied = post("/console/server-login/start", "")
        .json(&json!({"server":fake.origin,"name":"Workstation"}))
        .send(&service)
        .await;
    assert_eq!(
        denied.take_json::<Value>().await.unwrap()["code"],
        "local_access_required"
    );
    let local = session(&service).await;
    let (oauth_state, nonce) = start(&service, &fake, &local).await;
    let bad = callback(&service, &oauth_state, "hagency_server_login=wrong").await;
    assert!(
        !bad.headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("hagency_console="))
    );
    let reply = callback(&service, &oauth_state, &nonce).await;
    assert_eq!(reply.status_code, Some(StatusCode::SEE_OTHER));
    assert_eq!(reply.headers().get("location").unwrap(), "/console/");
    let mut observation = get("/console/server-login", &local).send(&service).await;
    let observed = observation.take_json::<Value>().await.unwrap();
    assert_eq!(
        observed["status"]["state"], "device_authorized",
        "{observed}"
    );
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("Max-Age=900"));
    let cookie = cookie.split(';').next().unwrap().to_owned();
    assert_eq!(
        get("/console/api/project-sides", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let binding: Value =
        serde_json::from_slice(&std::fs::read(state.join("server-login.json")).unwrap()).unwrap();
    assert_eq!(binding["owner"], "@owner:example.test");
    assert!(!binding.to_string().contains("secret"));
    assert!(!state.join("palpo.machine_token").exists());
    assert!(!state.join("palpo.registration.json").exists());
    assert_eq!(observed["status"]["transportOnline"], false);
    let mut status = get("/console/server-login", &cookie).send(&service).await;
    assert_eq!(
        status.take_json::<Value>().await.unwrap()["status"]["state"],
        "device_authorized"
    );
    // Bound users can sign in without bootstrap credentials. A different
    // Pasion account cannot get a new local cookie or change the binding.
    let (next, nonce) = start(&service, &fake, "").await;
    fake.state.lock().unwrap().owner = "@other:example.test".into();
    let wrong = callback(&service, &next, &nonce).await;
    assert!(
        !wrong
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("hagency_console="))
    );
    assert_eq!(fake.state.lock().unwrap().enrollments, 1);
    fake.state.lock().unwrap().owner = "@owner:example.test".into();
    fake.state.lock().unwrap().lifetime = 1;
    let (short, nonce) = start(&service, &fake, "").await;
    let reply = callback(&service, &short, &nonce).await;
    let short_cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    fake.state.lock().unwrap().lifetime = 900;
    let (long, nonce) = start(&service, &fake, "").await;
    let _ = callback(&service, &long, &nonce).await;
    assert_eq!(
        get("/console/api/project-sides", &short_cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED),
        "expired remote sessions cannot become local recovery sessions when their token is pruned"
    );
    fake.state.lock().unwrap().active = false;
    // Background renewal validates without browser activity.
    tokio::time::sleep(std::time::Duration::from_secs(31)).await;
    assert_eq!(
        get("/console/api/project-sides", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let hashes: Value =
        serde_json::from_slice(&std::fs::read(state.join("console-logins.json")).unwrap()).unwrap();
    assert_eq!(
        hashes["sessions"].as_array().unwrap().len(),
        0,
        "verified re-login revokes bootstrap authority; remote sessions never persist"
    );
    f.close().await;
    server.abort();
}

#[tokio::test]
async fn native_server_login_background_refresh_logout_and_issuer_pin() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 3,
            refresh: true,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let service = f.service();
    let local = session(&service).await;
    let (state, nonce) = start(&service, &fake, &local).await;
    let reply = callback(&service, &state, &nonce).await;
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let device = f.console.authorized_device().await.unwrap();
    assert_eq!(device.bearer().unwrap(), "e".repeat(64));
    assert_eq!(device.owner_mxid(), "@owner:example.test");
    assert!(device.valid_until() <= std::time::Instant::now() + std::time::Duration::from_secs(31));
    // No console request triggers this renewal; the host worker consumes the
    // refresh token and rotates access credentials before their expiry.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(fake.state.lock().unwrap().refreshes >= 1);
    assert!(fake.state.lock().unwrap().renewals >= 1);
    let mut status = get("/console/server-login", &cookie).send(&service).await;
    let text = status.take_json::<Value>().await.unwrap().to_string();
    assert!(!text.contains("secret"));
    assert!(!text.contains(&"e".repeat(64)));
    let replay = callback(&service, &state, &nonce).await;
    assert_eq!(replay.status_code, Some(StatusCode::BAD_REQUEST));
    let result = post("/console/server-login/sign-out", &cookie)
        .send(&service)
        .await;
    assert_eq!(result.status_code, Some(StatusCode::OK));
    assert!(f.console.authorized_device().await.is_err());
    assert!(
        device.bearer().is_err(),
        "previously obtained snapshots must stop on local logout"
    );
    assert_eq!(fake.state.lock().unwrap().revocations, 2);
    let pending: Value = serde_json::from_slice(
        &std::fs::read(f.root.path().join("state/server-login-revocations.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pending, json!([]));
    assert_eq!(
        get("/console/api/project-sides", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let (state, nonce) = start(&service, &fake, "").await;
    fake.state.lock().unwrap().issuer_override = Some("https://attacker.test/auth/".into());
    let reply = callback(&service, &state, &nonce).await;
    assert!(
        !reply
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("hagency_console="))
    );
    assert_eq!(fake.state.lock().unwrap().enrollments, 1);
    fake.state.lock().unwrap().issuer_override = None;
    let (state, nonce) = start(&service, &fake, "").await;
    let _ = callback(&service, &state, &nonce).await;
    let last = f.console.authorized_device().await.unwrap();
    fake.state.lock().unwrap().active = false;
    // The newly refreshed finite proof is cached. Observe background revocation
    // within its freshness bound, without a console request forcing renewal.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while last.bearer().is_ok() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        f.console.authorized_device().await.is_err(),
        "invalid_grant disables device capability without browser activity"
    );
    assert!(last.bearer().is_err());
    f.close().await;
    server.abort();
}

#[tokio::test]
async fn native_server_login_offline_logout_retries_after_restart_without_restoring_authority() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: true,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let service = f.service();
    let local = session(&service).await;
    let (state, nonce) = start(&service, &fake, &local).await;
    let reply = callback(&service, &state, &nonce).await;
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let device = f.console.authorized_device().await.unwrap();
    fake.state.lock().unwrap().revoke_unavailable = true;
    assert_eq!(
        post("/console/server-login/sign-out", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert!(device.bearer().is_err());
    assert!(f.console.authorized_device().await.is_err());
    let path = f.root.path().join("state/server-login-revocations.json");
    let pending: Value =
        serde_json::from_slice(&hagency_store::private::read_secret(&path).unwrap()).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["hint"], "refresh_token");
    let mut observation = get("/console/server-login", &cookie).send(&service).await;
    let status = observation.take_json::<Value>().await.unwrap();
    assert_eq!(status["status"]["remoteRevocationPending"], true);
    assert!(!status.to_string().contains("secret"));
    f.console.retire();
    fake.state.lock().unwrap().revoke_unavailable = false;
    let restarted = hagency::console::Console::with_state(Some(&f.root.path().join("state")))
    .unwrap();
    assert!(
        restarted.authorized_device().await.is_err(),
        "a persisted revoke record must never become login authority"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let pending: Value =
                serde_json::from_slice(&hagency_store::private::read_secret(&path).unwrap())
                    .unwrap();
            if pending == json!([]) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("persisted revocations should finish after connectivity returns");
    assert_eq!(fake.state.lock().unwrap().revocations, 2);
    restarted.retire();
    f.close().await;
    server.abort();
}

#[tokio::test]
async fn native_owned_agents_user_scope_local_policy_privacy_and_lifecycle() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: true,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let service = f.service();
    let local = session(&service).await;
    assert_eq!(
        get("/console/api/owned-agents", &local)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let (state, nonce) = start(&service, &fake, &local).await;
    let reply = callback(&service, &state, &nonce).await;
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    use hagency::console::device_execution::{
        DeviceOperation as Op, DeviceResponse as Reply, HistorySnapshot, Outcome, Takeover,
    };
    assert!(
        matches!(f.console.execution_api(Op::History{agent_id:"agt_fixture".into(),cursor:None,snapshot:None}).await.unwrap(),Reply::History(page) if page.snapshot.count==0 && page.executions.is_empty())
    );
    assert_eq!(
        f.console
            .execution_api(Op::History {
                agent_id: "agt_fixture".into(),
                cursor: Some("evt_fixture".into()),
                snapshot: Some(HistorySnapshot {
                    count: 1,
                    digest: "b".repeat(64)
                })
            })
            .await
            .err()
            .unwrap()
            .status,
        502,
        "history snapshot changed must fail closed"
    );
    let lease = match f
        .console
        .execution_api(Op::Acquire {
            agent_id: "agt_fixture".into(),
            ttl_ms: 30000,
            takeover: Takeover::Never,
            history_snapshot: HistorySnapshot {
                count: 0,
                digest: "0".repeat(64),
            },
        })
        .await
        .unwrap()
    {
        Reply::Lease(lease) => lease.reference(),
        _ => panic!("wrong operation response"),
    };
    assert!(matches!(
        f.console
            .execution_api(Op::Renew {
                lease: lease.clone(),
                ttl_ms: 30000
            })
            .await
            .unwrap(),
        Reply::Lease(_)
    ));
    assert!(
        matches!(f.console.execution_api(Op::Poll{binding_id:"bnd_fixture".into(),lease:lease.clone(),limit:1}).await.unwrap(),Reply::Events(events) if events.len()==1)
    );
    assert_eq!(
        f.console
            .execution_api(Op::Poll {
                binding_id: "bnd_fixture".into(),
                lease: lease.clone(),
                limit: 2
            })
            .await
            .err()
            .unwrap()
            .code,
        "execution_scope_mismatch"
    );
    assert!(
        f.console
            .execution_api(Op::Poll {
                binding_id: "bnd_fixture".into(),
                lease: lease.clone(),
                limit: 4
            })
            .await
            .is_err(),
        "foreign Room binding must fail closed even when owner/device/agent/epoch match"
    );
    assert!(matches!(
        f.console
            .execution_api(Op::Ack {
                lease: lease.clone(),
                dispatch_id: "evt_fixture".into()
            })
            .await
            .unwrap(),
        Reply::Acknowledged
    ));
    assert!(
        matches!(f.console.execution_api(Op::Start{lease:lease.clone(),dispatch_id:"evt_fixture".into(),execution_id:"exe_fixture".into()}).await.unwrap(),Reply::Started(start) if !start.newly_started)
    );
    assert!(
        matches!(f.console.execution_api(Op::AuthorizeTool{lease:lease.clone(),dispatch_id:"evt_fixture".into(),execution_id:"exe_fixture".into()}).await.unwrap(),Reply::ToolAuthorized(dispatch) if dispatch.state=="running")
    );
    assert_eq!(
        f.console
            .execution_api(Op::AuthorizeTool {
                lease: lease.clone(),
                dispatch_id: "evt_fixture".into(),
                execution_id: "wrong-proof".into()
            })
            .await
            .err()
            .unwrap()
            .code,
        "execution_scope_mismatch"
    );
    assert!(matches!(
        f.console
            .execution_api(Op::Finish {
                lease: lease.clone(),
                dispatch_id: "evt_fixture".into(),
                execution_id: "exe_fixture".into(),
                outcome: Outcome::Unknown
            })
            .await
            .unwrap(),
        Reply::Finished
    ));
    assert!(
        matches!(f.console.execution_api(Op::Reply{lease:lease.clone(),dispatch_id:"evt_fixture".into(),execution_id:"exe_fixture".into(),body:"Result".into()}).await.unwrap(),Reply::ReplyQueued(receipt) if receipt.state=="unknown")
    );
    use hagency::console::device_execution::{KnownReply, LeaseRef};
    let known = |body: &str| KnownReply {
        dispatch_id: "evt_fixture".into(),
        execution_id: "exe_fixture".into(),
        body: body.into(),
        binding_id: "bnd_fixture".into(),
        room_id: "!room:example.test".into(),
        requester_mxid: "@requester:example.test".into(),
        thread_root: "$event:example.test".into(),
        binding_generation: 1,
        original_epoch: 1,
    };
    for body in ["Known durable output", "Already sent"] {
        assert!(
            matches!(f.console.execution_api(Op::ReconcileKnownReply{lease:LeaseRef{agent_id:"agt_fixture".into(),epoch:2},known:known(body)}).await.unwrap(),Reply::ReplyQueued(receipt) if receipt.dispatch_epoch==1)
        );
    }
    for body in [
        "Wrong epoch",
        "Wrong delivery",
        "Wrong body",
        "Wrong room",
        "Wrong txn",
    ] {
        assert_eq!(
            f.console
                .execution_api(Op::ReconcileKnownReply {
                    lease: LeaseRef {
                        agent_id: "agt_fixture".into(),
                        epoch: 2
                    },
                    known: known(body)
                })
                .await
                .err()
                .unwrap()
                .code,
            "execution_scope_mismatch"
        );
    }
    // Escaping can exceed the wire budget before the 64KiB text budget.
    assert_eq!(
        f.console
            .execution_api(Op::Reply {
                lease: lease.clone(),
                dispatch_id: "evt_fixture".into(),
                execution_id: "exe_fixture".into(),
                body: "\u{0001}".repeat(30000)
            })
            .await
            .err()
            .unwrap()
            .code,
        "execution_wire_body_too_large"
    );
    assert_eq!(
        f.console
            .execution_api(Op::Poll {
                binding_id: "bnd_fixture".into(),
                lease: lease.clone(),
                limit: 101
            })
            .await
            .err()
            .unwrap()
            .code,
        "invalid_poll_limit"
    );
    assert!(matches!(
        f.console
            .execution_api(Op::Release { lease })
            .await
            .unwrap(),
        Reply::Released
    ));
    let mut response = get("/console/api/owned-agents", &cookie)
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let data = response.take_json::<Value>().await.unwrap();
    assert_eq!(data["ownerMxid"], "@owner:example.test");
    assert_eq!(data["projects"][0]["spaceId"], "!space:example.test");
    assert!(!data.to_string().contains("sensitive"));
    let mut created = post("/console/api/owned-agents", &cookie)
        .json(&json!({"displayName":"My Codex","idempotencyKey":"command_1"}))
        .send(&service)
        .await;
    assert_eq!(created.status_code, Some(StatusCode::OK));
    assert!(
        !created
            .take_json::<Value>()
            .await
            .unwrap()
            .to_string()
            .contains("sensitive")
    );
    let mut devices = get("/console/api/owned-agents/devices", &cookie)
        .send(&service)
        .await;
    assert_eq!(devices.status_code, Some(StatusCode::OK));
    let data = devices.take_json::<Value>().await.unwrap();
    assert_eq!(data["currentDeviceId"], "dev_fixture");
    assert!(!data.to_string().contains("sensitive"));
    let agent_path = "/console/api/owned-agents/agt_fixture";
    let mut initial = get(agent_path, &cookie).send(&service).await;
    assert_eq!(
        initial.take_json::<Value>().await.unwrap()["agent"]["executionDeviceId"],
        "dev_fixture"
    );
    {
        let mut state = fake.state.lock().unwrap();
        state.execution_device = Some("dev_other".into());
    }
    let denied_policy=put("/console/api/owned-agents/agt_fixture/agent-policy",&cookie)
        .json(&json!({"expectedRevision":0,"policy":{"budget":{"limit":"Unlimited","period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}})).send(&service).await;
    assert_eq!(
        denied_policy.status_code,
        Some(StatusCode::CONFLICT),
        "foreign device cannot pretend local policy is active"
    );
    let device_path = "/console/api/owned-agents/agt_fixture/execution-device";
    let assignment = json!({"expectedGeneration":1});
    let mut assigned = put(device_path, &cookie)
        .json(&assignment)
        .send(&service)
        .await;
    assert_eq!(assigned.status_code, Some(StatusCode::OK));
    let assigned = assigned.take_json::<Value>().await.unwrap();
    assert_eq!(assigned["agent"]["generation"], 2);
    assert_eq!(assigned["agent"]["executionDeviceId"], "dev_fixture");
    assert!(!assigned.to_string().contains("sensitive"));
    assert_eq!(
        put(device_path, &cookie)
            .json(&assignment)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    let mut repeated = put(device_path, &cookie)
        .json(&json!({"expectedGeneration":2}))
        .send(&service)
        .await;
    assert_eq!(
        repeated.take_json::<Value>().await.unwrap()["agent"]["generation"],
        2,
        "same device assignment is idempotent"
    );
    assert_eq!(
        put(device_path, &cookie)
            .json(&json!({"expectedGeneration":2,"deviceId":"foreign"}))
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        get(
            "/console/api/owned-agents/agt_fixture/execution-instance",
            &cookie
        )
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::METHOD_NOT_ALLOWED)
    );
    let before = fake.state.lock().unwrap().management_calls;
    assert_eq!(post("/console/api/owned-agents",&cookie).json(&json!({"displayName":"My Codex","idempotencyKey":"command_2","ownerMxid":"@other:example.test"})).send(&service).await.status_code,Some(StatusCode::BAD_REQUEST));
    assert_eq!(fake.state.lock().unwrap().management_calls, before);
    let mut journal = get("/console/api/owned-agents/commands", &cookie)
        .send(&service)
        .await;
    assert_eq!(journal.status_code, Some(StatusCode::OK));
    let records = journal.take_json::<Value>().await.unwrap();
    let original_id = records["commands"][0]["id"].as_str().unwrap();
    assert_eq!(
        records["commands"][0]["input"]["idempotencyKey"],
        "command_1"
    );
    let resumed = post(
        &format!("/console/api/owned-agents/commands/{original_id}/resume"),
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(resumed.status_code, Some(StatusCode::OK));
    assert_eq!(fake.state.lock().unwrap().commands.len(), 1);
    let duplicate = post("/console/api/owned-agents", &cookie)
        .json(&json!({"displayName":"My Codex","idempotencyKey":"new_key"}))
        .send(&service)
        .await;
    assert_eq!(duplicate.status_code, Some(StatusCode::CONFLICT));
    let agent_policy = "/console/api/owned-agents/agt_fixture/agent-policy";
    let mut initial = get(agent_policy, &cookie).send(&service).await;
    assert_eq!(initial.status_code, Some(StatusCode::OK));
    assert_eq!(
        initial.take_json::<Value>().await.unwrap()["policy"]["revision"],
        0
    );
    let update = json!({"expectedRevision":0,"policy":{"budget":{"limit":{"Tokens":2000},"period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}});
    let mut saved = put(agent_policy, &cookie)
        .json(&update)
        .send(&service)
        .await;
    assert_eq!(saved.status_code, Some(StatusCode::OK));
    assert_eq!(
        saved.take_json::<Value>().await.unwrap()["policy"]["revision"],
        1
    );
    assert_eq!(
        put(agent_policy, &cookie)
            .json(&update)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(
        get("/console/api/owned-agents/agt_other/agent-policy", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        get(&format!("{agent_policy}?bindingId=foreign"), &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let workspace = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let model_path = "/console/api/owned-agents/agt_fixture/agent-model-profile";
    let initial = get(model_path, &cookie).send(&service).await.take_json::<Value>().await.unwrap();
    let initial: Option<hagency_agent_local::ModelProfile> = serde_json::from_value(initial["modelProfile"].clone()).unwrap();
    let model = json!({"expectedProfileDigest":hagency_agent_local::model_profile_digest(initial.as_ref()).unwrap(),"profile":{"model":"gpt-5.4","credential_ref":"keychain:codex:owner","workspace_root":workspace.path().to_str().unwrap()}});
    assert_eq!(
        put(model_path, &cookie)
            .json(&model)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let mut profile = get(model_path, &cookie).send(&service).await;
    assert_eq!(
        profile.take_json::<Value>().await.unwrap()["modelProfile"]["model"],
        "gpt-5.4"
    );
    assert_eq!(
        put(
            "/console/api/owned-agents/agt_other/agent-model-profile",
            &cookie
        )
        .json(&model)
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let path = "/console/api/owned-agents/agt_fixture/local-policy?bindingId=bnd_fixture&requester=%40requester%3Aexample.test";
    let mut initial = get(path, &cookie).send(&service).await;
    assert_eq!(initial.status_code, Some(StatusCode::OK));
    let initial = initial.take_json::<Value>().await.unwrap();
    assert_eq!(
        initial["policies"][1]["policy"]["budget"]["limit"],
        "Unlimited"
    );
    assert_eq!(initial["transportOnline"], false);
    assert_eq!(
        initial["policies"][0]["policy"]["budget"]["limit"]["Tokens"],
        2000
    );
    let edit = json!({"bindingId":"bnd_fixture","requester":"@requester:example.test","layer":"requester","expectedRevision":0,"policy":{"budget":{"limit":{"Tokens":1000},"period":"UtcDay"},"requests":"Deny","high_risk":"AskOwner"}});
    let mut changed = put(
        "/console/api/owned-agents/agt_fixture/local-policy",
        &cookie,
    )
    .json(&edit)
    .send(&service)
    .await;
    assert_eq!(changed.status_code, Some(StatusCode::OK));
    let changed = changed.take_json::<Value>().await.unwrap();
    assert_eq!(changed["policies"][2]["revision"], 1);
    assert_eq!(changed["policies"][2]["policy"]["requests"], "Deny");
    assert_eq!(
        put(
            "/console/api/owned-agents/agt_fixture/local-policy",
            &cookie
        )
        .json(&edit)
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(get("/console/api/owned-agents/agt_other/local-policy?bindingId=bnd_fixture&requester=%40requester%3Aexample.test",&cookie).send(&service).await.status_code,Some(StatusCode::FORBIDDEN));
    assert_eq!(get("/console/api/owned-agents/agt_fixture/local-policy?bindingId=bnd_other&requester=%40requester%3Aexample.test",&cookie).send(&service).await.status_code,Some(StatusCode::FORBIDDEN));
    let reset = json!({"bindingId":"bnd_fixture","requester":"@requester:example.test","layer":"requester","expectedRevision":1});
    let mut reset = delete(
        "/console/api/owned-agents/agt_fixture/local-policy",
        &cookie,
    )
    .json(&reset)
    .send(&service)
    .await;
    assert_eq!(reset.status_code, Some(StatusCode::OK));
    let reset = reset.take_json::<Value>().await.unwrap();
    assert_eq!(reset["policies"][2]["revision"], 2);
    assert_eq!(reset["policies"][2]["policy"]["high_risk"], "Deny");
    assert_eq!(
        reset["policies"][2]["policy"]["budget"]["limit"],
        "Unlimited"
    );
    // Resetting a Room removes its optional limit without granting high-risk tools.
    let mut reset_room = delete(
        "/console/api/owned-agents/agt_fixture/local-policy",
        &cookie,
    )
    .json(&json!({"bindingId":"bnd_fixture","requester":"@requester:example.test","layer":"room","expectedRevision":0}))
    .send(&service)
    .await;
    assert_eq!(reset_room.status_code, Some(StatusCode::OK));
    let reset_room = reset_room.take_json::<Value>().await.unwrap();
    assert_eq!(reset_room["policies"][1]["revision"], 1);
    assert_eq!(
        reset_room["policies"][1]["policy"]["budget"]["limit"],
        "Unlimited"
    );
    assert_eq!(reset_room["policies"][1]["policy"]["requests"], "Allow");
    assert_eq!(reset_room["policies"][1]["policy"]["high_risk"], "Deny");
    let workspace = f.root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let profile = json!({"bindingId":"bnd_fixture","requester":"@requester:example.test","profile":{"model":"codex-test-model","credential_ref":"keychain:codex/test","workspace_root":workspace}});
    assert_eq!(
        put(
            "/console/api/owned-agents/agt_fixture/model-profile",
            &cookie
        )
        .json(&profile)
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::METHOD_NOT_ALLOWED)
    );
    assert_eq!(get("/console/api/owned-agents/agt_fixture/model-profile?bindingId=bnd_fixture&requester=%40requester%3Aexample.test", &cookie).send(&service).await.status_code, Some(StatusCode::METHOD_NOT_ALLOWED));
    let mut secret = profile.clone();
    secret["profile"]["modelKey"] = json!("a-secret-that-must-not-be-saved");
    assert_eq!(
        put(
            "/console/api/owned-agents/agt_fixture/model-profile",
            &cookie
        )
        .json(&secret)
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::METHOD_NOT_ALLOWED)
    );
    let unchanged = get(model_path, &cookie).send(&service).await.take_json::<Value>().await.unwrap();
    assert_eq!(unchanged["modelProfile"]["model"], "gpt-5.4", "retired scope-based writes must not replace the current profile");
    let mut unsafe_request = TestClient::put(format!(
        "{BASE}/console/api/owned-agents/agt_fixture/local-policy"
    ))
    .add_header("host", "127.0.0.1:13300", true)
    .add_header("origin", "http://attacker.test", true)
    .add_header("cookie", &cookie, true)
    .json(&edit)
    .send(&service)
    .await;
    assert_eq!(unsafe_request.status_code, Some(StatusCode::UNAUTHORIZED));
    let _ = unsafe_request.take_json::<Value>().await.unwrap();
    let owners = f.root.path().join("state/owned-agent-owners");
    let entries = std::fs::read_dir(&owners)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let directory = entries[0].path();
    assert!(
        directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("owner_")
    );
    let db = directory.join("agent-local.sqlite");
    hagency_store::private::open(&db, false).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let mut separate=get("/console/api/owned-agents/agt_fixture/local-policy?bindingId=bnd_fixture&requester=%40other%3Aexample.test",&cookie).send(&service).await;
    assert_eq!(
        separate.take_json::<Value>().await.unwrap()["policies"][2]["revision"],
        0
    );
    assert_eq!(
        post("/console/api/owned-agents/agt_fixture/pause", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(fake.state.lock().unwrap().agent_state, "suspended");
    assert_eq!(
        post("/console/api/owned-agents/agt_fixture/resume", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(fake.state.lock().unwrap().agent_state, "active");
    let owners = f.root.path().join("state/owned-agent-owners");
    let owner_dir = std::fs::read_dir(&owners)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    {
        let mut state = fake.state.lock().unwrap();
        state.direct_unknown = true;
        state.direct_deny = true;
        state.direct_journal = Some(owner_dir.join("owner-direct/agt_fixture.json"));
    }
    let ensure = "/console/api/owned-agents/agt_fixture/owner-direct/ensure";
    fake.state.lock().unwrap().agent_state = "creating".into();
    let awaiting_state = fake.state.clone();
    let become_active = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let path = awaiting_state
                .lock()
                .unwrap()
                .direct_journal
                .clone()
                .unwrap();
            if let Ok(raw) = std::fs::read(path) {
                let record: Value = serde_json::from_slice(&raw).unwrap();
                if record["phase"] == "waiting_identity" {
                    awaiting_state.lock().unwrap().agent_state = "active".into();
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "waiting identity must be durable before fresh Agent polling"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let mut denied = post(ensure, &cookie).send(&service).await;
    assert_eq!(denied.status_code, Some(StatusCode::OK));
    assert_eq!(
        denied.take_json::<Value>().await.unwrap()["ownerDirect"]["phase"],
        "new"
    );
    assert_eq!(fake.state.lock().unwrap().direct_creates, 0);
    become_active.await.unwrap();
    let mut first = post(ensure, &cookie).send(&service).await;
    assert_eq!(first.status_code, Some(StatusCode::OK));
    assert_eq!(
        first.take_json::<Value>().await.unwrap()["ownerDirect"]["phase"],
        "creating"
    );
    let mut recovered = post(ensure, &cookie).send(&service).await;
    let status = recovered.status_code;
    let direct = recovered.take_json::<Value>().await.unwrap();
    assert_eq!(status, Some(StatusCode::OK), "{direct}");
    assert_eq!(direct["ownerDirect"]["state"], "active");
    assert!(direct["creation"]["binding"]["projectId"].is_null());
    assert_eq!(
        post(ensure, &cookie).send(&service).await.status_code,
        Some(StatusCode::OK)
    );
    {
        let state = fake.state.lock().unwrap();
        assert_eq!(
            state.direct_creates, 1,
            "unknown creation and retries must not repeat createRoom"
        );
        assert_eq!(
            state.direct_index["@unrelated:example.test"],
            json!(["!keep:example.test"])
        );
        assert_eq!(
            state.direct_index["@_hagency_agt_fixture:example.test"],
            json!(["!direct:example.test"])
        );
    }
    fake.state.lock().unwrap().direct_foreign = true;
    assert_eq!(
        post(ensure, &cookie).send(&service).await.status_code,
        Some(StatusCode::CONFLICT)
    );
    assert_eq!(fake.state.lock().unwrap().direct_creates, 1);
    fake.state.lock().unwrap().direct_foreign = false;

    for (verb, expected) in [("pause", "suspended"), ("resume", "active")] {
        let mut result = post(
            &format!("/console/api/owned-agents/agt_fixture/bindings/bnd_fixture/{verb}"),
            &cookie,
        )
        .send(&service)
        .await;
        assert_eq!(result.status_code, Some(StatusCode::OK));
        let data = result.take_json::<Value>().await.unwrap();
        assert_eq!(data["binding"]["state"], expected);
        assert!(!data.to_string().contains("sensitive"));
        assert_eq!(
            fake.state.lock().unwrap().agent_state,
            "active",
            "Room action cannot affect whole agent"
        );
    }
    let mut leaving = delete(
        "/console/api/owned-agents/agt_fixture/bindings/bnd_fixture",
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(leaving.status_code, Some(StatusCode::OK));
    assert_eq!(
        leaving.take_json::<Value>().await.unwrap()["binding"]["state"],
        "leaving"
    );
    assert_eq!(
        get(
            "/console/api/owned-agents/agt_wrong/bindings/bnd_fixture",
            &cookie
        )
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        delete("/console/api/owned-agents/agt_fixture", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(fake.state.lock().unwrap().agent_state, "retiring");
    let runtime = f.console.clone();
    let inflight = tokio::spawn(async move {
        runtime
            .execution_api(Op::Poll {
                binding_id: "bnd_fixture".into(),
                lease: hagency::console::device_execution::LeaseRef {
                    agent_id: "agt_fixture".into(),
                    epoch: 1,
                },
                limit: 3,
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        post("/console/server-login/sign-out", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        inflight.await.unwrap().err().unwrap().code,
        "device_authorization_required",
        "inflight response cannot restore logged-out authority"
    );
    assert_eq!(
        f.console
            .execution_api(Op::Acquire {
                agent_id: "agt_fixture".into(),
                ttl_ms: 30000,
                takeover: Takeover::Never,
                history_snapshot: HistorySnapshot {
                    count: 0,
                    digest: "0".repeat(64)
                },
            })
            .await
            .err()
            .unwrap()
            .status,
        401
    );
    f.close().await;
    server.abort();
}

/// Uses an already-running isolated server. The fixture is an owner-only JSON
/// file with {base, username, password}; never print its credentials or tokens.
#[tokio::test]
#[ignore = "requires isolated embedded Palpo/Pasion fixture via HAGENCY_REAL_LOGIN_FIXTURE"]
async fn native_real_pasion_dcr_pkce_consent_and_user_device_grants() {
    let path = std::env::var_os("HAGENCY_REAL_LOGIN_FIXTURE").expect("fixture path");
    let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let base = fixture["base"].as_str().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut browser_cookie = String::new(); // Browser starts unauthenticated; prompt=login performs one Pasion sign-in.
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let service = f.service();
    let local = session(&service).await;
    let mut start = post("/console/server-login/start", &local)
        .json(&json!({"server":base,"name":"Real native PKCE test"}))
        .send(&service)
        .await;
    assert_eq!(start.status_code, Some(StatusCode::OK), "native DCR failed");
    let nonce = start
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let data = start.take_json::<Value>().await.unwrap();
    let authorize = data["url"].as_str().unwrap();
    let grant = client
        .get(authorize)
        .header("cookie", &browser_cookie)
        .send()
        .await
        .unwrap();
    assert!(
        grant.status().is_redirection(),
        "PKCE authorization did not redirect"
    );
    let origin = reqwest::Url::parse(base).unwrap();
    let mut next = origin
        .join(grant.headers().get("location").unwrap().to_str().unwrap())
        .unwrap();
    if next.path() != "/console/server-login/callback" {
        let grant_id = if next.path().ends_with("/login") {
            let fields = next
                .query_pairs()
                .into_owned()
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(
                fields.get("kind").map(String::as_str),
                Some("continue_authorization_grant")
            );
            let reauthenticate = client
                .post(format!("{base}/auth/api/v1/auth/login"))
                .json(&json!({"username":fixture["username"],"password":fixture["password"]}))
                .send()
                .await
                .unwrap();
            assert_eq!(reauthenticate.status(), reqwest::StatusCode::OK);
            browser_cookie = reauthenticate
                .headers()
                .get_all("set-cookie")
                .iter()
                .map(|v| v.to_str().unwrap().split(';').next().unwrap())
                .collect::<Vec<_>>()
                .join("; ");
            fields["id"].clone()
        } else {
            next.path_segments()
                .unwrap()
                .next_back()
                .unwrap()
                .to_owned()
        };
        let consent = client
            .post(format!("{base}/auth/api/v1/oauth2/consent/{grant_id}"))
            .header("cookie", &browser_cookie)
            .header("origin", base)
            .json(&json!({"action":"consent"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            consent.status(),
            reqwest::StatusCode::OK,
            "native consent failed"
        );
        let data: Value = consent.json().await.unwrap();
        assert_eq!(data["status"], "success");
        next = reqwest::Url::parse(data["redirect_url"].as_str().unwrap()).unwrap();
    }
    let mut reply = TestClient::get(next.as_str())
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("cookie", &nonce, true)
        .send(&service)
        .await;
    assert_eq!(
        reply.status_code,
        Some(StatusCode::SEE_OTHER),
        "native OAuth exchange/identity/device registration failed"
    );
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let device = f.console.authorized_device().await.unwrap();
    assert!(device.bearer().is_ok());
    let mut agents = get("/console/api/owned-agents", &cookie)
        .send(&service)
        .await;
    assert_eq!(
        agents.status_code,
        Some(StatusCode::OK),
        "real owner API user-session proof failed"
    );
    assert_eq!(
        agents.take_json::<Value>().await.unwrap()["ownerMxid"],
        device.owner_mxid()
    );
    // Exercise real server-window renewal without extending the browser cookie.
    tokio::time::sleep(std::time::Duration::from_secs(22)).await;
    assert!(
        f.console
            .authorized_device()
            .await
            .unwrap()
            .bearer()
            .is_ok()
    );
    assert_eq!(
        post("/console/server-login/sign-out", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert!(device.bearer().is_err());
    assert!(f.console.authorized_device().await.is_err());
    assert!(
        get("/console/api/owned-agents", &cookie)
            .send(&service)
            .await
            .status_code
            .unwrap()
            .is_client_error()
    );
    f.close().await;
    // Consume callback response so its full body cannot retain token material.
    let _ = reply.take_string().await;
}

#[tokio::test]
#[ignore = "requires isolated live Palpo/Pasion fixture; no model inference"]
async fn real_owner_host_creates_space_room_adopts_discovers_and_creates_agent() {
    let path = std::env::var_os("HAGENCY_REAL_LOGIN_FIXTURE").expect("fixture path");
    let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let base = fixture["base"].as_str().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut browser_cookie = String::new(); // Browser starts unauthenticated; prompt=login performs one Pasion sign-in.
    let state = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let host = hagency::owner_host::OwnerHost::open(
        &state.path().join("state"),
        "127.0.0.1:13300".parse().unwrap(),
    )
    .unwrap();
    let ticket = host
        .access_ticket()
        .unwrap();
    let service = Service::new(host.router());
    let bootstrap = exchange(&service, &ticket).await;
    assert_eq!(bootstrap.status_code, Some(StatusCode::OK));
    let local = bootstrap
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut start = post("/console/server-login/start", &local)
        .json(&json!({"server":base,"name":"Real native PKCE test"}))
        .send(&service)
        .await;
    assert_eq!(start.status_code, Some(StatusCode::OK), "native DCR failed");
    let nonce = start
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let data = start.take_json::<Value>().await.unwrap();
    let authorize = data["url"].as_str().unwrap();
    let grant = client
        .get(authorize)
        .header("cookie", &browser_cookie)
        .send()
        .await
        .unwrap();
    assert!(
        grant.status().is_redirection(),
        "PKCE authorization did not redirect"
    );
    let origin = reqwest::Url::parse(base).unwrap();
    let mut next = origin
        .join(grant.headers().get("location").unwrap().to_str().unwrap())
        .unwrap();
    if next.path() != "/console/server-login/callback" {
        let grant_id = if next.path().ends_with("/login") {
            let fields = next
                .query_pairs()
                .into_owned()
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(
                fields.get("kind").map(String::as_str),
                Some("continue_authorization_grant")
            );
            let reauthenticate = client
                .post(format!("{base}/auth/api/v1/auth/login"))
                .json(&json!({"username":fixture["username"],"password":fixture["password"]}))
                .send()
                .await
                .unwrap();
            assert_eq!(reauthenticate.status(), reqwest::StatusCode::OK);
            browser_cookie = reauthenticate
                .headers()
                .get_all("set-cookie")
                .iter()
                .map(|v| v.to_str().unwrap().split(';').next().unwrap())
                .collect::<Vec<_>>()
                .join("; ");
            fields["id"].clone()
        } else {
            next.path_segments()
                .unwrap()
                .next_back()
                .unwrap()
                .to_owned()
        };
        let consent = client
            .post(format!("{base}/auth/api/v1/oauth2/consent/{grant_id}"))
            .header("cookie", &browser_cookie)
            .header("origin", base)
            .json(&json!({"action":"consent"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            consent.status(),
            reqwest::StatusCode::OK,
            "native consent failed"
        );
        let data: Value = consent.json().await.unwrap();
        assert_eq!(data["status"], "success");
        next = reqwest::Url::parse(data["redirect_url"].as_str().unwrap()).unwrap();
    }
    let reply = TestClient::get(next.as_str())
        .add_header("host", "127.0.0.1:13300", true)
        .add_header("cookie", &nonce, true)
        .send(&service)
        .await;
    assert_eq!(
        reply.status_code,
        Some(StatusCode::SEE_OTHER),
        "native OAuth exchange/identity/device registration failed"
    );
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut space = post("/console/api/matrix-creations", &cookie)
        .json(&json!({"commandId":"real-owner-space","kind":"space","name":"Owner native Space"}))
        .send(&service)
        .await;
    assert_eq!(space.status_code, Some(StatusCode::OK));
    let space = space.take_json::<Value>().await.unwrap();
    assert_eq!(
        space["creation"]["phase"], "complete",
        "Space failure: {}",
        space["creation"]["lastError"]
    );
    let project = space["creation"]["project"]["id"].as_str().unwrap();
    let space_id = space["creation"]["roomId"].as_str().unwrap();
    let input = json!({"commandId":"real-owner-room","kind":"room","name":"Owner native discussion","projectId":project,"spaceId":space_id});
    let mut room = post("/console/api/matrix-creations", &cookie)
        .json(&input)
        .send(&service)
        .await;
    assert_eq!(room.status_code, Some(StatusCode::OK));
    let room = room.take_json::<Value>().await.unwrap();
    assert_eq!(
        room["creation"]["phase"], "complete",
        "Room failure: {}",
        room["creation"]["lastError"]
    );
    let room_id = room["creation"]["roomId"].as_str().unwrap();
    let mut replay = post("/console/api/matrix-creations", &cookie)
        .json(&input)
        .send(&service)
        .await;
    assert_eq!(replay.take_json::<Value>().await.unwrap(), room);
    let mut status = get("/console/api/matrix-creations/real-owner-room", &cookie)
        .send(&service)
        .await;
    assert_eq!(status.take_json::<Value>().await.unwrap(), room);
    let rooms_path = format!("/console/api/owner-projects/{project}/rooms");
    let mut rooms = get(&rooms_path, &cookie).send(&service).await;
    assert_eq!(rooms.status_code, Some(StatusCode::OK));
    let rooms = rooms.take_json::<Value>().await.unwrap();
    assert!(
        rooms["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["roomId"] == room_id && r["active"] == true)
    );
    let mut agents=post("/console/api/owned-agents",&cookie).json(&json!({"displayName":"Owner native Codex","idempotencyKey":format!("real-{:x}",Sha256::digest(room_id.as_bytes()))})).send(&service).await;
    let status = agents.status_code;
    let agent = agents.take_json::<Value>().await.unwrap();
    assert_eq!(status, Some(StatusCode::OK), "global Agent create: {agent}");
    let agent_id = agent["creation"]["agent"]["id"].as_str().unwrap();
    assert!(agent["creation"].get("binding").is_none());
    let ensure_path = format!("/console/api/owned-agents/{agent_id}/owner-direct/ensure");
    let mut direct_room = None;
    for _ in 0..12 {
        let mut response = post(&ensure_path, &cookie).send(&service).await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let direct = response.take_json::<Value>().await.unwrap();
        if direct["ownerDirect"]["state"] == "active" {
            assert_eq!(direct["creation"]["binding"]["scopeKind"], "owner_direct");
            assert!(direct["creation"]["binding"]["projectId"].is_null());
            direct_room = Some(direct["ownerDirect"]["roomId"].as_str().unwrap().to_owned());
            break;
        }
        assert!(
            matches!(
                direct["ownerDirect"]["phase"].as_str(),
                Some("waiting_identity" | "pending" | "server_joining")
            ),
            "unexpected DM recovery phase: {} ({})",
            direct["ownerDirect"]["phase"],
            direct["ownerDirect"]["lastError"]
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let direct_room =
        direct_room.expect("default owner-direct should become active in isolated Palpo fixture");
    let mut replay = post(&ensure_path, &cookie).send(&service).await;
    assert_eq!(replay.status_code, Some(StatusCode::OK));
    assert_eq!(
        replay.take_json::<Value>().await.unwrap()["ownerDirect"]["roomId"],
        direct_room
    );
    let mut devices = get("/console/api/owned-agents/devices", &cookie)
        .send(&service)
        .await;
    assert_eq!(devices.status_code, Some(StatusCode::OK));
    let devices = devices.take_json::<Value>().await.unwrap();
    let current_device = devices["currentDeviceId"].as_str().unwrap();
    assert!(!devices.to_string().contains("token"));
    let mut details = get(&format!("/console/api/owned-agents/{agent_id}"), &cookie)
        .send(&service)
        .await;
    assert_eq!(details.status_code, Some(StatusCode::OK));
    let details = details.take_json::<Value>().await.unwrap();
    assert_eq!(details["agent"]["executionDeviceId"], current_device);
    assert_eq!(details["agent"]["id"], agent_id);
    let bound = post(
        &format!("/console/api/owned-agents/{agent_id}/bindings"),
        &cookie,
    )
    .json(&json!({"projectId":project,"roomId":room_id,"idempotencyKey":"real-owner-bind"}))
    .send(&service)
    .await;
    assert_eq!(bound.status_code, Some(StatusCode::OK));

    let mut encoded = reqwest::Url::parse("http://matrix.invalid/").unwrap();
    encoded.path_segments_mut().unwrap().push(room_id);
    let roster_path = format!("{rooms_path}/{}/agents", &encoded.path()[1..]);
    let mut visible = false;
    for _ in 0..30 {
        let mut roster = get(&roster_path, &cookie).send(&service).await;
        assert_eq!(roster.status_code, Some(StatusCode::OK));
        let roster = roster.take_json::<Value>().await.unwrap();
        if roster["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["agentId"] == agent_id)
        {
            visible = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        visible,
        "durable Agent command did not reach active Room roster within 30 seconds"
    );
    assert_eq!(
        post("/console/server-login/sign-out", &cookie)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert!(
        get(&rooms_path, &cookie)
            .send(&service)
            .await
            .status_code
            .unwrap()
            .is_client_error()
    );
    let fixture_path =
        std::path::PathBuf::from(std::env::var_os("HAGENCY_REAL_LOGIN_FIXTURE").unwrap());
    hagency_store::private::replace(
        &fixture_path.parent().unwrap().join("client-done.json"),
        b"{\"passed\":true}",
    )
    .unwrap();
}

fn profile_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("hagency_console=") && !v.starts_with("hagency_console=;"))
        .map(|v| v.split(';').next().unwrap().to_owned())
}
#[tokio::test]
async fn native_account_profiles_switch_revoke_old_tabs_and_pin_pasion_identity() {
    async fn fixture() -> (OAuthFixture, tokio::task::JoinHandle<()>) {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let fake = OAuthFixture {
            origin: format!("http://{address}"),
            state: Arc::new(Mutex::new(OAuthState {
                owner: "@alice:example.test".into(),
                active: true,
                challenge: None,
                enrollments: 0,
                lifetime: 900,
                refresh: false,
                refreshes: 0,
                revocations: 0,
                renewals: 0,
                token: "oauth-profile-secret".into(),
                issuer_override: None,
                revoke_unavailable: false,
                binding_state: "joining".into(),
                agent_state: "creating".into(),
                management_calls: 0,
                commands: Default::default(),
                execution_device: Some("dev_fixture".into()),
                agent_generation: 1,
                direct_room: None,
                direct_mapping: None,
                direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
                direct_creates: 0,
                direct_unknown: false,
                direct_deny: false,
                direct_foreign: false,
                direct_journal: None,
            })),
        };
        let handler = Service::new(
            Router::new()
                .hoop(fake.clone())
                .push(Router::with_path("{**path}").goal(oauth)),
        );
        let acceptor = TcpListener::new(address).bind().await;
        let task = tokio::spawn(async move {
            Server::new(acceptor).serve(handler).await;
        });
        (fake, task)
    }
    async fn login(service: &Service, fake: &OAuthFixture, local: &str) -> String {
        let (state, nonce) = start(service, fake, local).await;
        profile_cookie(&callback(service, &state, &nonce).await)
            .expect("successful callback cookie")
    }
    async fn select(service: &Service, cookie: &str, id: Value) -> Response {
        post("/console/server-login/switch", cookie)
            .json(&json!({"profileId":id}))
            .send(service)
            .await
    }
    let (fake, server) = fixture().await;
    let (other, other_server) = fixture().await;
    let f = Fixture::new("127.0.0.1:13300".parse().unwrap());
    let state = f.root.path().join("state");
    let service = Service::new(f.app.clone().with_palpo_import(state.clone()).router());
    let local = session(&service).await;
    let mut stale = get("/console/server-login", "stale-browser-cookie")
        .send(&service)
        .await;
    assert_eq!(
        stale.take_json::<Value>().await.unwrap()["localAccessReady"],
        false
    );
    let mut admitted = get("/console/server-login", &local).send(&service).await;
    assert_eq!(
        admitted.take_json::<Value>().await.unwrap()["localAccessReady"],
        true
    );
    let alice = login(&service, &fake, &local).await;
    let mut response = get("/console/server-login", &alice).send(&service).await;
    let first = response.take_json::<Value>().await.unwrap();
    assert_eq!(first["localAccessReady"], true);
    let alice_id = first["activeProfileId"].clone();
    let invalid = select(&service, &alice, json!("unknown-profile")).await;
    assert_eq!(invalid.status_code, Some(StatusCode::BAD_REQUEST));
    assert_eq!(
        get("/console/api/owned-agents", &alice)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK),
        "invalid profile selection must preserve current authority"
    );
    let switch = select(&service, &alice, Value::Null).await;
    assert_eq!(switch.status_code, Some(StatusCode::OK));
    let bridge = profile_cookie(&switch).unwrap();
    assert_eq!(
        get("/console/api/owned-agents", &alice)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        get("/console/api/owned-agents", &bridge)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    fake.state.lock().unwrap().owner = "@bob:example.test".into();
    let bob = login(&service, &fake, &bridge).await;
    assert_eq!(
        get("/console/api/owned-agents", &bob)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        post("/console/server-login/sign-out", &alice)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        get("/console/api/owned-agents", &bob)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK),
        "old-profile signout must leave new owner capability active"
    );
    let mut response = get("/console/server-login", &bob).send(&service).await;
    let second = response.take_json::<Value>().await.unwrap();
    assert_ne!(second["activeProfileId"], alice_id);
    assert_eq!(second["profiles"].as_array().unwrap().len(), 2);
    // Two tabs race with the same cookie: only the winner obtains a new local bridge.
    let (a, b) = tokio::join!(
        select(&service, &bob, alice_id.clone()),
        select(&service, &bob, alice_id.clone())
    );
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| r.status_code == Some(StatusCode::OK))
            .count(),
        1
    );
    let winner = if a.status_code == Some(StatusCode::OK) {
        a
    } else {
        b
    };
    let bridge = profile_cookie(&winner).unwrap();
    let (oauth_state, nonce) = start(&service, &fake, &bridge).await;
    let wrong = callback(&service, &oauth_state, &nonce).await; // Bob cannot become Alice's profile.
    assert!(profile_cookie(&wrong).is_none());
    fake.state.lock().unwrap().owner = "@alice:example.test".into();
    let alice = login(&service, &fake, &bridge).await;
    let signed_out = post("/console/server-login/sign-out", &alice)
        .send(&service)
        .await;
    assert_eq!(signed_out.status_code, Some(StatusCode::OK));
    let local_bridge = profile_cookie(&signed_out)
        .expect("valid signout keeps finite local account selection only");
    assert_eq!(
        get("/console/api/owned-agents", &alice)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        get("/console/api/owned-agents", &local_bridge)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let switch = select(&service, &local_bridge, Value::Null).await;
    let bridge = profile_cookie(&switch).unwrap();
    let remote = login(&service, &other, &bridge).await;
    let mut response = get("/console/server-login", &remote).send(&service).await;
    let third = response.take_json::<Value>().await.unwrap();
    assert_eq!(third["profiles"].as_array().unwrap().len(), 3);
    assert_ne!(third["activeProfileId"], alice_id);
    let selected = select(&service, &remote, alice_id.clone()).await;
    assert_eq!(selected.status_code, Some(StatusCode::OK));
    let bridge = profile_cookie(&selected).unwrap();
    assert_eq!(
        get("/console/api/owned-agents", &bridge)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let restarted = Service::new(f.app.clone().with_palpo_import(state).router());
    assert_eq!(
        get("/console/api/owned-agents", &bridge)
            .send(&restarted)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    // Re-login after restart needs Pasion, not a fresh IPC browser ticket.
    // Any previously admitted full identity on that server may return, while
    // a new account must not obtain local/device authority without host admission.
    let mut observed = get("/console/server-login", "expired-browser")
        .send(&restarted)
        .await;
    assert_eq!(
        observed.take_json::<Value>().await.unwrap()["localAccessReady"],
        false
    );
    {
        let mut state = fake.state.lock().unwrap();
        state.owner = "@not-admitted:example.test".into();
        state.active = true;
    }
    let enrollments = fake.state.lock().unwrap().enrollments;
    let (state, nonce) = start(&restarted, &fake, "expired-browser").await;
    let denied = callback(&restarted, &state, &nonce).await;
    assert_eq!(denied.status_code, Some(StatusCode::SEE_OTHER));
    let mut observation = get("/console/server-login", "expired-browser")
        .send(&restarted)
        .await;
    assert_eq!(
        observation.take_json::<Value>().await.unwrap()["status"]["code"],
        "owner_mismatch"
    );
    assert!(profile_cookie(&denied).is_none());
    assert_eq!(fake.state.lock().unwrap().enrollments, enrollments);
    let mut observed = get("/console/server-login", "expired-browser")
        .send(&restarted)
        .await;
    assert_eq!(
        observed.take_json::<Value>().await.unwrap()["activeProfileId"],
        alice_id
    );
    {
        let mut state = fake.state.lock().unwrap();
        state.owner = "@bob:example.test".into();
        state.active = true;
    }
    // A server grant outage occurs after OAuth minting but before a Hagency
    // token exists. It must still revoke OAuth and must not enroll a device.
    let revocations = fake.state.lock().unwrap().revocations;
    fake.state.lock().unwrap().active = false;
    let (state, nonce) = start(&restarted, &fake, "expired-browser").await;
    let denied = callback(&restarted, &state, &nonce).await;
    assert!(profile_cookie(&denied).is_none());
    assert_eq!(fake.state.lock().unwrap().revocations, revocations + 1);
    assert_eq!(fake.state.lock().unwrap().enrollments, enrollments);
    fake.state.lock().unwrap().active = true;
    let fresh = login(&restarted, &fake, "expired-browser").await;
    assert_eq!(
        get("/console/api/owned-agents", &fresh)
            .send(&restarted)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let mut observed = get("/console/server-login", &fresh).send(&restarted).await;
    let observed = observed.take_json::<Value>().await.unwrap();
    assert_eq!(observed["localAccessReady"], true);
    assert_eq!(observed["profiles"].as_array().unwrap().len(), 3);
    assert_ne!(observed["activeProfileId"], alice_id);
    server.abort();
    other_server.abort();
}

#[tokio::test]
async fn native_owner_space_candidates_use_own_oauth_and_paginate_partial_failures() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: false,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let host = hagency::owner_host::OwnerHost::open(
        &root.path().join("state"),
        "127.0.0.1:13300".parse().unwrap(),
    )
    .unwrap();
    let ticket = host
        .access_ticket()
        .unwrap();
    let service = Service::new(host.router());
    let bootstrap = exchange(&service, &ticket).await;
    assert_eq!(bootstrap.status_code, Some(StatusCode::OK));
    let local = bootstrap
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (oauth_state, nonce) = start(&service, &fake, &local).await;
    let reply = callback(&service, &oauth_state, &nonce).await;
    assert_eq!(reply.status_code, Some(StatusCode::SEE_OTHER));
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .find(|v| v.to_str().unwrap().starts_with("hagency_console="))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    // Exercise the authenticated closed Project-management proxy, not just
    // serialization: scopes stay fixed and bodies cannot assert admin identity.
    let scope = "/console/api/owner-projects/prj_fixture";
    let mut scope_response = get(&format!("{scope}/service-state"), &cookie)
        .send(&service)
        .await;
    assert_eq!(scope_response.status_code, Some(StatusCode::OK));
    assert_eq!(
        scope_response.take_json::<Value>().await.unwrap()["canManagePolicy"],
        true
    );
    for (path, policy) in [
        (
            format!("{scope}/creation-policy"),
            json!({"defaultAllow":false,"allow":[],"deny":[]}),
        ),
        (
            format!("{scope}/rooms/%21room%3Aexample%2Etest/creation-policy"),
            json!({"mode":"disabled"}),
        ),
    ] {
        let result = put(&path, &cookie)
            .json(&json!({"expectedRevision":3,"policy":policy}))
            .send(&service)
            .await;
        assert_eq!(result.status_code, Some(StatusCode::OK));
    }
    let result = post(
        &format!("{scope}/rooms/%21room%3Aexample%2Etest/pause-service"),
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(result.status_code, Some(StatusCode::OK));
    let before_admin = fake.state.lock().unwrap().management_calls;
    let forged=put(&format!("{scope}/creation-policy"),&cookie).json(&json!({"expectedRevision":3,"policy":{"defaultAllow":true,"allow":[],"deny":[]},"adminMxid":"@admin:example.test"})).send(&service).await;
    assert_eq!(forged.status_code, Some(StatusCode::BAD_REQUEST));
    let nonempty = post(
        &format!("{scope}/rooms/%21room%3Aexample%2Etest/pause-service"),
        &cookie,
    )
    .json(&json!({}))
    .send(&service)
    .await;
    assert_eq!(nonempty.status_code, Some(StatusCode::BAD_REQUEST));
    assert_eq!(fake.state.lock().unwrap().management_calls, before_admin);

    let mut candidates = get("/console/api/owner-projects/space-candidates", &cookie)
        .send(&service)
        .await;
    assert_eq!(candidates.status_code, Some(StatusCode::OK));
    let candidates = candidates.take_json::<Value>().await.unwrap();
    assert_eq!(candidates["spaces"].as_array().unwrap().len(), 32);
    assert_eq!(candidates["spaces"][0]["name"], "Existing Space");
    assert_eq!(candidates["incomplete"], true);
    assert_eq!(candidates["errors"][0]["roomId"], "!0bad:example.test");
    let cursor = candidates["nextCursor"].as_str().unwrap();
    let encoded = percent_encoding::utf8_percent_encode(cursor, percent_encoding::NON_ALPHANUMERIC);
    let mut page2 = get(
        &format!("/console/api/owner-projects/space-candidates?cursor={encoded}"),
        &cookie,
    )
    .send(&service)
    .await;
    assert_eq!(page2.status_code, Some(StatusCode::OK));
    let page2 = page2.take_json::<Value>().await.unwrap();
    assert_eq!(page2["spaces"].as_array().unwrap().len(), 2);
    assert!(page2["nextCursor"].is_null());
    assert_eq!(
        page2["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["spaceId"] == "!space:example.test")
            .unwrap()["projectId"],
        "prj_fixture"
    );
    assert_eq!(
        get(
            "/console/api/owner-projects/space-candidates?url=https://evil.test",
            &cookie
        )
        .send(&service)
        .await
        .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        get("/console/api/owner-projects/space-candidates", "")
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );

    server.abort();
}

#[tokio::test]
async fn native_in_process_facade_pins_matrix_account_without_exposing_credentials() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: "@owner:example.test".into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: false,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: "oauth-user-secret-0123456789".into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({"@unrelated:example.test":["!keep:example.test"]}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
        })),
    };
    let handler = Service::new(
        Router::new()
            .hoop(fake.clone())
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(handler).await;
    });

    use hagency::native_owner::{Command, NativeOwner};
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let native = NativeOwner::open(
        &root.path().join("native"),
        &fake.origin,
        "@owner:example.test",
    )
    .await
    .unwrap();
    assert_eq!(
        native.execute(Command::Agents).await.unwrap_err().code,
        "owner_authorization_required"
    );
    // A valid Pasion grant for another account is rejected before owner binding/device enrollment.
    fake.state.lock().unwrap().owner = "@other:example.test".into();
    let start = native.execute(Command::BeginLogin).await.unwrap();
    let url = reqwest::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let fields = url
        .query_pairs()
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    fake.state.lock().unwrap().challenge = Some(fields["code_challenge"].clone());
    let mut callback = reqwest::Url::parse(&fields["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "valid-code");
    let response = reqwest::Client::new().get(callback).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(fake.state.lock().unwrap().enrollments, 0);
    let binding: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("native/server-login.json")).unwrap(),
    )
    .unwrap();
    assert!(binding["owner"].is_null());
    assert!(native.identity().await.is_err());
    // The actual Matrix account can authorize, query typed commands and renew local entry.
    fake.state.lock().unwrap().owner = "@owner:example.test".into();
    let start = native.execute(Command::BeginLogin).await.unwrap();
    let url = reqwest::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let fields = url
        .query_pairs()
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    fake.state.lock().unwrap().challenge = Some(fields["code_challenge"].clone());
    let mut callback = reqwest::Url::parse(&fields["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "valid-code");
    let response = reqwest::Client::new()
        .get(callback.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.headers().get("set-cookie").is_none());
    let identity = native.identity().await.unwrap();
    assert_eq!(identity.owner, "@owner:example.test");
    assert_eq!(identity.subject, "@owner:example.test");
    let agents = native.execute(Command::Agents).await.unwrap();
    assert!(!agents.to_string().contains("unexpected-sensitive-field"));
    assert!(!agents.to_string().contains("secret"));
    assert!(native.execute(Command::Projects).await.unwrap()["projects"].is_array());
    assert!(
        native.execute(Command::Creations).await.unwrap()["creations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Typed native entrypoints share the journal service and bypass TestClient.
    assert!(
        native
            .matrix_creations()
            .await
            .unwrap()
            .creations
            .is_empty()
    );
    assert_eq!(
        native
            .matrix_creation_status("absent-command".into())
            .await
            .unwrap_err()
            .code,
        "creation_command_not_found"
    );
    assert_eq!(
        native
            .matrix_creation_start(hagency::native_owner::MatrixCreationInput {
                command_id: "invalid-command".into(),
                kind: hagency::native_owner::MatrixCreationKind::Space,
                name: "x".repeat(129),
                project_id: None,
                space_id: None,
            })
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let candidates = native
        .execute(Command::SpaceCandidates { cursor: None })
        .await
        .unwrap();
    assert_eq!(candidates["spaces"].as_array().unwrap().len(), 32);
    let replay = reqwest::Client::new().get(callback).send().await.unwrap();
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);
    native.shutdown().await;
    assert!(native.execute(Command::Agents).await.is_err());
    drop(native);
    let restored = NativeOwner::open(
        &root.path().join("native"),
        &fake.origin,
        "@owner:example.test",
    )
    .await
    .unwrap();
    assert_eq!(
        restored.execute(Command::Projects).await.unwrap_err().code,
        "owner_authorization_required"
    );
    restored.shutdown().await;
    server.abort();
}
