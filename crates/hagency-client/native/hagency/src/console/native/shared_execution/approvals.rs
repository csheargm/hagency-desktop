//! One-shot consumption before response writes; no durable cross-session grants.
use super::*;
use hagency_runtime::{
    codex::{
        approval::ApprovalRequest,
        session::{ApprovalControlPolicy, ObservationKind, PreparedUpdate},
    },
    owned::OwnedSession,
};
use serde_json::{Value, json};
struct EncryptedPending {
    device: Arc<crate::console::owned_runtime::e2ee::AgentDevice>,
    row: Value,
    request: Value,
    nonce: String,
    last_send: Instant,
}
struct Pending {
    request: ApprovalRequest,
    id: String,
    digest: String,
    deadline: tokio::time::Instant,
    encrypted: Option<EncryptedPending>,
    /// Refused here without asking anyone: the request would leave the
    /// Agent's folder or its sandbox on the owner's computer.
    refused: bool,
}
pub(super) enum RoomCall {
    History(hagency_runtime::codex::session::RoomHistoryCall),
    File(hagency_runtime::codex::session::RoomFileCall),
}
#[derive(Default)]
pub(super) struct Approvals {
    room_calls: Vec<RoomCall>,
    pending: Vec<Pending>,
    /// The model's questions, and Codex's withdrawals of them, that arrived
    /// while a decision was being sent, in arrival order. The driver hands
    /// them on.
    questions: Vec<super::questions::QuestionUpdate>,
}
pub(super) fn enable(process: &mut OwnedSession) -> Result<(), NativeError> {
    process
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 120_000,
            response_reserve_ms: 5_000,
        })
        .map_err(|_| invalid())?;
    process.enable_owner_wait_expiry().map_err(|_| invalid())
}
impl Approvals {
    pub(super) fn take_room_calls(&mut self) -> Vec<RoomCall> { std::mem::take(&mut self.room_calls) }

