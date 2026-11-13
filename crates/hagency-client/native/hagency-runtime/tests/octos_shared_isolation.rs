//! Opt-in real Octos/OS sandbox test; uses only a synthetic loopback model.
#![cfg(target_os = "macos")]
use hagency_runtime::{
    octos::{
        Outcome,
        profile_binding::ProfileBinding,
        session::{Event, HostTool, Limits, Permissions},
    },
    owned::{Cleanup, ManagedProcessState, OwnedOctosSession},
    questions::Answer,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn real_octos_runs_only_the_selected_profile_inside_the_room_sandbox() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner");
    let room = root.join("room");
    fs::create_dir(&owner).unwrap();
    fs::create_dir(owner.join("profiles")).unwrap();
    fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] {
        fs::create_dir(room.join(name)).unwrap();
    }
    let model_file = root.join("model.py");
    fs::write(
        &model_file,
        include_str!("../../hagency-agent-local/tests/octos_offline_model.py"),
    )
    .unwrap();
    let mut model = tokio::process::Command::new("python3")
        .arg(&model_file)
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let marker = root.join("extension-ran");
    let profile = json!({
        "id":"coding", "name":"Fixture", "enabled":true,
        "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}},
            "hooks":[{"event":"before_llm_call","command":["/usr/bin/touch",marker]}],
            "mcp_servers":[{"name":"fixture","command":"/usr/bin/touch","args":[marker]}]}
    });
    fs::write(owner.join("profiles/coding.json"), profile.to_string()).unwrap();
    let mut other = profile.clone();
    other["id"] = "other".into();
    other["config"]["env_vars"] = json!({"SECRET":"other-profile-secret"});
    fs::write(owner.join("profiles/other.json"), other.to_string()).unwrap();
    // Startup shares the machine with the user's existing JIT processes and
    // may be delayed by a linker. Still let this witness expire before the
    // turn, but do not make ordinary startup race a five-second fixture TTL.
    let witness_until = Instant::now() + Duration::from_secs(20);
    let binding = ProfileBinding::observe(&owner, "coding", witness_until).unwrap();
    let expected_profile = binding.accounting_key(&[7; 32]).unwrap();
    let mut process = OwnedOctosSession::spawn_shared_isolated(
        &guardian,
        &executable,
        &room,
        binding,
        Limits {
            write_timeout_ms: 5000,
            event_wait_ms: 30_000,
            lifetime_ms: 90_000,
        },
    )
    .unwrap();
    assert!(room.join("tmp/octos-profile/profiles/coding.json").exists());
    assert!(!room.join("tmp/octos-profile/profiles/other.json").exists());
    assert!(room.join("tmp/octos-instance").is_dir());
    if let Err(error) = process.hello().await {
        panic!(
            "isolated Octos hello failed: {error}; stderr: {}",
            process.stderr_tail(8192)
        );
    }
    assert!(process.supports_host_scoped_profile());
    let source = process.shared_process_source().unwrap();
    source
        .claim_managed_custody(&"a".repeat(64), "offline-fixture")
        .unwrap();
    let qualified = process.qualify_shared_profile(&[7; 32]).unwrap();
    assert_eq!(qualified.state(), ManagedProcessState::Qualified);
    assert_eq!(qualified.provider_digest(), Some(expected_profile.as_str()));
    process
        .install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(25))
        .unwrap();
    // The original short-lived enrollment witness can expire while an already
    // qualified process waits for work. Its live lease and unchanged profile
    // remain necessary; expiration must not spuriously kill the process.
    tokio::time::sleep_until(witness_until.into()).await;
    let workspace = room.join("work");
    process
        .open(
            "coding:local:isolated-fixture",
            "coding",
            workspace.to_str().unwrap(),
            Permissions::ReadOnly,
        )
        .await
        .unwrap();
    process.register_shared_text_tools(&[HostTool {
        name:"hagency.read_room_history".into(), description:"Read room history".into(),
        input_schema:json!({"type":"object","properties":{"cursor":{"type":["string","null"]}}}), read_only:true,
    }]).await.unwrap();
    process
        .start_turn(
            "0192f0c1-0000-7000-8000-000000000001",
            "Read the room and ask.",
        )
        .await
        .unwrap();
    let input_digest = format!("{:x}", Sha256::digest(b"Read the room and ask."));
    assert!(process.shared_call_matches(&source, &input_digest));
    assert!(!process.shared_call_matches(&source, &"b".repeat(64)));
    let mut tool = false;
    let mut question = false;
    let idle = tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            match process.next().await.unwrap() {
                Event::ToolCall { params } => {
                    let call = process.host_tool_call(&params).unwrap();
                    assert_eq!(call.name, "hagency.read_room_history");
                    assert!(
                        process
                            .host_tool_result(
                                &call.call_id,
                                Ok(json!({"message":"SCOPED_ROOM_RECEIPT"}))
                            )
                            .await
                            .unwrap()
                    );
                    tool = true;
                }
                Event::Question {
                    question_id,
                    questions,
                    ..
                } => {
                    assert_eq!(questions[0].question, "Which option?");
                    assert!(
                        process
                            .answer_question(
                                &question_id,
                                &[(questions[0].id.clone(), Answer::Option("One".into()))].into()
                            )
                            .await
                            .unwrap()
                    );
                    question = true;
                }
                Event::Idle(idle) => break idle,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(tool && question);
    assert_eq!(idle.outcome, Outcome::Completed);
    assert_eq!(idle.reply.as_deref(), Some("Offline integration complete."));
    assert!(!marker.exists());
    assert!(
        fs::read_to_string(root.join("offline-model-requests.json"))
            .unwrap()
            .contains("SCOPED_ROOM_RECEIPT")
    );
    let stopped = process.stop_shared_observation().unwrap();
    assert_eq!(stopped.state(), ManagedProcessState::Stopped);
    assert!(!process.shared_call_matches(&source, &input_digest));
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn isolated_octos_refuses_scope_changes_unrestricted_tools_and_unleased_turns() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner");
    fs::create_dir(&owner).unwrap();
    fs::create_dir(owner.join("profiles")).unwrap();
    let profile = json!({"id":"coding", "name":"Fixture", "enabled":true,
        "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":"http://127.0.0.1:1/v1","api_type":"openai"}}}}})
    .to_string();
    let path = owner.join("profiles/coding.json");
    for scenario in [
        "unclaimed",
        "other_profile",
        "other_workspace",
        "writable",
        "unrestricted_tools",
        "no_lease",
        "expired_lease",
        "no_tool_restriction",
        "changed_profile",
    ] {
        fs::write(&path, &profile).unwrap();
        let room = root.join(scenario);
        fs::create_dir(&room).unwrap();
        for name in ["home", "config", "work", "tmp"] {
            fs::create_dir(room.join(name)).unwrap();
        }
        let binding =
            ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(60))
                .unwrap();
        let mut process = OwnedOctosSession::spawn_shared_isolated(
            &guardian,
            &executable,
            &room,
            binding,
            Limits::default(),
        )
        .unwrap();
        process
            .hello()
            .await
            .unwrap_or_else(|error| panic!("{scenario}: {error}; {}", process.stderr_tail(4096)));
        let source = process.shared_process_source().unwrap();
        if scenario == "unclaimed" {
            assert!(process.qualify_shared_profile(&[7; 32]).is_err());
            assert!(
                matches!(process.cleanup(), Cleanup::Observed(report) if report.scope.whole_tree_stopped)
            );
            continue;
        }
        source
            .claim_managed_custody(&"a".repeat(64), "offline-fixture")
            .unwrap();
        process.qualify_shared_profile(&[7; 32]).unwrap();
        if scenario != "no_lease" {
            process
                .install_renewable_shared_activity(
                    1,
                    Instant::now()
                        + if scenario == "expired_lease" {
                            Duration::from_millis(100)
                        } else {
                            Duration::from_secs(30)
                        },
                )
                .unwrap();
            if scenario == "expired_lease" {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        if scenario == "changed_profile" {
            fs::write(&path, format!("{profile} ")).unwrap();
        }
        let workspace = room.join("work");
        let opened = process
            .open(
                "coding:local:isolated-fixture",
                if scenario == "other_profile" {
                    "other"
                } else {
                    "coding"
                },
                if scenario == "other_workspace" {
                    root.to_str().unwrap()
                } else {
                    workspace.to_str().unwrap()
                },
                if scenario == "writable" {
                    Permissions::WorkspaceWrite
                } else {
                    Permissions::ReadOnly
                },
            )
            .await;
        if matches!(
            scenario,
            "other_profile"
                | "other_workspace"
                | "writable"
                | "changed_profile"
                | "no_lease"
                | "expired_lease"
        ) {
            assert!(opened.is_err(), "{scenario}");
        } else {
            opened.unwrap();
            let tool = HostTool {
                name: "hagency.read_room_history".into(),
                description: "Read history".into(),
                input_schema: json!({"type":"object","properties":{}}),
                read_only: true,
            };
            if scenario == "unrestricted_tools" {
                assert!(process.register_tools(&[tool]).await.is_err());
            } else {
                if scenario != "no_tool_restriction" {
                    process.register_shared_text_tools(&[]).await.unwrap();
                }
                assert!(
                    process
                        .start_turn("0192f0c1-0000-7000-8000-000000000001", "must not run")
                        .await
                        .is_err(),
                    "{scenario}"
                );
            }
        }
        assert!(
            matches!(process.cleanup(), Cleanup::Observed(report) if report.scope.whole_tree_stopped && report.scope.leader_exited),
            "{scenario}"
        );
    }
}
