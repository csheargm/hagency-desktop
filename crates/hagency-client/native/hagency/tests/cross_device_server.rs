//! Real embedded Server/Pasion/Palpo plus two isolated NativeOwner processes.
//! The provider is a declared fixture subprocess. No paid inference or real
//! provider credentials are used. Run through the sibling Server smoke runner.
#![cfg(unix)]

use hagency::native_owner::{
    Command, LocalAction, MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner,
    ProviderAction,
};
use hagency_agent_local::{
    Budget, Ledger, Limit, Period, Policy, RequestPolicy, Scope, ToolPolicy,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

struct Source {
    token: String,
    client: String,
}
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            Ok(MatrixAccessToken {
                access_token: self.token.clone(),
                client_id: self.client.clone(),
            })
        })
    }
}
fn fixture() -> (PathBuf, Value) {
    let path = PathBuf::from(
        std::env::var_os("HAGENCY_REAL_CROSS_DEVICE_FIXTURE").expect("private fixture path"),
    );
    let file = hagency_store::private::open(&path, false).expect("private fixture");
    let value = serde_json::from_reader(file).expect("fixture JSON");
    (path, value)
}
async fn owner(value: &Value, key: &str, state: &Path) -> NativeOwner {
    NativeOwner::open_with_matrix(
        state,
        value["base"].as_str().unwrap(),
        value[key]["owner"].as_str().unwrap(),
        Arc::new(Source {
            token: value[key]["accessToken"].as_str().unwrap().into(),
            client: value[key]["clientId"].as_str().unwrap().into(),
        }),
    )
    .await
    .expect("real SDK-authorized owner")
}
fn local(agent: &str, action: LocalAction, input: Option<Value>) -> Command {
    Command::Local {
        agent_id: agent.into(),
        action,
        input,
        binding_id: None,
        requester: None,
    }
}
fn remote(agent: &str, generation: i64, key: &str, command: Command) -> Command {
    Command::AgentConfiguration {
        agent_id: agent.into(),
        expected_generation: generation,
        idempotency_key: key.into(),
        command: Box::new(command),
    }
}
fn no_secrets(value: &Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                assert!(
                    ![
                        "authUrl",
                        "accessToken",
                        "refreshToken",
                        "credentialStore",
                        "account",
                        "email"
                    ]
                    .contains(&key.as_str()),
                    "unexpected private field"
                );
                no_secrets(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                no_secrets(value);
            }
        }
        _ => {}
    }
}