    /// Question updates observed by `poll`, in order, each handed out once.
    pub(super) fn take_questions(&mut self) -> Vec<super::questions::QuestionUpdate> {
        std::mem::take(&mut self.questions)
    }
    pub async fn register(
        &mut self,
        io: &Io,
        b: &Bound,
        work: &SharedWorkItem,
        a: &SharedActivityRef,
        until: Instant,
        process: &OwnedSession,
        request: ApprovalRequest,
    ) -> Result<(), NativeError> {
        if self.pending.len() >= 16 || self.pending.iter().any(|p| p.request.id() == request.id()) {
            return Err(invalid());
        }
        let deadline = process
            .approval_deadline(request.id())
            .map_err(|_| invalid())?;
        // The person who sent the message decides requests inside the
        // Agent's folder. Anything else would run outside the sandbox on the
        // owner's computer: it is refused at once, and nobody is asked.
        if !within_workspace(request.method(), request.params(), process.shared_workspace()) {
            self.pending.push(Pending {
                request,
                id: String::new(),
                digest: String::new(),
                deadline,
                encrypted: None,
                refused: true,
            });
            return Ok(());
        }
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis()
            .min(120_000) as u64;
        let id = format!(
            "sap_{}",
            super::digest(&json!({"work":work.id,"request":request.id()}))?
        );
        let request_json = json!({"id":request.id(),"method":request.method(),"params":request.params(),"sessionAllowed":request.session_response().is_ok(),"managerCanDecide":true});
        let private = if work.encrypted {
            Some((
                super::e2ee::device(io, b, &work.assignment_id).await?,
                crate::encrypted_approvals::nonce().map_err(|_| invalid())?,
            ))
        } else {
            None
        };
        let wire = if let Some((device, nonce)) = &private {
            crate::encrypted_approvals::metadata(
                &request_json,
                nonce,
                device
                    .approval_client()
                    .device_id()
                    .ok_or_else(invalid)?
                    .as_str(),
            )
            .map_err(|_| invalid())?
        } else {
            request_json.clone()
        };
        // The server returns the row it already holds for the same id and
        // request, so a registration refused during a credential refresh is
        // sent again under a fresh scope instead of failing the turn.
        let mut rebound: Option<Bound> = None;
        let reply: Timed<Value> = loop {
            let error = match rebound
                .as_ref()
                .unwrap_or(b)
                .upload(
                    Action::ApprovalRegister {
                        authority: a.clone(),
                        work: work.clone(),
                        id: id.clone(),
                        request: wire.clone(),
                        remaining_ms: remaining,
                    },
                    until,
                )
                .await
            {
                Ok(reply) => break reply,
                Err(error) => error,
            };
            if !io.transient(&error) || Instant::now() + io::RETRY_GAP >= until {
                return Err(error);
            }
            tracing::info!(code = %error.code, "Shared approval registration resubmitting");
            tokio::time::sleep(io::RETRY_GAP).await;
            rebound = Some(io.settled(until, || io.bound()).await?);
        };
        if reply.value["approval"]["id"] != id
            || reply.value["approval"]["workId"] != work.id
            || reply.value["approval"]["request"] != wire
            || reply.value["approval"]["deciderMxid"] != work.requester_mxid.as_str()
        {
            return Err(invalid());
        }
        let digest = reply.value["digest"]
            .as_str()
            .ok_or_else(invalid)?
            .to_owned();
        hash(&digest)?;
        if super::digest(&reply.value["approval"])? != digest {
            return Err(invalid());
        }
        let encrypted = if let Some((device, nonce)) = private {
            // Delivery can be retried independently of immutable metadata registration.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                crate::encrypted_approvals::send_request(
                    device.approval_client(),
                    &reply.value,
                    &request_json,
                    &nonce,
                ),
            )
            .await;
            Some(EncryptedPending {
                device,
                row: reply.value,
                request: request_json,
                nonce,
                last_send: Instant::now(),
            })
        } else {
            None
        };
        self.pending.push(Pending {
            request,
            id,
            digest,
            deadline,
            encrypted,
            refused: false,
        });
        Ok(())
    }
    pub async fn poll(
        &mut self,
        io: &Io,
        b: &Bound,
        work: &SharedWorkItem,
        a: &SharedActivityRef,
        until: Instant,
        process: &mut OwnedSession,
        capture: &mut SharedCallSink,
        usage: &mut crate::console::native::telemetry::TurnTelemetry,
        mut history_reader: Option<&mut super::history::Reader<super::history::Source>>,
    ) -> Result<Option<SharedWorkReceipt>, NativeError> {
        let mut i = 0;
        while i < self.pending.len() {
            // Codex withdrew this request, or the turn that asked ended: the
            // process no longer holds its callback and nothing waits for an
            // answer. Deciding or expiring it would fail the whole turn.
            if process.approval_deadline(self.pending[i].request.id()).is_err() {
                self.pending.remove(i);
                continue;
            }
            let p = &mut self.pending[i];
            let mut expired = false;
            let decision = if p.refused {
                Some("deny".to_owned())
            } else if tokio::time::Instant::now() >= p.deadline {
                process
                    .expire_approval(p.request.id())
                    .map_err(|_| invalid())?;
                expired = true;
                Some("deny".to_owned())
            } else {
                if let Some(e) = &mut p.encrypted {
                    if e.device.is_revoked() {
                        return Err(failure(409, "encrypted_approval_device_changed"));
                    }
                    if !e
                        .device
                        .approvals
                        .has_decision(&e.row)
                        // An expired or invalid envelope never authorizes a
                        // tool. Keep waiting for the process's own deny timer.
                        .unwrap_or(false)
                    {
                        if e.last_send.elapsed() >= Duration::from_secs(10) {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(5),
                                crate::encrypted_approvals::send_request(
                                    e.device.approval_client(),
                                    &e.row,
                                    &e.request,
                                    &e.nonce,
                                ),
                            )
                            .await;
                            e.last_send = Instant::now();
                        }
                        i += 1;
                        continue;
                    }
                }
                let reply: Timed<Value> = match b
                    .upload(
                        Action::ApprovalTake {
                            authority: a.clone(),
                            id: p.id.clone(),
                            digest: p.digest.clone(),
                        },
                        until,
                    )
                    .await
                {
                    Ok(reply) => reply,
                    // Refused during a credential refresh: nothing was decided
                    // here. Ask again on the next pass, under a fresh scope.
                    Err(error) if io.transient(&error) => {
                        tracing::info!(code = %error.code, "Shared approval decision read retrying");
                        i += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if reply.value["id"] != p.id || reply.value["digest"] != p.digest {
                    return Err(invalid());
                }
                let authenticated = if let Some(e) = &p.encrypted {
                    match reply.value["decision"].as_str() {
                        None => true,
                        Some(decision) => e
                            .device
                            .approvals
                            .matches_decision(
                                &e.row,
                                reply.value["decidedByMxid"].as_str().unwrap_or(""),
                                decision,
                            )
                            .unwrap_or(false),
                    }
                } else {
                    true
                };
                if !authenticated {
                    Some("deny".into())
                } else if matches!(
                    reply.value["decision"].as_str(),
                    Some("approve_once" | "approve_session")
                ) && reply.value["ownerApproved"] != true
                    && !within_workspace(
                        p.request.method(),
                        p.request.params(),
                        process.shared_workspace(),
                    )
                {
                    Some("deny".into())
                } else {
                    reply.value["decision"].as_str().map(str::to_owned)
                }
            };
            let Some(mut decision) = decision else {
                i += 1;
                continue;
            };
            // An answer read just before the owner bound can no longer be
            // prepared before it. From the bound on the process accepts only
            // its own decline, so the request expires instead of the turn
            // failing on the late answer.
            if !expired && tokio::time::Instant::now() >= p.deadline {
                process
                    .expire_approval(p.request.id())
                    .map_err(|_| invalid())?;
                decision = "deny".to_owned();
            }
            let p = self.pending.remove(i);
            let response = match decision.as_str() {
                "approve_once" => p.request.response(true),
                "approve_session" => p.request.session_response().map_err(|_| invalid())?,
                "deny" => p.request.response(false),
                _ => return Err(invalid()),
            };
            // A consumed server decision cannot be replayed after uncertain IO or restart.
            let mut prepared = process.prepare_approval(response).map_err(|_| invalid())?;
            loop {
                match process
                    .send_prepared_approval(&mut prepared)
                    .await
                    .map_err(|_| invalid())?
                {
                    PreparedUpdate::WriteAccepted(_) => break,
                    PreparedUpdate::Update(update, observation) => {
                        let event = process
                            .shared_event(update, *observation)
                            .map_err(|_| invalid())?;
                        let next = event.approval().cloned();
                        let history = event.room_history_call().cloned();
                        let file = event.room_file_call().cloned();
                        let question = event.user_input().cloned();
                        let withdrawn = event.user_input_resolved().cloned();
                        let context = event.observation().context_usage().cloned();
                        let ended =
                            matches!(event.observation().kind(), ObservationKind::TurnEnded(_));
                        let pending = usage.pending(&event);
                        let receipt = capture.observe(event).await.map_err(|e| {
                            usage.abandon();
                            store(e)
                        })?;
                        usage.accept(pending);
                        usage.quota(process);
                        if let (Some(reader), Some(context)) = (history_reader.as_mut(), context.as_ref()) {
                            reader.observe_context(context);
                        }
                        if ended {
                            return Ok(Some(receipt.ok_or_else(invalid)?));
                        }
                        self.room_calls.extend(history.map(RoomCall::History));
                        self.room_calls.extend(file.map(RoomCall::File));
                        if let Some(next) = next {
                            self.register(io, b, work, a, until, process, next).await?;
                        }
                        // One event is a question or a withdrawal, never both.
                        self.questions
                            .extend(question.map(super::questions::QuestionUpdate::Asked));
                        self.questions
                            .extend(withdrawn.map(super::questions::QuestionUpdate::Withdrawn));
                    }
                }
            }
        }
        Ok(None)
    }
}

