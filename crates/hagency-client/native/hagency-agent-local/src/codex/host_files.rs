//! Dynamic host tools, deliberately separate from native Codex tools.
use super::*;
use crate::room_files::{FileError, Operation, PreparedCall, Workspace};
use std::{future::Future, pin::Pin, sync::Arc};

pub const FILE_CAPABILITY_VERSION: &str = "hagency-room-files-v1";
/// Host must capture the exact owner/device/lease/scope and compare the fresh
/// server running dispatch before returning true. Called again after approval.
pub trait HostToolGate: Send + Sync {
    fn authorize<'a>(
        &'a self,
        dispatch: &'a str,
        execution: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}
pub(super) struct HostFiles {
    workspace: Option<Workspace>,
    directory: PathBuf,
    gate: Option<Arc<dyn HostToolGate>>,
    calls: BTreeMap<String, (String, PreparedCall, Value)>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    path: String,
    content: String,
}

pub(super) fn specifications() -> Value {
    value!([
        {"type":"function","name":"hagency_file_list","description":"List this Room binding's private workspace. Use an empty path for the root.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}},
        {"type":"function","name":"hagency_file_read","description":"Read a UTF-8 file in this Room binding's private workspace.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}},
        {"type":"function","name":"hagency_file_create","description":"Create a new UTF-8 file in this Room binding's private workspace. Existing files cannot be replaced.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}
    ])
}
fn failure(code: &str) -> Value {
    value!({"success":false,"contentItems":[{"type":"inputText","text":code}]})
}
fn operation(params: &Value, owner: &str, scope: &Scope, dispatch: &str) -> Result<Operation> {
    if params.get("namespace").is_some_and(|v| !v.is_null()) {
        return Err(Error::Protocol("unexpected host tool namespace"));
    }
    let args = params["arguments"].clone();
    match params["tool"].as_str() {
        Some("hagency_file_list") => {
            let i: PathInput = serde_json::from_value(args)?;
            Ok(Operation::List { path: i.path })
        }
        Some("hagency_file_read") => {
            let i: PathInput = serde_json::from_value(args)?;
            Ok(Operation::Read { path: i.path })
        }
        Some("hagency_file_create") => {
            let i: CreateInput = serde_json::from_value(args)?;
            use sha2::{Digest, Sha256};
            // The model never selects a write idempotency token or capability root.
            let nonce = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(owner, scope, dispatch, params))?)
            );
            Ok(Operation::Create {
                path: i.path,
                content: i.content,
                nonce,
            })
        }
        _ => Err(Error::Protocol("unknown host tool")),
    }
}
impl HostFiles {
    pub(super) fn workspace_directory(&self) -> &std::path::Path {
        &self.directory
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Session<R, W> {
    /// Host-only capability attachment before context creation. The working
    /// directory must equal this generated binding root when opening context.
    pub fn enable_host_files(
        &mut self,
        workspace: Workspace,
        gate: Arc<dyn HostToolGate>,
    ) -> Result<()> {
        if self.thread.is_some() || self.host_files.is_some() {
            return Err(Error::Protocol("host capability already configured"));
        }
        self.host_files = Some(HostFiles {
            directory: workspace.canonical_directory().to_path_buf(),
            workspace: Some(workspace),
            gate: Some(gate),
            calls: BTreeMap::new(),
        });
        Ok(())
    }
    /// Retain tool definitions, but never a previous dispatch's permission.
    pub fn suspend_host_files(&mut self) {
        if let Some(files) = self.host_files.as_mut() {
            files.gate = None;
            files.calls.clear();
            // Release the per-room exclusive file lock while the native
            // process is idle, including before a different requester's turn.
            files.workspace = None;
        }
    }
    pub fn rebind_host_files(
        &mut self,
        workspace: Workspace,
        gate: Arc<dyn HostToolGate>,
    ) -> Result<()> {
        if !self.can_continue() {
            return Err(Error::Protocol(
                "host capability requires completed prior turn",
            ));
        }
        let files = self
            .host_files
            .as_mut()
            .ok_or(Error::Protocol("host capability missing"))?;
        if files.gate.is_some()
            || files.workspace.is_some()
            || files.workspace_directory() != workspace.canonical_directory()
        {
            return Err(Error::Protocol(
                "host capability still active or workspace changed",
            ));
        }
        files.workspace = Some(workspace);
        files.calls.clear();
        files.gate = Some(gate);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn host_file_call(
        &mut self,
        ledger: &mut Ledger,
        owner: &str,
        scope: &Scope,
        dispatch: &str,
        execution: &str,
        frame: &Value,
        thread: &str,
        turn: &str,
        queue: &ApprovalQueue,
    ) -> Result<()> {
        let id = frame["id"].clone();
        if !(id.is_string() || id.is_u64()) || !self.server_ids.insert(json(&id)?) {
            return Err(Error::Protocol("invalid or reused host request ID"));
        }
        let p = &frame["params"];
        exact_scope(p, thread, turn)?;
        let call = p["callId"]
            .as_str()
            .ok_or(Error::Protocol("host call identity missing"))?;
        key(call)?;
        let Some(files) = self.host_files.as_mut() else {
            return self
                .write(value!({"id":id,"result":failure("host_files_disabled")}))
                .await;
        };
        let digest = json(p)?;
        let Some(gate) = files.gate.clone() else {
            return self
                .write(value!({"id":id,"result":failure("host_files_inactive")}))
                .await;
        };
        if !gate.authorize(dispatch, execution).await {
            return self
                .write(value!({"id":id,"result":failure("server_scope_unavailable")}))
                .await;
        }
        if let Some((prior, prepared, result)) = files.calls.get(call) {
            if prior != &digest {
                return Err(Error::Protocol("host call arguments changed"));
            }
            let result = if ledger.tool_disposition(prepared.proposal(), now()).is_ok() {
                result.clone()
            } else {
                failure("local_policy_changed")
            };
            return self.write(value!({"id":id,"result":result})).await;
        }
        if files.calls.len() >= 128 {
            return Err(Error::Protocol("host call capacity"));
        }
        let operation = match operation(p, owner, scope, dispatch) {
            Ok(op) => op,
            Err(_) => {
                return self
                    .write(value!({"id":id,"result":failure("invalid_host_file_request")}))
                    .await;
            }
        };
        let Some(workspace) = files.workspace.as_mut() else {
            return self
                .write(value!({"id":id,"result":failure("host_files_inactive")}))
                .await;
        };
        let prepared = match workspace.prepare(
            ledger,
            scope,
            dispatch,
            thread,
            turn,
            call,
            operation,
            now(),
        ) {
            Ok(prepared) => prepared,
            Err(_) => {
                return self
                    .write(value!({"id":id,"result":failure("invalid_host_file_request")}))
                    .await;
            }
        };
        let mut outcome = workspace.execute(ledger, &prepared, now());
        if matches!(outcome, Err(FileError::NeedsOwner)) {
            let (send, recv) = oneshot::channel();
            if queue
                .sender
                .try_send(PendingApproval {
                    proposal: prepared.proposal().clone(),
                    decision: send,
                })
                .is_ok()
                && matches!(
                    tokio::time::timeout(Duration::from_secs(300), recv).await,
                    Ok(Ok(true))
                )
                && gate.authorize(dispatch, execution).await
                && ledger
                    .approve_tool(owner, prepared.proposal(), now())
                    .is_ok()
            {
                // execute consumes the exact permit; never consume it here.
                outcome = workspace.execute(ledger, &prepared, now());
            }
        }
        let result = match outcome {
            Ok(output) => {
                value!({"success":true,"contentItems":[{"type":"inputText","text":serde_json::to_string(&output)?}]})
            }
            Err(FileError::Unknown) => return Err(Error::Unknown),
            Err(FileError::NeedsOwner | FileError::Denied) => failure("host_file_denied"),
            Err(FileError::Conflict) => failure("file_exists_or_changed"),
            Err(FileError::Capacity) => failure("file_capacity_exceeded"),
            Err(_) => failure("host_file_unavailable"),
        };
        files
            .calls
            .insert(call.into(), (digest, prepared, result.clone()));
        self.write(value!({"id":id,"result":result})).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Budget, Layer, Limit, Period, Policy, RequestPolicy};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Gate(Arc<AtomicBool>);
    impl HostToolGate for Gate {
        fn authorize<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { self.0.load(Ordering::Acquire) })
        }
    }
    type Peer = BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;
    type TestSession = Session<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    >;
    fn fixture(
        policy: ToolPolicy,
    ) -> (
        tempfile::TempDir,
        Ledger,
        Scope,
        TestSession,
        Peer,
        PathBuf,
        Arc<AtomicBool>,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner = temp
            .path()
            .canonicalize()
            .unwrap()
            .join(format!("owner_{}", "a".repeat(64)));
        std::fs::create_dir(&owner).unwrap();
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut ledger = Ledger::open(owner.join("ledger"), "@alice:test").unwrap();
        let scope = Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:test".into(),
            requester: "@bob:test".into(),
            thread: "matrix-thread".into(),
        };
        ledger.register_binding("@alice:test", &scope).unwrap();
        let workspace = Workspace::open(&owner, "@alice:test", &scope).unwrap();
        let root = workspace.canonical_directory().to_path_buf();
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
            let p = Policy {
                budget: Budget {
                    limit: Limit::Tokens(100),
                    period: Period::Lifetime,
                },
                requests: RequestPolicy::Allow,
                high_risk: policy.clone(),
            };
            ledger
                .set_policy("@alice:test", &scope, layer, 0, &p)
                .unwrap();
        }
        ledger
            .reserve(&scope, "execution", "dispatch", 20, now())
            .unwrap();
        let (client, server) = tokio::io::duplex(65536);
        let (r, w) = tokio::io::split(client);
        let (peer, _) = tokio::io::split(server);
        let mut session = Session::new(r, w);
        let live = Arc::new(AtomicBool::new(true));
        session
            .enable_host_files(workspace, Arc::new(Gate(live.clone())))
            .unwrap();
        (
            temp,
            ledger,
            scope,
            session,
            BufReader::new(peer),
            root,
            live,
        )
    }
    fn request(id: u64, path: &str) -> Value {
        value!({"id":id,"method":"item/tool/call","params":{"threadId":"codex-thread","turnId":"turn","callId":"call","tool":"hagency_file_create","arguments":{"path":path,"content":"room scoped"}}})
    }
    async fn response(peer: &mut Peer) -> Value {
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    #[tokio::test]
    async fn exact_owner_confirmation_executes_host_file_and_repeated_call_does_not_execute_again()
    {
        let (_temp, mut ledger, scope, mut session, mut peer, root, _) =
            fixture(ToolPolicy::AskOwner);
        let (queue, mut pending) = approval_queue(1).unwrap();
        let approval = tokio::spawn(async move {
            let proposed = pending.recv().await.unwrap();
            assert_eq!(proposed.proposal.arguments["codexThreadId"], "codex-thread");
            assert_eq!(proposed.proposal.scope.thread, "matrix-thread");
            assert_eq!(proposed.proposal.tool, "room.create");
            proposed.decide(true);
        });
        session
            .host_file_call(
                &mut ledger,
                "@alice:test",
                &scope,
                "dispatch",
                "execution",
                &request(1, "answer.txt"),
                "codex-thread",
                "turn",
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], true);
        approval.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("answer.txt")).unwrap(),
            "room scoped"
        );
        session
            .host_file_call(
                &mut ledger,
                "@alice:test",
                &scope,
                "dispatch",
                "execution",
                &request(2, "answer.txt"),
                "codex-thread",
                "turn",
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], true);
        assert!(
            session
                .host_file_call(
                    &mut ledger,
                    "@alice:test",
                    &scope,
                    "dispatch",
                    "execution",
                    &request(3, "changed.txt"),
                    "codex-thread",
                    "turn",
                    &queue
                )
                .await
                .is_err()
        );
        assert!(!root.join("changed.txt").exists());
    }
    #[tokio::test]
    async fn requester_deny_does_not_ask_owner_or_touch_files() {
        let (_temp, mut ledger, scope, mut session, mut peer, root, _) = fixture(ToolPolicy::Deny);
        let (queue, mut pending) = approval_queue(1).unwrap();
        session
            .host_file_call(
                &mut ledger,
                "@alice:test",
                &scope,
                "dispatch",
                "execution",
                &request(1, "answer.txt"),
                "codex-thread",
                "turn",
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], false);
        assert!(pending.try_recv().is_err());
        assert!(!root.join("answer.txt").exists());
    }
    #[tokio::test]
    async fn remote_revocation_while_owner_decides_stops_real_side_effect() {
        let (_temp, mut ledger, scope, mut session, mut peer, root, live) =
            fixture(ToolPolicy::AskOwner);
        let (queue, mut pending) = approval_queue(1).unwrap();
        let approval = tokio::spawn(async move {
            let proposed = pending.recv().await.unwrap();
            live.store(false, Ordering::Release);
            proposed.decide(true);
        });
        session
            .host_file_call(
                &mut ledger,
                "@alice:test",
                &scope,
                "dispatch",
                "execution",
                &request(1, "answer.txt"),
                "codex-thread",
                "turn",
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], false);
        approval.await.unwrap();
        assert!(!root.join("answer.txt").exists());
    }
    #[tokio::test]
    async fn cross_thread_and_model_selected_root_are_refused() {
        let (_temp, mut ledger, scope, mut session, mut peer, root, _) =
            fixture(ToolPolicy::AskOwner);
        let (queue, mut pending) = approval_queue(1).unwrap();
        let mut frame = request(1, "answer.txt");
        frame["params"]["threadId"] = "other".into();
        assert!(
            session
                .host_file_call(
                    &mut ledger,
                    "@alice:test",
                    &scope,
                    "dispatch",
                    "execution",
                    &frame,
                    "codex-thread",
                    "turn",
                    &queue
                )
                .await
                .is_err()
        );
        let mut frame = request(2, "answer.txt");
        frame["params"]["arguments"]["root"] = "/private".into();
        session
            .host_file_call(
                &mut ledger,
                "@alice:test",
                &scope,
                "dispatch",
                "execution",
                &frame,
                "codex-thread",
                "turn",
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], false);
        assert!(pending.try_recv().is_err());
        assert!(!root.join("answer.txt").exists());
    }
    #[tokio::test]
    async fn complete_dynamic_protocol_charges_usage_in_separate_capability_context() {
        let (_temp, mut ledger, scope, mut prior, _peer, root, _) = fixture(ToolPolicy::AskOwner);
        let (client, server) = tokio::io::duplex(65536);
        let (r, w) = tokio::io::split(client);
        let mut session = Session::new(r, w);
        session.host_files = prior.host_files.take();
        session.thread = Some("codex-thread".into());
        session.cwd = Some(root.to_string_lossy().into());
        session.model = Some("test-model".into());
        session.effort = Some("low".into());
        session.bound_scope = Some(scope.clone());
        let peer = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut r = BufReader::new(r);
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            let start: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(start["method"], "turn/start");
            assert_eq!(start["params"]["sandboxPolicy"]["networkAccess"], false);
            for frame in [
                value!({"id":start["id"],"result":{"turn":{"id":"turn"}}}),
                request(900, "answer.txt"),
            ] {
                w.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
            }
            line.clear();
            r.read_line(&mut line).await.unwrap();
            let result: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(result["result"]["success"], true);
            for frame in [
                value!({"method":"thread/tokenUsage/updated","params":{"threadId":"codex-thread","turnId":"turn","tokenUsage":{"total":{"inputTokens":3,"outputTokens":2,"cachedInputTokens":0,"reasoningOutputTokens":0,"totalTokens":5}}}}),
                value!({"method":"item/completed","params":{"threadId":"codex-thread","turnId":"turn","item":{"id":"answer","type":"agentMessage","text":"created"}}}),
                value!({"method":"turn/completed","params":{"threadId":"codex-thread","turn":{"id":"turn","status":"completed"}}}),
            ] {
                w.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
            }
        });
        let (queue, mut pending) = approval_queue(1).unwrap();
        let approval = tokio::spawn(async move { pending.recv().await.unwrap().decide(true) });
        let completed = session
            .run(
                &mut ledger,
                "@alice:test",
                &scope,
                "model-execution",
                "model-dispatch",
                "create answer",
                BudgetMode::Estimated { reservation: 10 },
                &queue,
            )
            .await
            .unwrap();
        assert_eq!(completed.text, "created");
        assert_eq!(completed.usage.input, 3);
        assert_eq!(completed.usage.output, 2);
        assert_eq!(completed.tool_requests, 1);
        assert_eq!(completed.tool_returns, 1);
        assert_eq!(
            ledger
                .account(&scope, Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (5, 20)
        );
        assert_eq!(
            std::fs::read_to_string(root.join("answer.txt")).unwrap(),
            "room scoped"
        );
        assert!(ledger.context_session(&scope).unwrap().is_none());
        approval.await.unwrap();
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn retained_session_rebinds_host_file_authority_and_call_cache_per_dispatch() {
        let (_temp, mut ledger, scope, mut prior, _peer, root, old_gate) =
            fixture(ToolPolicy::AskOwner);
        let owner_directory = root.parent().unwrap().parent().unwrap().to_path_buf();
        let (client, server) = tokio::io::duplex(65536);
        let (r, w) = tokio::io::split(client);
        let mut session = Session::new(r, w);
        session.host_files = prior.host_files.take();
        session.thread = Some("codex-thread".into());
        session.cwd = Some(root.to_string_lossy().into());
        session.bound_scope = Some(scope.clone());
        let peer = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut r = BufReader::new(r);
            for round in 0..2 {
                let mut line = String::new();
                r.read_line(&mut line).await.unwrap();
                let start: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(start["method"], "turn/start");
                let turn = format!("turn-{round}");
                let mut callback = request(900, &format!("answer-{round}.txt"));
                callback["params"]["turnId"] = turn.clone().into();
                for frame in [
                    value!({"id":start["id"],"result":{"turn":{"id":turn}}}),
                    callback,
                ] {
                    w.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
                }
                line.clear();
                r.read_line(&mut line).await.unwrap();
                let response: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    response["result"]["success"], true,
                    "fresh gate and approval required for round {round}"
                );
                for frame in [
                    value!({"method":"rawResponse/completed","params":{"threadId":"codex-thread","turnId":turn,"responseId":format!("response-{round}"),"usage":{"inputTokens":3,"outputTokens":2,"cachedInputTokens":0,"reasoningOutputTokens":0,"totalTokens":5}}}),
                    value!({"method":"item/completed","params":{"threadId":"codex-thread","turnId":turn,"item":{"id":"answer","type":"agentMessage","text":"created"}}}),
                    value!({"method":"turn/completed","params":{"threadId":"codex-thread","turn":{"id":turn,"status":"completed"}}}),
                ] {
                    w.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
                }
            }
        });
        for round in 0..2 {
            if round != 0 {
                old_gate.store(false, Ordering::Release);
                assert!(session.can_continue());
                session.suspend_host_files();
                let workspace = Workspace::open(&owner_directory, "@alice:test", &scope).unwrap();
                session
                    .rebind_host_files(workspace, Arc::new(Gate(Arc::new(AtomicBool::new(true)))))
                    .unwrap();
            }
            let dispatch = format!("dispatch-{round}");
            let expected_dispatch = dispatch.clone();
            let (queue, mut pending) = approval_queue(1).unwrap();
            let approval = tokio::spawn(async move {
                let proposed = pending.recv().await.unwrap();
                assert_eq!(proposed.proposal.dispatch, expected_dispatch);
                proposed.decide(true);
            });
            let completed = session
                .run(
                    &mut ledger,
                    "@alice:test",
                    &scope,
                    &format!("call-{round}"),
                    &dispatch,
                    "create answer",
                    BudgetMode::Estimated { reservation: 10 },
                    &queue,
                )
                .await
                .unwrap();
            assert_eq!(completed.usage.input + completed.usage.output, 5);
            assert_eq!(
                std::fs::read_to_string(root.join(format!("answer-{round}.txt"))).unwrap(),
                "room scoped"
            );
            approval.await.unwrap();
        }
        assert_eq!(
            ledger
                .account(&scope, Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (10, 20)
        );
        peer.await.unwrap();
    }
}
