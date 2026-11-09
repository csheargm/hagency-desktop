//! Private metadata only. A packet is neither send nor owner-decision authority.
use super::*;

const MAX_CARD: usize = 48 * 1024;

pub struct PrivateApprovalCard {
    target: ApprovalIntakeTarget,
    owner_expires_at: u64,
    content: Value,
}
impl PrivateApprovalCard {
    pub fn target(&self) -> &ApprovalIntakeTarget {
        &self.target
    }
    pub fn owner_expires_at(&self) -> u64 {
        self.owner_expires_at
    }
    /// Secret-bearing owner message. Never project into the public console.
    pub fn content(&self) -> &Value {
        &self.content
    }
}

impl DomainRepository {
    /// Current discovery metadata from authenticated bindings, never card input.
    /// The Matrix publisher supplies the monotonic state-event generation;
    /// all identity and association fields come from one database snapshot.
    pub fn private_approval_room_marker(
        &self,
        authority: &ApprovalRoomAuthority,
        device: &str,
        generation: u64,
    ) -> Result<Value, Error> {
        hagency_core::replies::generation(generation)?;
        text(device, 255)?;
        let tx = self.db.unchecked_transaction()?;
        if room_authority(&tx, &authority.engagement_id)? != *authority {
            return Err(Error::RunnerAuthority);
        }
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM current_approval_bindings c JOIN approval_bindings b ON b.engagement_id=c.engagement_id JOIN approval_rooms r ON r.server_name=b.server_name AND r.room_id=b.room_id WHERE c.engagement_id=?1 AND r.device_id=?2 AND r.owner_mxid=?3 AND r.bot_mxid=?4)",
            params![authority.engagement_id, device, authority.owner_mxid, authority.bot_mxid],
            |row| row.get(0),
        )?;
        if !current {
            return Err(Error::RunnerAuthority);
        }
        let mut statement = tx.prepare(
            "SELECT DISTINCT json_extract(e.projection,'$.agentName'),p.room_id FROM current_approval_bindings c JOIN approval_bindings b ON b.engagement_id=c.engagement_id JOIN engagements e ON e.id=b.engagement_id JOIN projects p ON p.fleet_id=e.fleet_id AND p.id=e.project_id AND p.generation=e.generation WHERE b.server_name=?1 AND b.room_id=?2 ORDER BY 1,2 LIMIT 257",
        )?;
        let rows = statement
            .query_map(params![authority.server_name, authority.room_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
        let mut associations = std::collections::BTreeSet::new();
        for row in rows {
            let (agent, room) = row?;
            hagency_core::project::AgentName::try_from(agent.clone())
                .map_err(|_| Error::RunnerAuthority)?;
            matrix_room(&room, &authority.server_name)?;
            associations.insert((agent, room));
            if associations.len() > 256 {
                return Err(Error::Capacity);
            }
        }
        let associations: Vec<_> = associations
            .into_iter()
            .map(|(agent, room)| json!({"agent":agent,"project_room_id":room,"active":true}))
            .collect();
        let marker = json!({"version":2,"binding_generation":generation,
            "publisher_mxid":authority.bot_mxid,"owner_mxid":authority.owner_mxid,
            "project_room_associations":associations});
        if serde_json::to_vec(&marker)?.len() > 48 * 1024 {
            return Err(Error::Capacity);
        }
        Ok(marker)
    }

    pub fn private_approval_card(
        &mut self,
        id: &str,
        owner_expires_at: u64,
        now: u64,
    ) -> Result<PrivateApprovalCard, Error> {
        self.private_approval_card_clock(id, owner_expires_at, || Ok(now))
    }

    pub(crate) fn private_approval_card_clock(
        &mut self,
        id: &str,
        owner_expires_at: u64,
        sample: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<PrivateApprovalCard, Error> {
        identifier(id, 128)?;
        clock(owner_expires_at)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample()?;
        clock(now)?;
        let target = intake_target(&tx, id, now)?;
        if owner_expires_at <= now || owner_expires_at > target.expires_at {
            return Err(Error::RunnerAuthority);
        }
        let (context, request) = request(&tx, id)?;
        let (description, scope_kind): (Option<String>, Option<String>) = tx.query_row(
            "SELECT description,scope_kind FROM owner_approvals WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut description = description.unwrap_or_default();
        let mut actions =
            vec![json!({"id":"approve_once","label":"Approve once","style":"primary"})];
        let scope = if target.reusable_scope {
            // Stored scope is the one already derived at request admission.
            // Formatting never recalculates or broadens its authorization.
            if description.is_empty() {
                return Err(Error::Schema);
            }
            let kind: Value = serde_json::from_str(scope_kind.as_deref().ok_or(Error::Schema)?)?;
            let scope = json!({"kind":kind,"description":description,
                "workspace":context.workspace,"task_id":context.task});
            description.push_str(&format!(
                "\nAuthorization scope for {} in project {}:\nWorkspace: {}\nThis task: {}\nAlways allow saves this exact rule for this Agent and project.",
                target.authority.agent_name, target.authority.project_id,
                context.workspace, context.task
            ));
            actions.push(
                json!({"id":"approve_task","label":"Allow for this task","style":"secondary"}),
            );
            actions.push(json!({"id":"approve_always","label":"Always allow this operation","style":"secondary"}));
            Some(scope)
        } else {
            None
        };
        actions.push(json!({"id":"deny","label":"Deny","style":"danger"}));
        // The card names the agent's framework (ADR-192, ADR-193). A Claude
        // request shows its tool and input, as the retained TS card did; an
        // Octos request its tool and what Octos says it does; a Codex request
        // shows its method and params, unchanged.
        let claude = request.method == policy::CLAUDE_TOOL_METHOD;
        let octos = request.method == policy::OCTOS_APPROVAL_METHOD;
        let runtime = if claude {
            "claude"
        } else if octos {
            "octos"
        } else {
            "codex"
        };
        let tool = if claude || octos {
            request.params["toolName"]
                .as_str()
                .filter(|tool| !tool.is_empty())
                .ok_or(Error::Schema)?
        } else {
            request.method.as_str()
        };
        let preview = if claude {
            serde_json::to_string(&request.params["input"])?
        } else if octos {
            // What Octos says the request does, as lines a person reads: its
            // title, its body, then the typed command and directory if any.
            let text = |key: &str| {
                request.params[key]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            };
            let mut lines: Vec<String> = ["title", "body"]
                .into_iter()
                .filter_map(|key| text(key).map(str::to_owned))
                .collect();
            if let Some(command) = text("command") {
                lines.push(format!("Command: {command}"));
            }
            if let Some(cwd) = text("cwd") {
                lines.push(format!("Working directory: {cwd}"));
            }
            // A request with no reusable scope has no stored description:
            // Octos's own title names it.
            if description.is_empty()
                && let Some(title) = text("title")
            {
                description = title.to_owned();
            }
            lines.join("\n")
        } else {
            serde_json::to_string(&request.params)?
        };
        // The display string and actual JSON-RPC type describe the same request;
        // neither substitutes for the owner/publisher binding below.
        let upstream_display = match &request.upstream {
            ApprovalRpcId::Number(value) => value.to_string(),
            ApprovalRpcId::String(value) => value.clone(),
        };
        let mut detail = json!({
            "version":1,"kind":"request",
            "owner_mxid":target.authority.owner_mxid,
            "publisher_mxid":target.authority.bot_mxid,
            "revision":1,
            "agent":target.authority.agent_name,
            "project":target.authority.project_id,
            "project_room_id":target.authority.project_room_id,
            "request_id":target.request_id,"input_digest":target.request_digest,
            "upstream_request_id":upstream_display,"upstream_rpc_id":request.upstream,"runtime":runtime,
            "tool_name":tool,"description":description,
            "input_preview":preview,"expires_at":owner_expires_at,"actions":actions
        });
        if let Some(scope) = scope {
            detail["reusable_scope"] = scope;
        }
        let expires = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(owner_expires_at) * 1_000_000,
        )
        .map_err(|_| Error::RunnerAuthority)?;
        let body = format!(
            "Approval required for {}\nProject: {}\nRuntime: {}\nTool: {}\nDescription: {}\nInput: {}\nExpires: {}\nChoose an approval button for the scope shown above. Text replies are not approval.",
            target.authority.agent_name,
            target.authority.project_id,
            runtime,
            tool,
            description,
            preview,
            expires
        );
        let content = json!({"msgtype":"com.agentchat.approval.request.v1",
            "body":body,"com.agentchat.approval":detail});
        serde_json::to_writer(&mut EncodedLimit(MAX_CARD), &content)
            .map_err(|_| Error::Capacity)?;
        tx.commit()?;
        Ok(PrivateApprovalCard {
            target,
            owner_expires_at,
            content,
        })
    }

    pub fn check_private_approval_card(
        &mut self,
        card: &PrivateApprovalCard,
        now: u64,
    ) -> Result<(), Error> {
        self.check_private_approval_card_clock(card, || Ok(now))
    }

    pub(crate) fn check_private_approval_card_clock(
        &mut self,
        card: &PrivateApprovalCard,
        sample: impl FnOnce() -> Result<u64, Error>,
    ) -> Result<(), Error> {
        let current = self.private_approval_card_clock(
            &card.target.request_id,
            card.owner_expires_at,
            sample,
        )?;
        if current.target != card.target || current.content != card.content {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
