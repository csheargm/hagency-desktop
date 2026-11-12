//! Dynamic host tools, deliberately separate from native Codex tools.
use super::*;
use crate::room_files::{FileError, Operation, PreparedCall, Workspace};
use std::{future::Future, pin::Pin, sync::Arc};
use hagency_runtime::codex::tree::TurnTree;

mod cache;

type Decision = Pin<Box<dyn Future<Output = bool> + Send>>;
struct Waiting {
    answer: Answer,
    decision: Decision,
}
#[derive(Default)]
pub(super) struct Calls(BTreeMap<String, Waiting>, BTreeSet<String>);
pub(super) struct Answer {
    id: Value,
    thread: String,
    turn: String,
    call: String,
    digest: String,
    prepared: PreparedCall,
    allowed: bool,
}
impl Calls {
    pub fn contains(&self, call: &str) -> bool {
        self.0.values().any(|w| w.answer.call == call)
    }
    fn can_offer(&self, call: &str) -> bool {
        self.0.len() < 16 && !self.1.contains(call)
    }
    fn offer(
        &mut self,
        answer: Answer,
        gate: Arc<dyn HostToolGate>,
        dispatch: &str,
        execution: &str,
        queue: &ApprovalQueue,
    ) -> Result<()> {
        if !self.can_offer(&answer.call) {
            return Err(Error::Protocol("pending host call capacity or duplicate"));
        }
        self.1.insert(answer.call.clone());
        let (decision, receiver) = oneshot::channel();
        let offered = queue
            .sender
            .try_send(PendingApproval {
                proposal: answer.prepared.proposal().clone(),
                decision,
            })
            .is_ok();
        let dispatch = dispatch.to_owned();
        let execution = execution.to_owned();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        let decision = Box::pin(async move {
            offered
                && matches!(
                    tokio::time::timeout_at(deadline, receiver).await,
                    Ok(Ok(true))
                )
                && matches!(
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        gate.authorize(&dispatch, &execution)
                    )
                    .await,
                    Ok(true)
                )
        });
        self.0
            .insert(answer.id.to_string(), Waiting { answer, decision });
        Ok(())
    }
    pub async fn next(&mut self) -> Answer {
        let (key, allowed) = std::future::poll_fn(|cx| {
            for (key, wait) in &mut self.0 {
                if let std::task::Poll::Ready(value) = wait.decision.as_mut().poll(cx) {
                    return std::task::Poll::Ready((key.clone(), value));
                }
            }
            std::task::Poll::Pending
        })
        .await;
        let mut answer = self.0.remove(&key).expect("original host file wait").answer;
        answer.allowed = allowed;
        answer
    }
    pub fn withdraw(&mut self, params: &Value) {
        self.0.retain(|_, wait| !wait.answer.withdrawn(params));
    }
    pub fn retain_active(&mut self, tree: &TurnTree) {
        self.0.retain(|_, wait| wait.answer.active(tree));
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
impl Answer {
    pub fn active(&self, tree: &TurnTree) -> bool {
        tree.is_active(&self.thread, &self.turn)
    }
    pub fn withdrawn(&self, params: &Value) -> bool {
        params["threadId"] == self.thread && params["requestId"] == self.id
    }
    pub fn call(&self) -> &str {
        &self.call
    }
}
fn file_result<T: Serialize>(outcome: std::result::Result<T, FileError>) -> Result<Value> {
    match outcome {
        Ok(output) => Ok(
            value!({"success":true,"contentItems":[{"type":"inputText","text":serde_json::to_string(&output)?}]}),
        ),
        Err(FileError::Unknown) => Err(Error::Unknown),
        Err(FileError::NeedsOwner | FileError::Denied) => Ok(failure("host_file_denied")),
        Err(FileError::Conflict) => Ok(failure("file_exists_or_changed")),
        Err(FileError::Capacity) => Ok(failure("file_capacity_exceeded")),
        Err(_) => Ok(failure("host_file_unavailable")),
    }
}

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
    /// Identity of an owner-selected shared project, fixed when the
    /// capability is attached. `None` for a private Room folder.
    shared: Option<String>,
    gate: Option<Arc<dyn HostToolGate>>,
    calls: cache::Cache,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    path: String,
    content: String,
    expected_sha256: String,
}

