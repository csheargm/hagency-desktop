//! Compare the reviewed policy and save its receipt in the same transaction.
use super::*;
use crate::management::{
    AgentExecutionPolicy, AgentExecutionPolicyPage, CommandReceipt, EditExecutionPolicy,
    ExecutionPolicyListQuery, ExecutionPolicyQuery, ExecutionPolicyValue, PolicyApplication,
};
use rusqlite::Connection;

fn snapshot(db: &Connection, query: &ExecutionPolicyQuery) -> Result<AgentExecutionPolicy, Error> {
    let config: Option<String> = db.query_row(
        "SELECT r.config FROM engagements e JOIN resources r ON r.id=e.resource_id WHERE e.id=?1 AND e.resource_id=?2",
        params![query.engagement_id, query.resource_id], |r| r.get(0),
    ).optional()?;
    let Some(config) = config else { return shared_snapshot(db, query); };
    let resource: hagency_core::project::Resource = serde_json::from_str(&config)?;
    let agent = super::super::read_engagement(db, &query.engagement_id)?;
    let saved: Option<(bool, u64)> = db
        .query_row(
            "SELECT yolo,updated_at FROM agent_execution_policies WHERE engagement_id=?1",
            [&query.engagement_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let policy = normalize_policy(
        Some(&json!({"yolo":saved.map(|p|p.0).unwrap_or(false)})),
        &resource.framework,
    )?;
    let mut result = AgentExecutionPolicy {
        web_search: None,
        shared_mode: None,
        resource_id: query.resource_id.clone(),
        engagement_id: query.engagement_id.clone(),
        agent: agent.agent_name.as_str().to_owned(),
        project_id: agent.project_id,
        project_name: agent.project_name,
        framework: resource.framework,
        execution_policy: ExecutionPolicyValue { yolo: policy.yolo },
        applies_to: PolicyApplication::NextDispatch,
        revision: String::new(),
    };
    result.revision =
        hagency_core::canonical::digest(&json!({"policy":result,"updatedAt":saved.map(|p|p.1)}))?;
    if serde_json::to_vec(&result)?.len() > 8192 {
        return Err(Error::Capacity);
    }
    Ok(result)
}

impl DomainRepository {
    pub(crate) fn management_execution_policies(
        &self,
        query: &ExecutionPolicyListQuery,
        check: impl Fn(&Connection) -> Result<(), Error>,
    ) -> Result<AgentExecutionPolicyPage, Error> {
        query.validate()?;
        let tx = self.db.unchecked_transaction()?;
        check(&tx)?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1)",
            [&query.resource_id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::NotFound);
        }
        let mut statement = tx.prepare(
            "SELECT id FROM engagements WHERE resource_id=?1 AND id>?2 UNION SELECT agent_id AS id FROM owner_managed_agents WHERE json_extract(original_command,'$.resourceId')=?1 AND agent_id>?2 AND state='reserved' AND permanent_fence=0 ORDER BY id LIMIT 17",
        )?;
        let ids = statement
            .query_map(params![query.resource_id, query.after], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let more = ids.len() > 16;
        let agents = ids
            .into_iter()
            .take(16)
            .map(|engagement_id| {
                snapshot(
                    &tx,
                    &ExecutionPolicyQuery {
                        resource_id: query.resource_id.clone(),
                        engagement_id,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_after = if more {
            agents.last().map(|a| a.engagement_id.clone())
        } else {
            None
        };
        let result = AgentExecutionPolicyPage {
            resource_id: query.resource_id.clone(),
            agents,
            next_after,
        };
        if serde_json::to_vec(&result)?.len() > 128 * 1024 {
            return Err(Error::Capacity);
        }
        check(&tx)?;
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn management_execution_policy(
        &self,
        query: &ExecutionPolicyQuery,
        check: impl Fn(&Connection) -> Result<(), Error>,
    ) -> Result<AgentExecutionPolicy, Error> {
        query.validate()?;
        let tx = self.db.unchecked_transaction()?;
        check(&tx)?;
        let result = snapshot(&tx, query)?;
        check(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) fn management_edit_execution_policy(
        &mut self,
        input: &EditExecutionPolicy,
        receipt: CommandReceipt,
        check: impl Fn(&Connection) -> Result<(), Error>,
    ) -> Result<AgentExecutionPolicy, Error> {
        input.validate()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check(&tx)?;
        if let Some(original) = receipt.lookup::<AgentExecutionPolicy>(&tx)? {
            check(&tx)?;
            return Ok(original);
        }
        let query = ExecutionPolicyQuery {
            resource_id: input.resource_id.clone(),
            engagement_id: input.engagement_id.clone(),
        };
        let current = snapshot(&tx, &query)?;
        if current.revision != input.expected_revision {
            return Err(Error::Conflict);
        }
        let policy = normalize_policy(
            Some(&serde_json::to_value(input.execution_policy)?),
            &current.framework,
        )?;
        if let Some(enabled) = input.web_search {
            if current.shared_mode.is_none() || input.execution_policy != current.execution_policy {
                return Err(Error::LocalAuthority);
            }
            let (child, profile): (String, String) = tx.query_row(
                "SELECT id,profile FROM owner_managed_agents WHERE agent_id=?1 AND json_extract(original_command,'$.resourceId')=?2 AND state='reserved' AND permanent_fence=0",
                params![input.engagement_id, input.resource_id], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            super::shared_web::write(&tx, &child, &profile, &input.engagement_id, enabled)?;
        } else if current.shared_mode.is_some() {
            tx.execute("INSERT INTO owner_shared_tool_policies(child_id,yolo,revision) SELECT id,?3,1 FROM owner_managed_agents WHERE agent_id=?1 AND json_extract(original_command,'$.resourceId')=?2 AND state='reserved' AND permanent_fence=0 ON CONFLICT(child_id) DO UPDATE SET yolo=excluded.yolo,revision=revision+1",params![input.engagement_id,input.resource_id,policy.yolo])?;
        } else {
            write_policy(&tx, &input.engagement_id, policy.yolo, crate::domain::graphs::now_ms()?)?;
        }
        let result = snapshot(&tx, &query)?;
        receipt.record(&tx, &result)?;
        check(&tx)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}

fn shared_snapshot(db: &Connection, query: &ExecutionPolicyQuery) -> Result<AgentExecutionPolicy, Error> {
    let (child, profile): (String, String) = db.query_row(
        "SELECT id,profile FROM owner_managed_agents WHERE agent_id=?1 AND json_extract(original_command,'$.resourceId')=?2 AND state='reserved' AND permanent_fence=0",
        params![query.engagement_id, query.resource_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.ok_or(Error::NotFound)?;
    let web = super::shared_web::read(db, &child, &profile, &query.engagement_id)?;
    let (raw,saved): (String,Option<String>) = db.query_row("SELECT a.original_command,CASE WHEN p.child_id IS NULL THEN NULL ELSE json_object('yolo',p.yolo,'revision',p.revision) END FROM owner_managed_agents a LEFT JOIN owner_shared_tool_policies p ON p.child_id=a.id WHERE a.agent_id=?1 AND json_extract(a.original_command,'$.resourceId')=?2 AND a.state='reserved' AND a.permanent_fence=0",params![query.engagement_id,query.resource_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Error::NotFound)?;
    let command: Value=serde_json::from_str(&raw)?;
    let saved: Option<Value>=saved.as_deref().map(serde_json::from_str).transpose()?;
    let yolo=saved.as_ref().is_some_and(|s|s["yolo"]==1);
    let mut result=AgentExecutionPolicy {
        web_search: Some(web.enabled),
        shared_mode:Some(if saved.is_none(){"chat"}else if yolo{"yolo"}else{"standard"}.into()),
        resource_id:query.resource_id.clone(),engagement_id:query.engagement_id.clone(),
        agent:command["definition"]["name"].as_str().ok_or(Error::LocalAuthority)?.into(),
        // The pool's own coding tool, so the page words its options for that tool.
        project_id:String::new(),project_name:None,framework:super::super::read_resource(db,&query.resource_id)?.framework,
        execution_policy:ExecutionPolicyValue{yolo},applies_to:PolicyApplication::NextDispatch,revision:String::new(),
    };
    result.revision=hagency_core::canonical::digest(&json!({"policy":result,"saved":saved,"webRevision":web.revision}))?;
    Ok(result)
}
