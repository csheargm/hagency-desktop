//! Synced custody of a management command before a native or remote effect.
//! These records do not authorize execution: each retry needs current scope and
//! the exact original operation permission. Unknown outcomes remain pending.
use super::*;
use serde_json::Value;
#[cfg(test)]
mod graph_tests;

const OLD_INPUT_LIMIT: usize = 16 * 1024;
const PERSONAL_RULES_INPUT_LIMIT:usize=80*1024;
const GRAPH_CREATE_INPUT_LIMIT: usize = 64 * 1024 + 4096;
const GRAPH_UPDATE_INPUT_LIMIT: usize = 128 * 1024 + 4096;
const GRAPH_BIND_INPUT_LIMIT: usize = 32 * 1024 + 4096;
pub(crate) const MAX_INTENT_INPUT: usize = GRAPH_UPDATE_INPUT_LIMIT;
pub(crate) const MAX_INTENT_OUTPUT: usize = 2 * 1024 * 1024;
/// Stored operation is the JSON encoding of the enum. Keep ordinary operation
/// limits unchanged; only these explicit graph variants admit large input.
pub(crate) const INPUT_LIMIT_SQL: &str = "CASE operation WHEN '\"graph_create\"' THEN 69632 WHEN '\"graph_update_node\"' THEN 135168 WHEN '\"graph_bind_recipient\"' THEN 36864 WHEN '\"personal_agent_rules\"' THEN 81920 ELSE 16384 END";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperation {
    WebCatalog,
    EnrollResource,
    EnrollAccountResource,
    ConfigureResource,
    EditLocalResource,
    CreateResourceConfiguration,
    RenameResource,
    TemplateCreate,
    TemplateUpdate,
    TemplateDelete,
    TriageResourceAlert,
    RetireApprovalBinding,
    RevokeToolGrant,
    EditExecutionPolicy,
    ClearWorkspaceDirty,
    ReviewStopped,
    ResolveStopped,
    ReconcileInterruptedCall,
    PrepareAccount,
    RetireAccount,
    LoginAccount,
    TaskCreate,
    TaskEdit,
    TaskDelete,
    TaskAccept,
    TaskTransition,
    TaskComment,
    GraphCreate,
    GraphUpdateNode,
    GraphCancel,
    GraphBindRecipient,
    PublishResource,
    Contribute,
    Reoffer,
    Decide,
    Revoke,
    Verify,
    Resume,
    ProjectRequest,
    ProjectDecide,
    ProjectRevoke,
    PoolAgentRequest,
    PoolAgentDecide,
    PoolAgentStop,
    PoolAgentRenew,
    PoolAgentReallocate,
    AgentRequest,
    AgentDecide,
    AgentCancel,
    AgentRevoke,
    AgentSettlementRequest,
    AgentTopUpRequest,
    AgentTopUpDecide,
    AgentTopUpCancel,
    AgentRunControl,
    AllocationDecide,
    AllocationRelease,
    PublishGrant,
    /// A v3 successor of a published grant (`grant.succeed`). Kept separate
    /// from `PublishGrant` so each keeps its own exact replay identity.
    PublishGrantSuccessor,
    PersonalAgentPolicy,
    PersonalAgentModel,
    PersonalAgentWorkspace,
    PersonalAgentRules,
    PersonalAgentObservation,
}
impl IntentOperation {
    fn permission(self) -> Permission {
        match self {
            Self::WebCatalog => Permission::ManageWebCatalog,
            Self::EnrollResource | Self::EnrollAccountResource => Permission::EnrollResources,
            Self::ConfigureResource
            | Self::EditLocalResource
            | Self::CreateResourceConfiguration => Permission::ConfigureResources,
            Self::RenameResource => Permission::RenameResources,
            Self::TemplateCreate | Self::TemplateUpdate | Self::TemplateDelete => Permission::ManageResourceTemplates,
            Self::TriageResourceAlert => Permission::TriageAlerts,
            Self::RetireApprovalBinding => Permission::RetireApprovalBindings,
            Self::RevokeToolGrant => Permission::RevokeToolGrants,
            Self::EditExecutionPolicy => Permission::EditExecutionPolicy,
            Self::ClearWorkspaceDirty => Permission::ClearWorkspaceDirty,
            Self::ReviewStopped => Permission::InspectStoppedTasks,
            Self::ResolveStopped => Permission::ResolveStoppedTasks,
            Self::ReconcileInterruptedCall => Permission::ReconcileAgentCalls,
            Self::PrepareAccount => Permission::PrepareAccounts,
            Self::RetireAccount => Permission::RetireAccounts,
            Self::LoginAccount => Permission::LoginAccounts,
            Self::TaskCreate => Permission::CreateTasks,
            Self::TaskEdit => Permission::EditTasks,
            Self::TaskDelete => Permission::DeleteTasks,
            Self::TaskAccept => Permission::AcceptTasks,
            Self::TaskTransition => Permission::TransitionTasks,
            Self::TaskComment => Permission::CommentTasks,
            Self::GraphCreate => Permission::CreateOperatorGraphs,
            Self::GraphUpdateNode => Permission::UpdateOperatorGraphNodes,
            Self::GraphCancel => Permission::CancelOperatorGraphs,
            Self::GraphBindRecipient => Permission::BindOperatorGraphRecipients,
            Self::PublishResource => Permission::PublishResources,
            Self::Contribute | Self::Reoffer => Permission::ContributeResources,
            Self::Decide => Permission::DecideInbox,
            Self::Revoke => Permission::RevokeConnections,
            Self::Verify => Permission::VerifyConnections,
            Self::Resume => Permission::ResumeConnections,
            Self::ProjectRequest => Permission::RequestProjects,
            Self::ProjectDecide => Permission::DecideProjects,
            Self::ProjectRevoke => Permission::RevokeProjects,
            Self::PoolAgentRequest => Permission::RequestAgents,
            Self::PoolAgentDecide => Permission::DecideAgentRequests,
            Self::PoolAgentReallocate => Permission::DecideAgentRequests,
            Self::PoolAgentRenew => Permission::DecideAgentRequests,
            Self::PoolAgentStop => Permission::CancelAgentRequests,
            Self::AgentRequest => Permission::RequestAgents,
            Self::AgentDecide => Permission::DecideAgentRequests,
            Self::AgentCancel => Permission::CancelAgentRequests,
            Self::AgentRevoke => Permission::RevokeAgentRequests,
            Self::AgentSettlementRequest => Permission::RequestAgentSettlement,
            Self::AgentTopUpRequest => Permission::RequestAgentTopUps,
            Self::AgentTopUpDecide => Permission::DecideAgentTopUps,
            Self::AgentTopUpCancel => Permission::CancelAgentTopUps,
            Self::AgentRunControl => Permission::ChangeAgentRunControl,
            Self::AllocationDecide => Permission::DecideAllocations,
            Self::AllocationRelease => Permission::ReleaseAllocations,
            Self::PublishGrant | Self::PublishGrantSuccessor => Permission::PublishGrants,
            Self::PersonalAgentPolicy | Self::PersonalAgentRules => Permission::EditPersonalAgentPolicy,
            Self::PersonalAgentModel => Permission::EditPersonalAgentModel,
            Self::PersonalAgentWorkspace => Permission::PreparePersonalAgentWorkspace,
            Self::PersonalAgentObservation => Permission::RequestPersonalAgentSettings,
        }
    }
    /// Publication of an owner-ledger grant receipt, root or successor.
    pub(crate) fn publishes_grant(self) -> bool {
        matches!(self, Self::PublishGrant | Self::PublishGrantSuccessor)
    }
    pub(crate) fn is_graph(self) -> bool {
        matches!(
            self,
            Self::GraphCreate
                | Self::GraphUpdateNode
                | Self::GraphCancel
                | Self::GraphBindRecipient
        )
    }
    fn input_limit(self) -> usize {
        match self {
            Self::GraphCreate => GRAPH_CREATE_INPUT_LIMIT,
            Self::GraphUpdateNode => GRAPH_UPDATE_INPUT_LIMIT,
            Self::GraphBindRecipient => GRAPH_BIND_INPUT_LIMIT,
            Self::PersonalAgentRules => PERSONAL_RULES_INPUT_LIMIT,
            _ => OLD_INPUT_LIMIT,
        }
    }
}

