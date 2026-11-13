//! Real Octos + local synthetic model. Never touches an owner's profile.
#![cfg(target_os = "macos")]
use hagency_runtime::{
    octos::{profile_binding::ProfileBinding, session::{Event, HostTool, Limits}},
    owned::{ManagedProcessState, OwnedOctosSession, SharedOctosPolicy},
};
use serde_json::json;
use std::{fs, path::PathBuf, process::Stdio, time::{Duration, Instant}};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn interactive_shell_supports_pty_and_pipe_input_and_stops_with_the_activity() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner"); let room = root.join("room");
    fs::create_dir_all(owner.join("profiles")).unwrap(); fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
    fs::write(owner.join("profiles/coding.json"), json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":"http://127.0.0.1:9/v1","api_type":"openai"}}}}}).to_string()).unwrap();
    let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
    let mut process = OwnedOctosSession::spawn_shared_isolated_with_policy(&guardian, &executable, &room, binding,
        Limits::default(), SharedOctosPolicy::Standard { web: false }).unwrap();
    process.hello().await.unwrap();
    process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "interactive-fixture").unwrap();
    process.qualify_shared_profile(&[7; 32]).unwrap();
    process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
    let shell = process.shared_shell().unwrap();
    for (tty, marker) in [(true, "TTY_READY"), (false, "PIPE_READY")] {
        let command = format!("if test -t 0; then printf TTY_READY; else printf PIPE_READY; fi; read line; printf ':ANSWER:%s' \"$line\"; if cat ../tmp/octos-profile/profiles/coding.json >/dev/null 2>&1; then printf SECRET_EXPOSED; else printf PROFILE_DENIED; fi");
        let mut result = shell.exec(&command, None, tty, Duration::from_secs(4), Duration::from_secs(20)).await.unwrap();
        let mut output = result.output.clone();
        let id = result.session_id.take().expect("input remains open");
        while !output.contains(marker) {
            let next = shell.write_stdin(&id, "", Duration::from_secs(4)).await.unwrap();
            output.push_str(&next.output);
            assert!(next.session_id.is_some(), "command ended before its prompt: {output}");
        }
        let mut next = shell.write_stdin(&id, "fixture-answer\n", Duration::from_secs(4)).await.unwrap();
        output.push_str(&next.output);
        while next.session_id.is_some() {
            next = shell.write_stdin(&id, "", Duration::from_secs(4)).await.unwrap();
            output.push_str(&next.output);
        }
        assert_eq!(next.exit_code, Some(0), "{output}");
        assert!(output.contains(":ANSWER:fixture-answer") && output.contains("PROFILE_DENIED"), "{output}");
        assert!(!output.contains("SECRET_EXPOSED"), "{output}");
    }
    // Exercise an installed system C toolchain in the same confined shell,
    // including SDK headers and linking. No owner files or network are used.
    if PathBuf::from("/Library/Developer/CommandLineTools/usr/bin/clang").is_file() {
        fs::write(room.join("work/hello.c"), "#include <stdio.h>\nint main(void) { puts(\"SCOPED_CLANG_OK\"); return 0; }\n").unwrap();
        let result = shell.run("clang hello.c -o hello && ./hello", Duration::from_secs(12)).await.unwrap();
        assert_eq!(result.exit_code, Some(0), "stdout: {}; stderr: {}", result.stdout, result.stderr);
        assert!(result.stdout.contains("SCOPED_CLANG_OK"), "{}", result.stdout);
        for tty in [false, true] {
            let command = format!("clang hello.c -o hello_{tty} && ./hello_{tty}");
            let mut result = shell.exec(&command, None, tty, Duration::from_secs(5), Duration::from_secs(12)).await.unwrap();
            let mut output = result.output.clone();
            while let Some(id) = result.session_id.take() {
                result = shell.write_stdin(&id, "", Duration::from_secs(5)).await.unwrap();
                output.push_str(&result.output);
            }
            assert_eq!(result.exit_code, Some(0), "{output}");
            assert!(output.contains("SCOPED_CLANG_OK"), "{output}");
        }
        fs::write(room.join("work/hello.cpp"), "#include <iostream>\nint main() { std::cout << \"SCOPED_CPP_OK\"; }\n").unwrap();
        let result = shell.run("clang++ hello.cpp -o hello_cpp && ./hello_cpp", Duration::from_secs(12)).await.unwrap();
        assert_eq!(result.exit_code, Some(0), "stdout: {}; stderr: {}", result.stdout, result.stderr);
        assert!(result.stdout.contains("SCOPED_CPP_OK"), "{}", result.stdout);
    }
    if PathBuf::from("/Library/Developer/CommandLineTools/usr/bin/python3").is_file() {
        let result = shell.run("python3 -c 'print(\"SCOPED_PYTHON_OK\")'", Duration::from_secs(12)).await.unwrap();
        assert_eq!(result.exit_code, Some(0), "stdout: {}; stderr: {}", result.stdout, result.stderr);
        assert!(result.stdout.contains("SCOPED_PYTHON_OK"), "{}", result.stdout);
    }
    assert!(shell.exec("true", Some("../tmp"), false, Duration::from_secs(1), Duration::from_secs(5)).await.is_err());
    assert!(shell.write_stdin("shell-foreign", "input", Duration::ZERO).await.is_err());
    let patch = json!({"patch":"*** Begin Patch\n*** Add File: patched.txt\n+patch in room\n*** End Patch\n"});
    let patched = shell.apply_patch(&patch).await.unwrap();
    assert_eq!(patched["success"], true, "{patched}");
    assert_eq!(fs::read_to_string(room.join("work/patched.txt")).unwrap(), "patch in room");
    std::os::unix::fs::symlink(room.join("tmp/octos-profile"), room.join("work/patch-escape")).unwrap();
    let outside = fs::read(room.join("tmp/octos-profile/profiles/coding.json")).unwrap();
    let escape = json!({"patch":"*** Begin Patch\n*** Delete File: patch-escape/profiles/coding.json\n*** End Patch\n"});
    let refused = shell.apply_patch(&escape).await;
    assert!(refused.as_ref().map_or(true, |value| value["success"] == false), "{refused:?}");
    assert_eq!(fs::read(room.join("tmp/octos-profile/profiles/coding.json")).unwrap(), outside);
    assert!(shell.apply_patch(&json!({"patch":"x".repeat(65536)})).await.is_err());
    let first = shell.exec("printf ONE; read line", None, true, Duration::from_secs(2), Duration::from_secs(20)).await.unwrap();
    let second = shell.exec("printf TWO; read line", None, false, Duration::from_secs(2), Duration::from_secs(20)).await.unwrap();
    assert!(first.session_id.is_some() && second.session_id.is_some() && first.session_id != second.session_id);
    shell.shutdown().await.unwrap();
    let pending = shell.exec("printf WAITING; sleep 3; printf leaked > revoked.txt", None, true, Duration::from_secs(1), Duration::from_secs(20)).await.unwrap();
    assert!(pending.session_id.is_some());
    fs::write(owner.join("profiles/coding.json"), b"{} ").unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    shell.shutdown().await.unwrap();
    assert!(!room.join("work/revoked.txt").exists());
    assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn cancelled_patch_worker_is_stopped_and_its_slot_is_reclaimed() {
    use std::os::unix::fs::PermissionsExt;
    let real = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner"); let room = root.join("room");
    fs::create_dir_all(owner.join("profiles")).unwrap(); fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
    fs::write(owner.join("profiles/coding.json"), json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":"http://127.0.0.1:9/v1","api_type":"openai"}}}}}).to_string()).unwrap();
    fs::copy(real, room.join("work/octos-real")).unwrap();
    let wrapper = room.join("work/octos-fixture");
    fs::write(&wrapper, "#!/bin/sh\nif [ \"$1\" = '__host-patch' ]; then printf ready > patch-started; /bin/sleep 3; printf leaked > patch-after-cancel; exit 0; fi\nexec ./octos-real \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
    let mut process = OwnedOctosSession::spawn_shared_isolated_with_policy(&guardian, &wrapper, &room, binding,
        Limits::default(), SharedOctosPolicy::Standard { web: false }).unwrap();
    process.hello().await.unwrap();
    process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "cancel-patch-fixture").unwrap();
    process.qualify_shared_profile(&[7; 32]).unwrap();
    process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
    let shell = process.shared_shell().unwrap();
    for _ in 0..4 {
        let _ = fs::remove_file(room.join("work/patch-started"));
        let worker = shell.clone();
        let task = tokio::spawn(async move { worker.apply_patch(&json!({"patch":"unused"})).await });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !room.join("work/patch-started").exists() { tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await.expect("synthetic patch worker started");
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    let result = shell.exec("printf next-command", None, false, Duration::from_secs(2), Duration::from_secs(5)).await.unwrap();
    assert!(result.output.contains("next-command"));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!room.join("work/patch-after-cancel").exists());
    shell.shutdown().await.unwrap();
    assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn each_configured_model_reaches_its_selected_endpoint_without_modifying_the_profile() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner");
    fs::create_dir_all(owner.join("profiles")).unwrap();
    fs::write(root.join("offline-model-script.json"), "[null,null]").unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    let mut server = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(server.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let endpoint = format!("http://127.0.0.1:{}/v1", port.trim().parse::<u16>().unwrap());
    let route = json!({"base_url":endpoint,"api_type":"openai"});
    let original = json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"qwen-local","route":route},
            "fallbacks":[{"family_id":"local","model_id":"deepseek-local","route":route}]}}}).to_string();
    let path = owner.join("profiles/coding.json");
    fs::write(&path, &original).unwrap();
    for (index, selected) in ["qwen-local", "deepseek-local"].into_iter().enumerate() {
        let room = root.join(format!("room-{index}"));
        fs::create_dir(&room).unwrap();
        for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
        let binding = ProfileBinding::observe_model(&owner, "coding", selected, Instant::now() + Duration::from_secs(30)).unwrap();
        let policy = SharedOctosPolicy::Text;
        let mut process = OwnedOctosSession::spawn_shared_isolated_with_policy(
            &guardian, &executable, &room, binding, Limits::default(), policy).unwrap();
        let scoped: serde_json::Value = serde_json::from_slice(&fs::read(room.join("tmp/octos-profile/profiles/coding.json")).unwrap()).unwrap();
        assert_eq!(scoped["config"]["llm"]["primary"]["model_id"], selected);
        assert_eq!(scoped["config"]["llm"]["fallbacks"], json!([]));
        process.hello().await.unwrap_or_else(|e| panic!("hello: {e}; {}", process.stderr_tail(4096)));
        process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "model-fixture").unwrap();
        assert_eq!(process.qualify_shared_profile(&[7; 32]).unwrap().state(), ManagedProcessState::Qualified);
        process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
        process.open("coding:local:models", "coding", room.join("work").to_str().unwrap(), policy.permissions()).await.unwrap();
        process.register_shared_tools(&[]).await.unwrap();
        process.start_turn("0192f0c1-0000-7000-8000-000000000002", "Answer the fixture.").await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop { if let Event::Idle(_) = process.next().await.unwrap_or_else(|e| panic!("next: {e}; {}", process.stderr_tail(4096))) { break; } }
        }).await.unwrap();
        process.stop_shared_observation().unwrap();
        let calls: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(root.join("offline-model-requests.json")).unwrap()).unwrap();
        assert_eq!(calls.len(), index + 1);
        assert_eq!(calls[index]["model"], selected);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
    server.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn isolated_coding_edits_and_runs_only_the_owner_approved_shell() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    for approve in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owner = root.join("owner");
        let room = root.join("room");
        fs::create_dir_all(owner.join("profiles")).unwrap();
        fs::create_dir(&room).unwrap();
        for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
        fs::create_dir(room.join("work/nested")).unwrap();
        fs::create_dir(room.join("work/.git")).unwrap();
        fs::write(room.join("work/.gitignore"), "*.ignored\n").unwrap();
        fs::write(room.join("work/nested/hidden.ignored"), "NEEDLE_IGNORE_SENTINEL\n").unwrap();
        fs::write(room.join("work/nested/hello.c"), "#include <stdio.h>\nint main(void) { puts(\"OCTOS_MODEL_COMPILE_OK\"); return 0; }\n").unwrap();
        fs::write(room.join("work/nested/screenshot.png"), [137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,1,0,0,0,1,8,4,0,0,0,181,28,12,2,0,0,0,11,73,68,65,84,120,218,99,252,255,31,0,3,3,2,0,239,167,155,108,0,0,0,0,73,69,78,68,174,66,96,130]).unwrap();
        let model_file = root.join("model.py");
        fs::write(&model_file, include_str!("../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
        let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
        let mut port = String::new();
        BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
        let port: u16 = port.trim().parse().unwrap();
        let command = format!("if cat ../tmp/octos-profile/profiles/coding.json >/dev/null 2>&1; then printf SECRET_EXPOSED; else printf PROFILE_DENIED; fi; if /usr/bin/nc -w 1 127.0.0.1 {port} >/dev/null 2>&1; then printf NETWORK_EXPOSED; else printf NETWORK_DENIED; fi; if [ -x /Library/Developer/CommandLineTools/usr/bin/clang ]; then clang nested/hello.c -o nested/compiled && ./nested/compiled; else printf NO_APPLE_CLT; fi");
        let script = json!([
            {"name":"update_plan","arguments":{"plan":[{"step":"Inspect the workspace","status":"in_progress"}]}},
            {"name":"write_file","arguments":{"path":"nested/result.txt","content":"workspace"}},
            {"name":"edit_file","arguments":{"path":"nested/result.txt","old_string":"workspace","new_string":"workspace-updated"}},
            {"name":"read_file","arguments":{"path":"nested/result.txt"}},
            {"name":"read_file","arguments":{"path":"escape/profiles/coding.json"}},
            {"name":"run_tool","arguments":{"name":"list_dir","arguments":{"path":"nested"}}},
            {"name":"run_tool","arguments":{"name":"glob","arguments":{"pattern":"**/*.txt"}}},
            {"name":"run_tool","arguments":{"name":"grep","arguments":{"pattern":"workspace-updated"}}},
            {"name":"run_tool","arguments":{"name":"grep","arguments":{"pattern":"NEEDLE_IGNORE_SENTINEL"}}},
            {"name":"run_tool","arguments":{"name":"glob","arguments":{"pattern":"**/*.ignored"}}},
            {"name":"run_tool","arguments":{"name":"grep","arguments":{"pattern":"synthetic-owner-secret","path":"escape"}}},
            {"name":"run_tool","arguments":{"name":"view_image","arguments":{"path":"nested/screenshot.png"}}},
            {"name":"run_tool","arguments":{"name":"view_image","arguments":{"path":"escape/profiles/coding.json"}}},
            {"name":"run_tool","arguments":{"name":"browser","arguments":{}}},
            {"name":"run_tool","arguments":{"name":"hagency_shell","arguments":{"command":command}}},
            {"name":"run_tool","arguments":{"name":"hagency_shell","arguments":{"command":"printf approved > approved.txt"}}}
        ]);
        fs::write(root.join("offline-model-script.json"), script.to_string()).unwrap();
        let profile = json!({"id":"coding","name":"Fixture","enabled":true,
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
            "config":{"env_vars":{"SYNTHETIC_API_KEY":"synthetic-owner-secret"},
                "llm":{"primary":{"family_id":"local","model_id":"test-model",
                    "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}}}});
        fs::write(owner.join("profiles/coding.json"), profile.to_string()).unwrap();
        let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
        let policy = SharedOctosPolicy::Standard { web: false };
        let mut process = OwnedOctosSession::spawn_shared_isolated_with_policy(
            &guardian, &executable, &room, binding, Limits::default(), policy).unwrap();
        std::os::unix::fs::symlink(room.join("tmp/octos-profile"), room.join("work/escape")).unwrap();
        process.hello().await.unwrap_or_else(|e| panic!("hello: {e}; {}", process.stderr_tail(4096)));
        process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "coding-fixture").unwrap();
        assert_eq!(process.qualify_shared_profile(&[7; 32]).unwrap().state(), ManagedProcessState::Qualified);
        process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
        process.open("coding:local:isolated-coding", "coding", room.join("work").to_str().unwrap(), policy.permissions()).await.unwrap();
        let shell = process.shared_shell().unwrap();
        let live_text = hagency_runtime::live_text::LiveText::default();
        process.enable_live_text(live_text.clone()).unwrap();
        process.register_shared_tools(&[HostTool {
            name:"hagency.shell".into(), description:"Run a command in the room workspace".into(),
            input_schema:json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}), read_only:false,
        }]).await.unwrap();
        process.start_turn("0192f0c1-0000-7000-8000-000000000002", "Run the fixture.").await.unwrap();
        let mut shell_calls = 0;
        let mut saw_plan = false;
        tokio::time::timeout(Duration::from_secs(50), async {
            loop {
                match process.next().await.unwrap_or_else(|e| panic!("next: {e}; {}", process.stderr_tail(4096))) {
                    Event::ToolCall { params } => {
                        let call = process.host_tool_call(&params).unwrap();
                        assert_eq!(call.name, "hagency.shell");
                        shell_calls += 1;
                        let command = call.args["command"].as_str().unwrap();
                        let result = if shell_calls == 1 || approve {
                            let output = shell.run(command, Duration::from_secs(10)).await.unwrap();
                            Ok(serde_json::to_value(output).unwrap())
                        } else { Err("shell_owner_denied".into()) };
                        assert!(process.host_tool_result(&call.call_id, result).await.unwrap());
                    }
                    Event::Idle(_) => break,
                    _ => {}
                }
                saw_plan |= live_text.since(0).is_some_and(|(_, text)| text.contains("◉ Inspect the workspace"));
            }
        }).await.unwrap();
        assert_eq!(shell_calls, 2);
        assert!(saw_plan, "Octos plan update was not presented");
        let requests = fs::read_to_string(root.join("offline-model-requests.json")).unwrap();
        assert_eq!(fs::read_to_string(room.join("work/nested/result.txt")).unwrap_or_else(|e| panic!("workspace result: {e}; stderr: {}", process.stderr_tail(4096))), "workspace-updated");
        assert_eq!(room.join("work/approved.txt").exists(), approve);
        let recorded: Vec<serde_json::Value> = serde_json::from_str(&requests).unwrap();
        let results = recorded.last().unwrap()["messages"].as_array().unwrap().iter()
            .filter(|message| message["role"] == "tool").map(|message| message["content"].to_string()).collect::<Vec<_>>().join("\n");
        assert!(results.contains("PROFILE_DENIED") && results.contains("NETWORK_DENIED"), "{results}");
        if PathBuf::from("/Library/Developer/CommandLineTools/usr/bin/clang").is_file() {
            assert!(results.contains("OCTOS_MODEL_COMPILE_OK"), "{results}");
        }
        assert!(results.contains("unknown or unavailable") && results.contains("workspace-updated"), "{results}");
        assert!(results.contains("nested/result.txt") && results.contains("[file] result.txt"), "{results}");
        assert!(results.contains("nested/hidden.ignored") && !results.contains("NEEDLE_IGNORE_SENTINEL"), "{results}");
        assert!(requests.contains("data:image/png;base64,"), "the model did not receive the workspace image");
        assert!(!results.contains("synthetic-owner-secret") && !results.contains("SECRET_EXPOSED"), "{results}");
        assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
        model.kill().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn selected_login_wins_and_rotation_interrupts_the_confined_shell() {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner");
    let room = root.join("room");
    let auth = root.join("synthetic-auth");
    fs::create_dir_all(owner.join("profiles")).unwrap();
    fs::create_dir(&auth).unwrap();
    fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
    let credential = |value: &str| json!({"access_token":value,"provider":"openai",
        "refresh_token":"synthetic-never-copied","auth_method":"oauth"});
    let write_auth = |value: &str| fs::write(auth.join("auth.json"), json!({"credentials":{
        "openai":credential(value),"other":credential("other-synthetic-secret")}}).to_string()).unwrap();
    write_auth("synthetic-login");
    fs::write(root.join("offline-model-script.json"), "[null]").unwrap();
    fs::write(root.join("offline-model-expected-authorization.txt"), "synthetic-login").unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let profile = json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"env_vars":{"OPENAI_API_KEY":"synthetic-profile-key"},
            "llm":{"primary":{"family_id":"openai","model_id":"test-model",
                "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}}}});
    fs::write(owner.join("profiles/coding.json"), profile.to_string()).unwrap();
    let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
    let policy = SharedOctosPolicy::Standard { web: false };
    let mut process = OwnedOctosSession::spawn_shared_isolated_with_auth(
        &guardian, &executable, &room, binding, Limits::default(), policy, Some(&auth)).unwrap();
    let copied: serde_json::Value = serde_json::from_slice(&fs::read(room.join("tmp/octos-auth/octos/auth.json")).unwrap()).unwrap();
    assert_eq!(copied["credentials"]["openai"]["access_token"], "synthetic-login");
    assert!(copied["credentials"].get("other").is_none());
    assert!(copied["credentials"]["openai"].get("refresh_token").is_none());
    process.hello().await.unwrap_or_else(|e| panic!("hello: {e}; {}", process.stderr_tail(4096)));
    process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "auth-fixture").unwrap();
    assert_eq!(process.qualify_shared_profile(&[7; 32]).unwrap().state(), ManagedProcessState::Qualified);
    process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
    process.open("coding:local:isolated-auth", "coding", room.join("work").to_str().unwrap(), policy.permissions()).await.unwrap();
    process.register_shared_tools(&[]).await.unwrap();
    process.start_turn("0192f0c1-0000-7000-8000-000000000002", "Answer the fixture.").await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match process.next().await.unwrap_or_else(|e| panic!("next: {e}; {}", process.stderr_tail(4096))) {
                Event::Idle(_) => break,
                _ => {}
            }
        }
    }).await.unwrap();
    assert_eq!(fs::read_to_string(root.join("offline-model-authorization-matches.txt")).unwrap(), "True");
    let shell = process.shared_shell().unwrap();
    let task = tokio::spawn(async move {
        shell.run("sleep 6; printf late > should-not-exist", Duration::from_secs(15)).await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    write_auth("rotated-synthetic-login");
    assert!(tokio::time::timeout(Duration::from_secs(8), task).await.unwrap().unwrap().is_err());
    assert!(!room.join("work/should-not-exist").exists());
    assert!(process.start_turn("0192f0c1-0000-7000-8000-000000000003", "Must not start").await.is_err());
    assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn enabled_web_fetch_cannot_reach_a_local_service() {
    use std::net::TcpListener;
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner"); let room = root.join("room");
    fs::create_dir_all(owner.join("profiles")).unwrap(); fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    service.set_nonblocking(true).unwrap();
    let url = format!("http://127.0.0.1:{}/private", service.local_addr().unwrap().port());
    fs::write(root.join("offline-model-script.json"), json!([
        {"name":"web_fetch","arguments":{"url":url}}
    ]).to_string()).unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    fs::write(owner.join("profiles/coding.json"), json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}}}}).to_string()).unwrap();
    let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
    let policy = SharedOctosPolicy::LiveWeb;
    let mut process = OwnedOctosSession::spawn_shared_isolated_with_policy(
        &guardian, &executable, &room, binding, Limits::default(), policy).unwrap();
    process.hello().await.unwrap_or_else(|e| panic!("hello: {e}; {}", process.stderr_tail(4096)));
    process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "web-fixture").unwrap();
    assert_eq!(process.qualify_shared_profile(&[7; 32]).unwrap().state(), ManagedProcessState::Qualified);
    process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
    process.open("coding:local:isolated-web", "coding", room.join("work").to_str().unwrap(), policy.permissions()).await.unwrap();
    process.register_shared_tools(&[]).await.unwrap();
    process.start_turn("0192f0c1-0000-7000-8000-000000000002", "Fetch the fixture URL.").await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match process.next().await.unwrap_or_else(|e| panic!("next: {e}; {}", process.stderr_tail(4096))) {
                Event::Idle(_) => break,
                _ => {}
            }
        }
    }).await.unwrap();
    let requests: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(root.join("offline-model-requests.json")).unwrap()).unwrap();
    let results = requests.last().unwrap()["messages"].as_array().unwrap().iter()
        .filter(|message| message["role"] == "tool").map(|message| message["content"].to_string()).collect::<Vec<_>>().join("\n");
    assert!(!results.is_empty(), "the native web tool ran");
    assert!(service.accept().is_err(), "the local listener received no connection");
    assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to the scoped Octos build"]