// Command approvals can execute outside the sandbox, so only the owner may
// grant them. Delegation is limited to literal, canonical workspace file paths.
fn within_workspace(method: &str, params: &Value, workspace: &std::path::Path) -> bool {
    let Ok(root) = workspace.canonicalize() else {
        return false;
    };
    let inside = |value: &Value| -> bool {
        let Some(raw) = value.as_str() else {
            return false;
        };
        let path = std::path::Path::new(raw);
        if !path.is_absolute()
            || !path.starts_with(&root)
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return false;
        }
        let mut existing = path;
        loop {
            match std::fs::symlink_metadata(existing) {
                Ok(_) => return existing.canonicalize().is_ok_and(|p| p.starts_with(&root)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let Some(parent) = existing.parent() else {
                        return false;
                    };
                    existing = parent;
                }
                Err(_) => return false,
            }
        }
    };
    if method == "item/fileChange/requestApproval" {
        return inside(&params["grantRoot"]);
    }
    if method != "item/permissions/requestApproval" {
        return false;
    }
    let profile = &params["permissions"];
    if profile["network"]["enabled"] == true {
        return false;
    }
    let Some(fs) = profile["fileSystem"].as_object() else {
        return false;
    };
    if fs.get("globScanMaxDepth").is_some_and(|v| !v.is_null()) {
        return false;
    }
    let mut count = 0;
    for key in ["read", "write"] {
        if let Some(paths) = fs.get(key).filter(|v| !v.is_null()) {
            let Some(paths) = paths.as_array() else {
                return false;
            };
            for path in paths {
                if !inside(path) {
                    return false;
                }
                count += 1;
            }
        }
    }
    if let Some(entries) = fs.get("entries").filter(|v| !v.is_null()) {
        let Some(entries) = entries.as_array() else {
            return false;
        };
        for e in entries {
            if !matches!(e["access"].as_str(), Some("read" | "write"))
                || e["path"]["type"] != "path"
                || !inside(&e["path"]["path"])
            {
                return false;
            }
            count += 1;
        }
    }
    count > 0
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_delegation_rejects_network_commands_traversal_and_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let p = root.join("new.txt");
        let mut params = json!({"permissions":{"fileSystem":{"write":[p]}}});
        assert!(within_workspace(
            "item/permissions/requestApproval",
            &params,
            &root
        ));
        params["permissions"]["network"] = json!({"enabled":true});
        assert!(!within_workspace(
            "item/permissions/requestApproval",
            &params,
            &root
        ));
        assert!(!within_workspace(
            "item/commandExecution/requestApproval",
            &json!({"cwd":root,"command":"anything"}),
            &root
        ));
        assert!(!within_workspace(
            "item/fileChange/requestApproval",
            &json!({"grantRoot":root.join("../escape")}),
            &root
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", root.join("escape")).unwrap();
            assert!(!within_workspace(
                "item/fileChange/requestApproval",
                &json!({"grantRoot":root.join("escape/tmp")}),
                &root
            ));
        }
    }
    #[test]
    fn room_approval_scope_excludes_sibling_rooms_and_legacy_agent_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let room = root.join("rooms/room_one");
        let sibling = root.join("rooms/room_two");
        std::fs::create_dir_all(&room).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        for (path, allowed) in [
            (room.join("new.txt"), true),
            (sibling.join("new.txt"), false),
            (root.join("legacy.txt"), false),
        ] {
            assert_eq!(
                within_workspace(
                    "item/permissions/requestApproval",
                    &json!({"permissions":{"fileSystem":{"write":[path]}}}),
                    &room
                ),
                allowed
            );
        }
    }
}