/// Native-prepared, already parsed operation. Deliberately not Deserialize.
#[derive(Clone)]
pub struct Intent {
    operation: IntentOperation,
    command_id: String,
    input: Value,
    digest: String,
    require_unused_publication: bool,
}
impl Intent {
    pub fn operation(&self) -> IntentOperation {
        self.operation
    }
    /// Native-only restriction for an original owner UI intent. Does not alter
    /// its wire bytes/digest or grant any authority to the caller.
    pub fn require_unused_grant_publication(mut self) -> Self {
        self.require_unused_publication = true;
        self
    }
    pub(super) fn permission(&self) -> Permission {
        self.operation.permission()
    }
    pub fn new(
        operation: IntentOperation,
        command_id: String,
        input: &impl Serialize,
    ) -> Result<Self, Error> {
        identifier(&command_id, 128)?;
        let input = serde_json::to_value(input)?;
        if !input.is_object()
            || serde_json::to_vec(&input)?.len() > operation.input_limit()
            || input
                .get("commandId")
                .or_else(|| input.get("command_id"))
                .and_then(Value::as_str)
                != Some(command_id.as_str())
        {
            return Err(hagency_core::InvalidInput("invalid management intent").into());
        }
        if operation.publishes_grant() {
            identifier(
                input["receipt"]["grantId"]
                    .as_str()
                    .ok_or(Error::Conflict)?,
                128,
            )?;
        }
        let envelope = serde_json::json!({"operation":operation,"input":input});
        let digest = if operation.is_graph() {
            // Graph results/conditions are opaque JSON and may contain
            // fractions. The existing canonical financial-command digest is
            // integer-only and must remain unchanged for every old operation.
            let mut bytes = b"hagency.operator-graph-intent.v1\0".to_vec();
            bytes.extend(serde_json::to_vec(&envelope)?);
            hagency_core::project::hash(&bytes)
        } else {
            hagency_core::canonical::digest(&envelope)?
        };
        Ok(Self {
            operation,
            command_id,
            input,
            digest,
            require_unused_publication: false,
        })
    }
    pub(crate) fn matches_digest(&self, digest: &str) -> bool {
        self.digest == digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IntentReply {
    Success(Value),
    Failure { status: u16, code: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingIntent {
    pub command_id: String,
    pub operation: IntentOperation,
    pub input: Value,
    pub reply: Option<IntentReply>,
    #[serde(skip)]
    newly_prepared: bool,
}
impl PendingIntent {
    /// True only for the transaction that first persisted this command. A later
    /// attempt cannot assume a new validation failure disproves an old effect.
    pub fn is_first_attempt(&self) -> bool {
        self.newly_prepared
    }
}

pub(crate) enum JournalAction {
    Prepare(Intent),
    Reply(Intent, IntentReply),
    Read,
    /// One saved command by its exact ID, including kept publication history.
    Lookup(String),
    RequireUnusedGrantPublication(String),
    Acknowledge(String),
}
pub(crate) struct JournalCommand {
    pub(super) authority: ManagementAuthority,
    context: HostContext,
    action: JournalAction,
    pub(super) deadline: Instant,
}
impl JournalCommand {
    pub(super) fn new(
        authority: ManagementAuthority,
        context: HostContext,
        action: JournalAction,
    ) -> Self {
        Self {
            authority,
            context,
            action,
            deadline: Instant::now() + Duration::from_secs(2),
        }
    }
    pub(crate) fn weight(&self) -> u32 {
        match &self.action {
            JournalAction::Prepare(intent) | JournalAction::Reply(intent, _) => {
                (intent.operation.input_limit() + 64 * 1024 + 4096) as u32
            }
            // Eight pending records, each with its own input and bounded reply.
            JournalAction::Read => (8 * (MAX_INTENT_INPUT + 64 * 1024 + 4096)) as u32,
            JournalAction::Lookup(_) => (MAX_INTENT_INPUT + 64 * 1024 + 4096) as u32,
            _ => 84 * 1024,
        }
    }
    fn check(&self, permission: Permission) -> Result<(), Error> {
        self.authority.authorize(&self.context, permission)?;
        if Instant::now() >= self.deadline {
            return Err(Error::OutcomeUnknown);
        }
        Ok(())
    }
}
impl Session {
    async fn journal(
        &self,
        context: &HostContext,
        action: JournalAction,
        permission: Permission,
        mutation: bool,
    ) -> Result<Vec<PendingIntent>, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, permission)?;
        let command = JournalCommand::new(self.authority(), context.clone(), action);
        let result = if let Some(journal) = &self.server_journal {
            journal.command(command).await?
        } else {
            match self.local_store()? {

                SessionStore::Current(store) => store.journal(command).await?,
            }
        };
        self.check(context, permission).map_err(|error| {
            if mutation {
                Error::OutcomeUnknown
            } else {
                error
            }
        })?;
        Ok(result)
    }
    pub async fn prepare_intent(
        &self,
        context: &HostContext,
        intent: Intent,
    ) -> Result<PendingIntent, Error> {
        let permission = intent.operation.permission();
        self.journal(context, JournalAction::Prepare(intent), permission, true)
            .await?
            .pop()
            .ok_or(Error::Schema)
    }
    /// Only a known reply belongs here. A timeout or post-write scope loss must
    /// leave custody pending, so recovery retries the original command identity.
    pub async fn record_intent_reply(
        &self,
        context: &HostContext,
        intent: Intent,
        reply: IntentReply,
    ) -> Result<(), Error> {
        if serde_json::to_vec(&reply)?.len() > 64 * 1024 {
            return Err(Error::Capacity);
        }
        let permission = intent.operation.permission();
        self.journal(
            context,
            JournalAction::Reply(intent, reply),
            permission,
            true,
        )
        .await?;
        Ok(())
    }
    /// Outstanding saved commands: every unacknowledged intent except replied
    /// grant publications, which are kept history (see `publication_history`).
    pub async fn pending_intents(
        &self,
        context: &HostContext,
    ) -> Result<Vec<PendingIntent>, Error> {
        self.journal(context, JournalAction::Read, Permission::ReadIntents, false)
            .await
    }
    /// The saved command with this exact ID, if this actor still journals it
    /// and the caller holds its operation's permission. Unlike
    /// `pending_intents` it also finds a replied grant publication, so a
    /// publication is observed, recovered or replayed by its own command ID.
    /// An acknowledged (scrubbed) command is never returned.
    pub async fn intent(
        &self,
        context: &HostContext,
        command_id: String,
    ) -> Result<Option<PendingIntent>, Error> {
        identifier(&command_id, 128)?;
        Ok(self
            .journal(
                context,
                JournalAction::Lookup(command_id),
                Permission::ReadIntents,
                false,
            )
            .await?
            .pop())
    }
    /// Descriptive absence proof only. Scrubbed/acknowledged prior grant
    /// custody cannot establish which grant it belonged to, so fails closed.
    pub async fn require_unused_grant_publication(
        &self,
        context: &HostContext,
        grant_id: String,
    ) -> Result<(), Error> {
        identifier(&grant_id, 128)?;
        self.journal(
            context,
            JournalAction::RequireUnusedGrantPublication(grant_id),
            Permission::ReadIntents,
            false,
        )
        .await?;
        Ok(())
    }
    pub async fn acknowledge_intent(
        &self,
        context: &HostContext,
        command_id: String,
    ) -> Result<(), Error> {
        identifier(&command_id, 128)?;
        self.journal(
            context,
            JournalAction::Acknowledge(command_id),
            Permission::AcknowledgeIntents,
            true,
        )
        .await?;
        Ok(())
    }
}

/// The two grant publication operations. Their replied rows are permanent
/// publication history, not outstanding work: they can never be acknowledged
/// (an acknowledged or scrubbed publication makes every later publication
/// fail closed in `require_unused_publication`), so they must not consume the
/// eight-intent outstanding bound or the generic pending read. A pending
/// (unanswered) publication still counts and is still listed.
fn publication_history() -> Result<(String, String), Error> {
    Ok((
        serde_json::to_string(&IntentOperation::PublishGrant)?,
        serde_json::to_string(&IntentOperation::PublishGrantSuccessor)?,
    ))
}
type Stored = (String, String, Option<String>, Option<String>, String);
fn stored(db: &Connection, actor: &str, id: &str) -> Result<Option<Stored>, Error> {
    Ok(db.query_row(&format!("SELECT operation,digest,CASE WHEN length(CAST(input AS BLOB))<={INPUT_LIMIT_SQL} THEN input END,CASE WHEN length(CAST(reply AS BLOB))<=65536 THEN reply END,state FROM management_intents WHERE actor_scope=?1 AND command_id=?2"),
        params![actor,id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?)
}
fn pending(id: String, row: &Stored) -> Result<PendingIntent, Error> {
    if row.4 == "replied" && row.3.is_none() {
        return Err(Error::Quarantined);
    }
    let operation = serde_json::from_str(&row.0)?;
    let input = serde_json::from_str(row.2.as_deref().ok_or(Error::Conflict)?)?;
    let original = Intent::new(operation, id.clone(), &input)?;
    if !original.matches_digest(&row.1) {
        return Err(Error::Quarantined);
    }
    Ok(PendingIntent {
        command_id: id,
        operation,
        input,
        reply: row.3.as_deref().map(serde_json::from_str).transpose()?,
        newly_prepared: false,
    })
}
fn require_unused_publication(db: &Connection, actor: &str, grant_id: &str) -> Result<(), Error> {
    let operation = serde_json::to_string(&IntentOperation::PublishGrant)?;
    let successor = serde_json::to_string(&IntentOperation::PublishGrantSuccessor)?;
    let scrubbed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM management_intents WHERE actor_scope=?1 AND operation IN(?2,?3) AND (state='acknowledged' OR input IS NULL))",
        params![actor, operation, successor], |r| r.get(0))?;
    if scrubbed {
        return Err(Error::Conflict);
    }
    // Successor publications are bounded by the 2048-grant ledger.
    let mut query = db.prepare(
        "SELECT input FROM management_intents WHERE actor_scope=?1 AND operation=?2 LIMIT 2049",
    )?;
    let inputs = query.query_map(params![actor, successor], |r| r.get::<_, String>(0))?;
    for (index, old) in inputs.enumerate() {
        if index >= 2048 {
            return Err(Error::Capacity);
        }
        let old: Value = serde_json::from_str(&old?)?;
        if old["receipt"]["grantId"].as_str().ok_or(Error::Conflict)? == grant_id {
            return Err(Error::Conflict);
        }
    }
    // Root publications are bounded by the same 2048-grant ledger. Every row
    // is scanned: a grant ID is never published twice.
    let mut query = db.prepare(
        "SELECT input FROM management_intents WHERE actor_scope=?1 AND operation=?2 LIMIT 2049",
    )?;
    let inputs = query.query_map(params![actor, operation], |r| r.get::<_, String>(0))?;
    for (index, old) in inputs.enumerate() {
        if index >= 2048 {
            return Err(Error::Capacity);
        }
        let old: Value = serde_json::from_str(&old?)?;
        let Some(id) = old["receipt"]["grantId"].as_str() else {
            return Err(Error::Conflict);
        };
        if id == grant_id {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
pub(crate) fn execute_intent(
    db: &mut Connection,
    command: JournalCommand,
) -> Result<Vec<PendingIntent>, Error> {
    let actor = CommandReceipt::new(&command.context, "intent", "scope", &())?.actor;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut rows = Vec::new();
    let permission = match &command.action {
        JournalAction::Prepare(intent) | JournalAction::Reply(intent, _) => {
            intent.operation.permission()
        }
        JournalAction::Read
        | JournalAction::Lookup(_)
        | JournalAction::RequireUnusedGrantPublication(_) => Permission::ReadIntents,
        JournalAction::Acknowledge(_) => Permission::AcknowledgeIntents,
    };
    command.check(permission)?;
    match &command.action {
        JournalAction::Prepare(intent) => {
            if let Some(mut row) = stored(&tx, &actor, &intent.command_id)? {
                if row.1 != intent.digest || row.4 == "acknowledged" {
                    return Err(Error::Conflict);
                }
                // A native effect and its original receipt commit atomically.
                // Recover that receipt before requiring the provider again. The
                // journal was written first and fixes these original arguments;
                // a newly prepared ID may not alias an older receipt below.
                if row.4 == "pending" {
                    let original_operation = match intent.operation {
                        IntentOperation::EnrollResource => Some("resource.enroll"),
                        IntentOperation::ConfigureResource => Some("resource.configure"),
                        IntentOperation::PublishResource => Some("resource.publish"),
                        _ => None,
                    };
                    if let Some(expected) = original_operation {
                        let receipt: Option<(String,String)> = tx.query_row(
                            "SELECT operation,result FROM management_commands WHERE actor_scope=?1 AND command_id=?2",
                            params![actor,intent.command_id], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                        if let Some((operation, result)) = receipt {
                            if operation != expected {
                                return Err(Error::Conflict);
                            }
                            let reply = serde_json::to_string(&IntentReply::Success(
                                serde_json::from_str(&result)?,
                            ))?;
                            tx.execute("UPDATE management_intents SET state='replied',reply=?3 WHERE actor_scope=?1 AND command_id=?2",params![actor,intent.command_id,reply])?;
                            row.3 = Some(reply);
                            row.4 = "replied".into();
                        }
                    }
                }
                rows.push(pending(intent.command_id.clone(), &row)?);
            } else {
                let prior: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM management_commands WHERE actor_scope=?1 AND command_id=?2)",params![actor,intent.command_id],|r|r.get(0))?;
                if prior {
                    return Err(Error::Conflict);
                }
                // Outstanding work only: kept publication history is excluded.
                let (root, successor) = publication_history()?;
                let count: i64 = tx.query_row("SELECT COUNT(*) FROM management_intents WHERE actor_scope=?1 AND state<>'acknowledged' AND NOT (state='replied' AND operation IN (?2,?3))", params![actor, root, successor], |r|r.get(0))?;
                let total: i64 =
                    tx.query_row("SELECT COUNT(*) FROM management_intents", [], |r| r.get(0))?;
                if count >= 8 || total >= 100_000 {
                    return Err(Error::Capacity);
                }
                if intent.operation.publishes_grant() {
                    if intent.require_unused_publication {
                        require_unused_publication(
                            &tx,
                            &actor,
                            intent.input["receipt"]["grantId"]
                                .as_str()
                                .ok_or(Error::Conflict)?,
                        )?;
                    } else {
                        // Existing explicit native correction after an acknowledged
                        // definite failure remains supported. Pending uncertainty
                        // always requires its original command.
                        let mut query = tx.prepare("SELECT input FROM management_intents WHERE actor_scope=?1 AND operation=?2 AND state='pending'")?;
                        let inputs = query.query_map(
                            params![
                                actor,
                                serde_json::to_string(&intent.operation)?
                            ],
                            |r| r.get::<_, String>(0),
                        )?;
                        for old in inputs {
                            let old: Value = serde_json::from_str(&old?)?;
                            if old["receipt"]["grantId"] == intent.input["receipt"]["grantId"] {
                                return Err(Error::Conflict);
                            }
                        }
                    }
                }
                tx.execute("INSERT INTO management_intents(actor_scope,command_id,operation,digest,input,state) VALUES(?1,?2,?3,?4,?5,'pending')",
                        params![actor,intent.command_id,serde_json::to_string(&intent.operation)?,intent.digest,serde_json::to_string(&intent.input)?])?;
                rows.push(PendingIntent {
                    command_id: intent.command_id.clone(),
                    operation: intent.operation,
                    input: intent.input.clone(),
                    reply: None,
                    newly_prepared: true,
                });
            }
        }
        JournalAction::Reply(intent, reply) => {
            let row = stored(&tx, &actor, &intent.command_id)?.ok_or(Error::Conflict)?;
            if row.1 != intent.digest || row.4 == "acknowledged" {
                return Err(Error::Conflict);
            }
            let encoded = serde_json::to_string(reply)?;
            if row.3.as_ref().is_some_and(|old| *old != encoded) {
                return Err(Error::Conflict);
            }
            tx.execute("UPDATE management_intents SET state='replied',reply=?3 WHERE actor_scope=?1 AND command_id=?2", params![actor,intent.command_id,encoded])?;
        }
        JournalAction::Read => {
            let (root, successor) = publication_history()?;
            let mut stmt = tx.prepare(&format!("SELECT command_id,operation,digest,CASE WHEN length(CAST(input AS BLOB))<={INPUT_LIMIT_SQL} THEN input END,CASE WHEN length(CAST(reply AS BLOB))<=65536 THEN reply END,state FROM management_intents WHERE actor_scope=?1 AND state<>'acknowledged' AND NOT (state='replied' AND operation IN (?2,?3)) ORDER BY command_id LIMIT 9"))?;
            let found = stmt.query_map(params![actor, root, successor], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?),
                ))
            })?;
            let mut observed = 0;
            for row in found {
                observed += 1;
                if observed > 8 {
                    return Err(Error::Capacity);
                }
                let (id, row) = row?;
                let value = pending(id, &row)?;
                if command
                    .authority
                    .authorize(&command.context, value.operation.permission())
                    .is_ok()
                {
                    rows.push(value);
                }
            }
            if rows.len() > 8 {
                return Err(Error::Capacity);
            }
        }
        JournalAction::Lookup(id) => {
            // The same visibility as the pending read, for one exact ID: never
            // an acknowledged command, never one outside the caller's grants.
            if let Some(row) = stored(&tx, &actor, id)?.filter(|row| row.4 != "acknowledged") {
                let value = pending(id.clone(), &row)?;
                if command
                    .authority
                    .authorize(&command.context, value.operation.permission())
                    .is_ok()
                {
                    rows.push(value);
                }
            }
        }
        JournalAction::RequireUnusedGrantPublication(grant_id) => {
            command.check(Permission::PublishGrants)?;
            require_unused_publication(&tx, &actor, grant_id)?;
        }
        JournalAction::Acknowledge(id) => {
            let row = stored(&tx, &actor, id)?.ok_or(Error::Conflict)?;
            command.check(serde_json::from_str::<IntentOperation>(&row.0)?.permission())?;
            if row.4 == "pending" {
                return Err(Error::Conflict);
            }
            tx.execute("UPDATE management_intents SET state='acknowledged',input=NULL,reply=NULL WHERE actor_scope=?1 AND command_id=?2",params![actor,id])?;
        }
    }
    command.check(permission)?;
    tx.commit()?;
    Ok(rows)
}