async fn selected_custom_environment_key_reaches_only_the_scoped_profile() {
    const KEY: &str = "HAGENCY_TEST_OCTOS_SYNTHETIC_PROVIDER_KEY";
    const CHILD: &str = "HAGENCY_TEST_OCTOS_SYNTHETIC_ENV_CHILD";
    // Start a clean test process with only a synthetic credential, never
    // mutate this multithreaded process's environment or read a real key.
    if std::env::var_os(CHILD).is_none() {
        let executable = std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "selected_custom_environment_key_reaches_only_the_scoped_profile", "--test-threads=1", "--nocapture"])
            .env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HAGENCY_TEST_OCTOS_EXECUTABLE", executable).env(CHILD, "1")
            .env(KEY, "synthetic-environment-key").output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(env!("CARGO_BIN_EXE_hagency-runtime-probe"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let owner = root.join("owner"); let room = root.join("room"); let auth = root.join("empty-auth");
    fs::create_dir_all(owner.join("profiles")).unwrap(); fs::create_dir(&room).unwrap(); fs::create_dir(&auth).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(room.join(name)).unwrap(); }
    fs::write(root.join("offline-model-script.json"), "[null]").unwrap();
    fs::write(root.join("offline-model-expected-authorization.txt"), "synthetic-environment-key").unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    fs::write(owner.join("profiles/coding.json"), json!({"id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"openai","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai","api_key_env":KEY}}}}}).to_string()).unwrap();
    let binding = ProfileBinding::observe(&owner, "coding", Instant::now() + Duration::from_secs(30)).unwrap();
    let policy = SharedOctosPolicy::Text;
    let mut process = OwnedOctosSession::spawn_shared_isolated_with_auth(
        &guardian, &executable, &room, binding, Limits::default(), policy, Some(&auth)).unwrap();
    let scoped: serde_json::Value = serde_json::from_slice(&fs::read(room.join("tmp/octos-profile/profiles/coding.json")).unwrap()).unwrap();
    assert_eq!(scoped["config"]["env_vars"][KEY], "synthetic-environment-key");
    assert!(!fs::read_to_string(owner.join("profiles/coding.json")).unwrap().contains("synthetic-environment-key"));
    process.hello().await.unwrap_or_else(|e| panic!("hello: {e}; {}", process.stderr_tail(4096)));
    process.shared_process_source().unwrap().claim_managed_custody(&"a".repeat(64), "env-fixture").unwrap();
    assert_eq!(process.qualify_shared_profile(&[7; 32]).unwrap().state(), ManagedProcessState::Qualified);
    process.install_renewable_shared_activity(1, Instant::now() + Duration::from_secs(30)).unwrap();
    process.open("coding:local:isolated-env", "coding", room.join("work").to_str().unwrap(), policy.permissions()).await.unwrap();
    process.register_shared_tools(&[]).await.unwrap();
    process.start_turn("0192f0c1-0000-7000-8000-000000000002", "Answer the fixture.").await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match process.next().await.unwrap_or_else(|e| panic!("next: {e}; {}", process.stderr_tail(4096))) {
                Event::Idle(_) => break,
                _ => {}
            }
        }
    }).await.unwrap();
    assert_eq!(fs::read_to_string(root.join("offline-model-authorization-matches.txt")).unwrap(), "True");
    assert_eq!(process.stop_shared_observation().unwrap().state(), ManagedProcessState::Stopped);
    model.kill().await.unwrap();
}