#[tokio::test]
#[ignore = "requires isolated embedded Server and private two-grant fixture"]
async fn controller_without_local_tools() {
    let (path, value) = fixture();
    let state = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let b = owner(&value, "b", &state.path().join("state")).await;
    let agent = value["agentId"].as_str().unwrap();
    let generation = value["agentGeneration"].as_i64().unwrap();
    let status = b
        .execute(Command::AgentConfigurationStatus {
            agent_id: agent.into(),
        })
        .await
        .unwrap();
    assert_eq!(
        status["configuration"]["executionDeviceId"],
        value["deviceA"]
    );
    assert_eq!(status["configuration"]["online"], true);
    let devices = b.execute(Command::Devices).await.unwrap();
    assert_ne!(devices["currentDeviceId"], value["deviceA"]);
    let env = b
        .execute(Command::ExecutionEnvironment {
            agent_id: agent.into(),
        })
        .await
        .unwrap();
    no_secrets(&env);
    let provider = env["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["runtime"] == "claude")
        .unwrap();
    assert_eq!(
        provider["installed"], true,
        "must discover A's fixture, despite all B executables being absent"
    );
    assert!(
        provider["version"]
            .as_str()
            .is_some_and(|v| v.contains("9.8.7"))
    );
    let connected = b
        .execute(remote(
            agent,
            generation,
            "connect-a",
            Command::RuntimeProvider {
                runtime: "claude".into(),
                action: ProviderAction::UseLocal,
            },
        ))
        .await
        .unwrap();
    assert_eq!(connected["authenticated"], true);
    no_secrets(&connected);
    let models = b
        .execute(remote(
            agent,
            generation,
            "models-a",
            Command::RuntimeProvider {
                runtime: "claude".into(),
                action: ProviderAction::Models,
            },
        ))
        .await
        .unwrap();
    let model = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "sonnet")
        .unwrap();
    let profile = json!({"model":"sonnet","credential_ref":model["credentialRef"],"workspace_root":value["workspaceA"],"reasoning_effort":"medium"});
    let initial = b.execute(remote(agent, generation, "model-before-save", local(agent, LocalAction::AgentModel, None))).await.unwrap();
    let initial: Option<hagency_agent_local::ModelProfile> = serde_json::from_value(initial["modelProfile"].clone()).unwrap();
    let expected = hagency_agent_local::model_profile_digest(initial.as_ref()).unwrap();
    let mut invalid = profile.clone();
    invalid["model"] = "not-in-target-catalog".into();
    assert_eq!(
        b.execute(remote(
            agent,
            generation,
            "invalid-model",
            local(
                agent,
                LocalAction::SaveAgentModel,
                Some(json!({"profile":invalid,"expectedProfileDigest":expected}))
            )
        ))
        .await
        .unwrap_err()
        .code,
        "configuration_model_unavailable"
    );
    let mut invalid = profile.clone();
    invalid["workspace_root"] = "/hagency-device-b-only-missing/workspace".into();
    assert_eq!(
        b.execute(remote(
            agent,
            generation,
            "invalid-workspace",
            local(
                agent,
                LocalAction::SaveAgentModel,
                Some(json!({"profile":invalid,"expectedProfileDigest":expected}))
            )
        ))
        .await
        .unwrap_err()
        .code,
        "invalid_local_policy"
    );
    let saved = b
        .execute(remote(
            agent,
            generation,
            "save-model",
            local(
                agent,
                LocalAction::SaveAgentModel,
                Some(json!({"profile":profile,"expectedProfileDigest":expected})),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(saved["modelProfile"], profile);
    let replay = b
        .execute(remote(
            agent,
            generation,
            "save-model",
            local(
                agent,
                LocalAction::SaveAgentModel,
                Some(json!({"profile":profile,"expectedProfileDigest":expected})),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(
        replay["modelProfile"], profile,
        "same request returns original result"
    );
    let before = b
        .execute(remote(
            agent,
            generation,
            "read-policy",
            local(agent, LocalAction::AgentPolicy, None),
        ))
        .await
        .unwrap();
    let policy = Policy {
        budget: Budget {
            limit: Limit::Tokens(43210),
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    let saved = b
        .execute(remote(
            agent,
            generation,
            "save-policy",
            local(
                agent,
                LocalAction::SaveAgentPolicy,
                Some(json!({"expectedRevision":before["policy"]["revision"],"policy":policy})),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(
        saved["policy"]["policy"]["budget"]["limit"],
        json!({"Tokens":43210})
    );
    no_secrets(&saved);
    b.shutdown().await;
    drop(b);
    let b = owner(&value, "b", &state.path().join("state")).await;
    let reopened = b
        .execute(remote(
            agent,
            generation,
            "reopen-model",
            local(agent, LocalAction::AgentModel, None),
        ))
        .await
        .unwrap();
    assert_eq!(reopened["modelProfile"], profile);
    assert_eq!(
        reopened["policy"]["policy"]["budget"]["limit"],
        json!({"Tokens":43210})
    );
    b.shutdown().await;
    hagency_store::private::replace(
        &path.parent().unwrap().join("controller-result.json"),
        &serde_json::to_vec(&json!({"profile":profile,"passed":true})).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated embedded Server and private two-grant fixture"]
async fn real_two_device_configuration() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("HAGENCY_CROSS_DEVICE_EXECUTOR").is_none() {
        let providers =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let binary = providers.path().join("fixture-claude");
        let claude_home = providers.path().join("claude-fixture");
        std::fs::create_dir(&claude_home).unwrap();
        std::fs::write(&binary,b"#!/bin/sh\ncase \"$1\" in\n--version) echo 'Claude Code 9.8.7';;\nauth) echo '{\"loggedIn\":true,\"email\":\"must-not-leak\"}';;\n*) exit 91;;\nesac\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Set provider selectors before starting the executor process. Runtime
        // threads never race mutation of this process's ambient environment.
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "real_two_device_configuration",
                "--ignored",
                "--exact",
                "--test-threads=1",
            ])
            .env("HAGENCY_CROSS_DEVICE_EXECUTOR", "1")
            .env("HAGENCY_CLAUDE_BINARY", &binary)
            .env("CLAUDE_CONFIG_DIR", &claude_home)
            .env("HAGENCY_CODEX_BINARY", providers.path().join("no-codex"))
            .env("HAGENCY_OCTOS_BINARY", providers.path().join("no-octos"))
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "executor test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!(
            "PASS: real two-device configuration and A persistence/resolver; explicit provider fixture, no inference"
        );
        return;
    }
    let (path, mut value) = fixture();
    let state = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let workspace = state.path().join("workspace-a");
    std::fs::create_dir(&workspace).unwrap();
    let a_state = state.path().join("state");
    let a = owner(&value, "a", &a_state).await;
    let created = a.execute(Command::CreateAgent {input:json!({"displayName":"Cross-device fixture","idempotencyKey":"cross-device-create"})}).await.unwrap();
    let agent = created["creation"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = a
        .execute(Command::AgentDetails {
            agent_id: agent.clone(),
        })
        .await
        .unwrap();
    value["agentId"] = agent.clone().into();
    value["agentGeneration"] = record["agent"]["generation"].clone();
    value["deviceA"] = record["agent"]["executionDeviceId"].clone();
    value["workspaceA"] = workspace.canonicalize().unwrap().to_str().unwrap().into();
    hagency_store::private::replace(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
    let mut online = false;
    for _ in 0..30 {
        let status = a
            .execute(Command::AgentConfigurationStatus {
                agent_id: agent.clone(),
            })
            .await
            .unwrap();
        if status["configuration"]["online"] == true {
            online = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        online,
        "A must accept configuration without starting Agent execution"
    );
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "controller_without_local_tools",
            "--ignored",
            "--exact",
            "--test-threads=1",
        ])
        .env("HAGENCY_REAL_CROSS_DEVICE_FIXTURE", &path)
        .env(
            "HAGENCY_CLAUDE_BINARY",
            state.path().join("device-b-no-claude"),
        )
        .env(
            "HAGENCY_CODEX_BINARY",
            state.path().join("device-b-no-codex"),
        )
        .env(
            "HAGENCY_OCTOS_BINARY",
            state.path().join("device-b-no-octos"),
        )
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "controller test failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_reader(
        hagency_store::private::open(
            &path.parent().unwrap().join("controller-result.json"),
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let persisted = a
        .execute(local(&agent, LocalAction::AgentModel, None))
        .await
        .unwrap();
    assert_eq!(persisted["modelProfile"], result["profile"]);
    assert_eq!(
        persisted["policy"]["policy"]["budget"]["limit"],
        json!({"Tokens":43210})
    );
    // Verify the exact Ledger::model_profile resolver used by owned runtime.
    let identity = a.identity().await.unwrap();
    let directory = std::fs::read_dir(a_state.join("owned-agent-owners"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let digest = directory
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("owner_")
        .unwrap();
    let mut ledger = Ledger::open_scoped(
        directory.join("agent-local.sqlite"),
        &identity.owner,
        digest,
    )
    .unwrap();
    a.execute(Command::EnsureAgentOwnerDirect {
        agent_id: agent.clone(),
    })
    .await
    .unwrap();
    let mut binding = None;
    for _ in 0..60 {
        let bindings = a
            .execute(Command::Bindings {
                agent_id: agent.clone(),
            })
            .await
            .unwrap();
        binding = bindings["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["scopeKind"] == "owner_direct" && b["state"] == "active")
            .cloned();
        if binding.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let binding = binding.expect("real active owner-direct binding for execution resolver");
    let scope = Scope {
        agent: agent.clone(),
        binding: binding["id"].as_str().unwrap().into(),
        room: binding["roomId"].as_str().unwrap().into(),
        requester: identity.owner,
        thread: "cross-device-runtime-resolver".into(),
    };
    ledger.register_binding(&scope.requester, &scope).unwrap();
    let effective = ledger.model_profile(&scope).unwrap().unwrap();
    assert_eq!(serde_json::to_value(effective).unwrap(), result["profile"]);
    drop(ledger);
    a.shutdown().await;
    drop(a);
    let a = owner(&value, "a", &a_state).await;
    let reopened = a
        .execute(local(&agent, LocalAction::AgentModel, None))
        .await
        .unwrap();
    assert_eq!(reopened["modelProfile"], result["profile"]);
    a.shutdown().await;
    hagency_store::private::replace(
        &path.parent().unwrap().join("client-done.json"),
        b"{\"passed\":true}",
    )
    .unwrap();
    println!(
        "PASS: real two-device NativeOwner configuration, B without providers, A persistence/reopen and runtime profile resolution; fixture provider, no inference"
    );
}