pub(super) fn specifications(shared: bool) -> Value {
    let mut tools = value!([
        {"type":"function","name":"hagency_file_list","description":"List the authorized workspace. Use an empty path for the root.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}},
        {"type":"function","name":"hagency_file_read","description":"Read a UTF-8 file in the authorized workspace.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}},
        {"type":"function","name":"hagency_file_create","description":"Create a new UTF-8 file in the authorized workspace. Existing files cannot be replaced.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}
    ]);
    if shared {
        tools.as_array_mut().unwrap().push(value!({"type":"function","name":"hagency_directory_create","description":"Create a new directory inside the authorized shared workspace.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}));
        tools.as_array_mut().unwrap().push(value!({"type":"function","name":"hagency_file_update","description":"Replace an existing UTF-8 file in the authorized shared workspace only if its SHA-256 is unchanged since hagency_file_read.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"expected_sha256":{"type":"string"}},"required":["path","content","expected_sha256"],"additionalProperties":false}}));
    }
    tools
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
        Some("hagency_directory_create") => {
            let i: PathInput = serde_json::from_value(args)?;
            Ok(Operation::MakeDirectory { path: i.path })
        }
        Some("hagency_file_update") => {
            let i: UpdateInput = serde_json::from_value(args)?;
            use sha2::{Digest, Sha256};
            let nonce = format!("{:x}", Sha256::digest(serde_json::to_vec(&(owner, scope, dispatch, params))?));
            Ok(Operation::Update { path: i.path, content: i.content, expected_sha256: i.expected_sha256, nonce })
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
    /// Whether this capability serves an owner-selected shared project.
    pub(super) fn is_shared(&self) -> bool {
        self.shared.is_some()
    }
    /// The shared project's identity as it was when the capability was attached.
    pub(super) fn shared_pin(&self) -> Option<&str> {
        self.shared.as_deref()
    }
}
/// A shared project is identified by its path, device and inode; a private
/// Room folder has no such identity.
fn shared_identity(workspace: &Workspace) -> Result<Option<String>> {
    if !workspace.is_shared() {
        return Ok(None);
    }
    workspace
        .shared_pin()
        .map(Some)
        .map_err(|_| Error::Profile("workspace identity"))
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
        let shared = shared_identity(&workspace)?;
        self.host_files = Some(HostFiles {
            directory: workspace.canonical_directory().to_path_buf(),
            shared,
            workspace: Some(workspace),
            gate: Some(gate),
            calls: Default::default(),
        });
        Ok(())
    }
    /// Retain tool definitions, but never a previous dispatch's permission.
    pub fn suspend_host_files(&mut self) {
        if let Some(files) = self.host_files.as_mut() {
            files.gate = None;
            files.calls = Default::default();
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
        // A retained session keeps the kind of workspace it started with: a
        // shared project is never swapped for another one or for a Room folder.
        let shared = shared_identity(&workspace)?;
        let files = self
            .host_files
            .as_mut()
            .ok_or(Error::Protocol("host capability missing"))?;
        if files.gate.is_some()
            || files.workspace.is_some()
            || files.workspace_directory() != workspace.canonical_directory()
            || files.shared != shared
        {
            return Err(Error::Protocol(
                "host capability still active or workspace changed",
            ));
        }
        files.workspace = Some(workspace);
        files.calls = Default::default();
        files.gate = Some(gate);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn host_file_offer(
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
        waiting: &mut Calls,
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
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), gate.authorize(dispatch, execution)).await,
            Ok(true)
        ) {
            return self
                .write(value!({"id":id,"result":failure("server_scope_unavailable")}))
                .await;
        }
        if let Some((prior, prepared, result)) = files.calls.get(call)? {
            if prior != digest {
                return Err(Error::Protocol("host call arguments changed"));
            }
            let result = if ledger.tool_disposition(&prepared, now()).is_ok() {
                result.clone()
            } else {
                failure("local_policy_changed")
            };
            return self.write(value!({"id":id,"result":result})).await;
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
        let outcome = workspace.execute(ledger, &prepared, now());
        if matches!(outcome, Err(FileError::NeedsOwner)) {
            if !waiting.can_offer(call) {
                return self
                    .write(value!({"id":id,"result":failure("host_file_busy")})).await;
            }
            return waiting.offer(
                Answer {
                    id,
                    thread: thread.into(),
                    turn: turn.into(),
                    call: call.into(),
                    digest,
                    prepared,
                    allowed: false,
                },
                gate,
                dispatch, execution,
                queue,
            );
        }
        let result = file_result(outcome)?;
        files
            .calls
            .insert(call, &digest, prepared.proposal(), &result)?;
        self.write(value!({"id":id,"result":result})).await
    }

    pub(super) fn host_file_response(
        &mut self,
        ledger: &mut Ledger,
        owner: &str,
        answer: Answer,
    ) -> Result<Value> {
        let files = self
            .host_files
            .as_mut()
            .ok_or(Error::Protocol("host files withdrawn"))?;
        let workspace = files
            .workspace
            .as_mut()
            .ok_or(Error::Protocol("host workspace withdrawn"))?;
        let result = if answer.allowed
            && ledger
                .approve_tool(owner, answer.prepared.proposal(), now())
                .is_ok()
        {
            file_result(workspace.execute(ledger, &answer.prepared, now()))?
        } else {
            failure("host_file_denied")
        };
        files.calls.insert(&answer.call, &answer.digest, answer.prepared.proposal(), &result)?;
        Ok(value!({"id":answer.id,"result":result}))
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn host_file_call(
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
        let mut waiting = Calls::default();
        self.host_file_offer(
            ledger,
            owner,
            scope,
            dispatch,
            execution,
            frame,
            thread,
            turn,
            queue,
            &mut waiting,
        )
        .await?;
        if !waiting.0.is_empty() {
            let answer = waiting.next().await;
            let response = self.host_file_response(ledger, owner, answer)?;
            self.write(response).await?;
        }
        Ok(())
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
        let policy = if let ToolPolicy::AllowWithRules { tools, .. } = policy {
            ToolPolicy::AllowWithRules { tools, directories: vec![root.to_str().unwrap().into()] }
        } else { policy };
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
    async fn host_file_replays_survive_more_than_128_calls_without_reexecution() {
        let (_temp, mut ledger, scope, mut session, mut peer, root, live) = fixture(
            ToolPolicy::AllowWithRules { tools: vec!["room.create".into()], directories: vec![] });
        let (queue, _) = approval_queue(1).unwrap();
        let mut original = Value::Null;
        for n in 0..200 {
            let mut frame = request(n + 1, &format!("file-{n}.txt"));
            frame["params"]["callId"] = format!("call-{n}").into();
            session.host_file_call(&mut ledger, "@alice:test", &scope, "dispatch", "execution",
                &frame, "codex-thread", "turn", &queue).await.unwrap();
            let reply = response(&mut peer).await;
            assert_eq!(reply["result"]["success"], true, "call {n}: {reply}");
            if n == 0 { original = reply["result"].clone(); }
        }
        std::fs::write(root.join("file-0.txt"), "changed after execution").unwrap();
        let mut replay = request(201, "file-0.txt"); replay["params"]["callId"] = "call-0".into();
        session.host_file_call(&mut ledger,"@alice:test",&scope,"dispatch","execution",&replay,
            "codex-thread","turn",&queue).await.unwrap();
        assert_eq!(response(&mut peer).await["result"], original);
        assert_eq!(std::fs::read_to_string(root.join("file-0.txt")).unwrap(), "changed after execution");
        replay["id"] = 202.into(); replay["params"]["arguments"]["content"] = "different".into();
        assert!(session.host_file_call(&mut ledger,"@alice:test",&scope,"dispatch","execution",&replay,
            "codex-thread","turn",&queue).await.is_err());
        live.store(false, Ordering::Release); replay["id"] = 203.into();
        session.host_file_call(&mut ledger,"@alice:test",&scope,"dispatch","execution",&replay,
            "codex-thread","turn",&queue).await.unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], false);
    }
    #[tokio::test]
    async fn waiting_host_file_is_withdrawable_and_duplicate_ready_call_cannot_ask_twice() {
        for withdraw in [false, true] {
            let (_temp, mut ledger, scope, mut session, mut peer, root, _) =
                fixture(ToolPolicy::AskOwner);
            let (queue, mut incoming) = approval_queue(2).unwrap();
            let mut waiting = Calls::default();
            session
                .host_file_offer(
                    &mut ledger,
                    "@alice:test",
                    &scope,
                    "dispatch",
                    "execution",
                    &request(1, "answer.txt"),
                    "codex-thread",
                    "turn",
                    &queue,
                    &mut waiting,
                )
                .await
                .unwrap();
            let pending = incoming.recv().await.unwrap();
            assert!(!root.join("answer.txt").exists());
            if withdraw {
                waiting.clear();
                assert!(pending.is_closed());
                pending.decide(true);
                assert!(!root.join("answer.txt").exists());
            } else {
                pending.decide(true);
                let answer = waiting.next().await;
                session
                    .host_file_offer(
                        &mut ledger,
                        "@alice:test",
                        &scope,
                        "dispatch",
                        "execution",
                        &request(2, "answer.txt"),
                        "codex-thread",
                        "turn",
                        &queue,
                        &mut waiting,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response(&mut peer).await["result"]["contentItems"][0]["text"],
                    "host_file_busy"
                );
                assert!(incoming.try_recv().is_err());
                assert!(!root.join("answer.txt").exists());
                assert_eq!(
                    session
                        .host_file_response(&mut ledger, "@alice:test", answer)
                        .unwrap()["result"]["success"],
                    true
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("answer.txt")).unwrap(),
                    "room scoped"
                );
            }
        }
    }
    #[tokio::test]
    async fn shared_update_uses_exact_approval_and_requires_live_gate() {
        use sha2::{Digest, Sha256};
        let (temp, mut ledger, scope, mut session, mut peer, root, live) = fixture(ToolPolicy::AskOwner);
        let owner = root.parent().unwrap().parent().unwrap().to_path_buf();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        std::fs::write(project.join("file.txt"), "old").unwrap();
        session.host_files = None;
        session.enable_host_files(Workspace::open_shared(&owner, &project, "@alice:test", &scope).unwrap(), Arc::new(Gate(live.clone()))).unwrap();
        let private = specifications(false).to_string();
        let shared = specifications(true).to_string();
        assert!(!private.contains("hagency_file_update"));
        assert!(shared.contains("hagency_file_update") && shared.contains("hagency_directory_create"));
        let request = value!({"id":1,"method":"item/tool/call","params":{"threadId":"codex-thread","turnId":"turn","callId":"update","tool":"hagency_file_update","arguments":{"path":"file.txt","content":"new","expected_sha256":format!("{:x}",Sha256::digest(b"old"))}}});
        let (queue, mut pending) = approval_queue(1).unwrap();
        let approval = tokio::spawn(async move {
            let proposed = pending.recv().await.unwrap();
            assert_eq!(proposed.proposal.tool, "room.update");
            assert_eq!(proposed.proposal.arguments["file"]["content"], "new");
            proposed.decide(true);
        });
        session.host_file_call(&mut ledger, "@alice:test", &scope, "dispatch", "execution", &request, "codex-thread", "turn", &queue).await.unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], true);
        approval.await.unwrap();
        assert_eq!(std::fs::read_to_string(project.join("file.txt")).unwrap(), "new");
        live.store(false, Ordering::Release);
        let mut replay = request.clone(); replay["id"] = 2.into();
        session.host_file_call(&mut ledger, "@alice:test", &scope, "dispatch", "execution", &replay, "codex-thread", "turn", &queue).await.unwrap();
        assert_eq!(response(&mut peer).await["result"]["success"], false);
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
                BudgetMode::Postpaid,
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
                    BudgetMode::Postpaid,
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
