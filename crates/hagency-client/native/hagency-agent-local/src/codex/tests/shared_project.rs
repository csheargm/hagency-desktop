//! A Room with the shared-workspace opt-in works in the owner's project
//! directory. That directory is changed only through the host file tools.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Live;
impl HostToolGate for Live {
    fn authorize<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
}

#[test]
fn each_workspace_kind_has_its_own_native_sandbox() {
    let mut thread = value!({});
    NativeSandbox::RoomFolder.apply_to_thread(&mut thread);
    assert_eq!(thread, value!({"sandbox":"workspace-write"}));
    assert_eq!(NativeSandbox::RoomFolder.reported(), "workspaceWrite");
    let mut turn = value!({});
    NativeSandbox::RoomFolder.apply_to_turn(&mut turn, "/room");
    assert_eq!(turn["sandboxPolicy"]["type"], "workspaceWrite");
    assert_eq!(turn["sandboxPolicy"]["writableRoots"], value!(["/room"]));
    assert_eq!(turn["sandboxPolicy"]["networkAccess"], false);
    assert!(turn.get("environments").is_none());

    let mut thread = value!({});
    NativeSandbox::SharedProject.apply_to_thread(&mut thread);
    assert_eq!(thread["sandbox"], "read-only");
    assert_eq!(thread["environments"], value!([]));
    let hint = thread["developerInstructions"].as_str().unwrap();
    assert!(hint.contains("hagency_file_update") && hint.contains("hagency_directory_create"));
    assert_eq!(NativeSandbox::SharedProject.reported(), "readOnly");
    let mut turn = value!({});
    NativeSandbox::SharedProject.apply_to_turn(&mut turn, "/project");
    assert_eq!(
        turn,
        value!({"sandboxPolicy":{"type":"readOnly","networkAccess":false},"environments":[]})
    );
}

#[tokio::test]
async fn shared_project_context_is_read_only_and_approves_nothing_native() {
    for reported in ["readOnly", "workspaceWrite"] {
        let (temp, mut ledger, mut profile) = setup(false);
        let root = temp.path().canonicalize().unwrap();
        let owner = root.join(format!("owner_{}", "a".repeat(64)));
        std::fs::create_dir(&owner).unwrap();
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o700)).unwrap();
        profile.cwd = project.clone();
        let cwd = project.to_str().unwrap().to_owned();

        let (client, server) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(client);
        let (r, mut w) = tokio::io::split(server);
        let peer_cwd = cwd.clone();
        let peer = tokio::spawn(async move {
            let mut r = BufReader::new(r);
            let mut line = String::new();
            let mut answers = Vec::new();
            loop {
                line.clear();
                if r.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let frame: Value = serde_json::from_str(&line).unwrap();
                match frame["method"].as_str() {
                    Some("initialize") => {
                        emit(&mut w, value!({"id":frame["id"],"result":{"userAgent":"fake-codex/0.167.0"}})).await
                    }
                    Some("initialized") => {}
                    Some("config/read") => {
                        emit(&mut w, value!({"id":frame["id"],"result":{"config":{"web_search":"live","mcp_servers":{}}}})).await
                    }
                    Some("thread/start") => {
                        let p = &frame["params"];
                        assert_eq!(p["cwd"], peer_cwd.as_str());
                        assert_eq!(p["sandbox"], "read-only");
                        assert_eq!(p["environments"], value!([]));
                        assert_eq!(p["approvalPolicy"], "untrusted");
                        assert!(p["developerInstructions"].as_str().unwrap().contains("hagency_file_update"));
                        let tools = p["dynamicTools"].to_string();
                        assert!(tools.contains("hagency_file_update") && tools.contains("hagency_directory_create"));
                        emit(&mut w, value!({"id":frame["id"],"result":{"thread":{"id":"thread-a","cwd":peer_cwd},"cwd":peer_cwd,"model":"codex-test-model","approvalPolicy":"untrusted","approvalsReviewer":"user","sandbox":{"type":reported,"networkAccess":false}}})).await;
                    }
                    Some("thread/name/set") => {
                        emit(&mut w, value!({"id":frame["id"],"result":{}})).await
                    }
                    None => answers.push(frame),
                    other => panic!("unexpected request {other:?}"),
                }
            }
            answers
        });

        let mut session = Session::new(reader, writer);
        session.initialize().await.unwrap();
        assert_eq!(session.native_sandbox(), NativeSandbox::RoomFolder);
        let workspace =
            crate::room_files::Workspace::open_shared(&owner, &project, "@alice:test", &scope())
                .unwrap();
        session.enable_host_files(workspace, Arc::new(Live)).unwrap();
        assert_eq!(session.native_sandbox(), NativeSandbox::SharedProject);
        let opened = session.open_context(&mut ledger, &scope(), &profile).await;
        if reported != "readOnly" {
            // Codex must confirm the read-only sandbox before any turn starts.
            assert!(matches!(
                opened,
                Err(Error::Protocol("thread scope or policy differs"))
            ));
            drop(session);
            assert!(peer.await.unwrap().is_empty());
            continue;
        }
        opened.unwrap();

        // Whatever the saved rules say, a native command, file change or wider
        // permission is declined here and the owner is not asked.
        let (queue, mut owner_prompts) = approval_queue(4).unwrap();
        let mut waiting = crate::codex::approvals::Approvals::default();
        let requests = [
            ("item/commandExecution/requestApproval", value!({"threadId":"thread-a","turnId":"turn-a","itemId":"command-a","command":"touch changed","cwd":cwd})),
            ("item/fileChange/requestApproval", value!({"threadId":"thread-a","turnId":"turn-a","itemId":"patch-a","grantRoot":cwd})),
            ("item/permissions/requestApproval", value!({"threadId":"thread-a","turnId":"turn-a","itemId":"permissions-a","cwd":cwd,"permissions":{"fileSystem":{"write":[cwd]}}})),
        ];
        for (index, (method, params)) in requests.iter().enumerate() {
            let frame = value!({"id":format!("native-{index}"),"method":method,"params":params});
            session
                .approval(&mut ledger, &scope(), "dispatch", &frame, "thread-a", "turn-a", &cwd, &queue, &mut waiting)
                .await
                .unwrap();
        }
        assert!(owner_prompts.try_recv().is_err());
        // The session stays a shared-project session while its workspace is
        // released between turns.
        session.park();
        assert_eq!(session.native_sandbox(), NativeSandbox::SharedProject);
        drop(session);
        let answers = peer.await.unwrap();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0], value!({"id":"native-0","result":{"decision":"decline"}}));
        assert_eq!(answers[1], value!({"id":"native-1","result":{"decision":"decline"}}));
        assert_eq!(answers[2], value!({"id":"native-2","result":{"permissions":{},"scope":"turn"}}));
        assert!(!project.join("changed").exists());
    }
}
