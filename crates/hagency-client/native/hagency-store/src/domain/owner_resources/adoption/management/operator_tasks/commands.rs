//! Atomic local operator edits under the retained proven-empty task binding.
use super::*;
use crate::management::operator_task_commands::{clean, required, task_labels};
use crate::management::{
    CommandReceipt, OperatorTaskChange, OperatorTaskCommand, OperatorTaskResult, TaskField,
};

fn parent(db: &Connection, profile: &str, id: Option<&str>) -> Result<(), Error> {
    if let Some(id) = id {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_operator_tasks WHERE profile=?1 AND id=?2)",
            params![profile, id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(hagency_core::InvalidInput("parent task not found").into());
        }
    }
    Ok(())
}
fn optional(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|s| clean(s, max)).filter(|s| !s.is_empty())
}
fn read(db: &Connection, profile: &str, id: &str) -> Result<(OperatorTask, u64, usize), Error> {
    let task = db
        .query_row(
            &format!("{SELECT} WHERE profile=?1 AND id=?2"),
            params![profile, id],
            task_row,
        )
        .optional()?
        .ok_or(Error::NotFound)??;
    let revision = db.query_row(
        "SELECT revision FROM owner_operator_tasks WHERE profile=?1 AND id=?2",
        params![profile, id],
        |r| r.get(0),
    )?;
    let count = db.query_row(
        "SELECT COUNT(*) FROM owner_operator_task_comments WHERE profile=?1 AND task_id=?2",
        params![profile, id],
        |r| r.get(0),
    )?;
    Ok((task, revision, count))
}
fn update_text(
    db: &Connection,
    profile: &str,
    id: &str,
    column: &str,
    value: Option<&str>,
) -> Result<(), Error> {
    // Column names are fixed call-site constants, never supplied by the caller.
    db.execute(
        &format!("UPDATE owner_operator_tasks SET {column}=?3 WHERE profile=?1 AND id=?2"),
        params![profile, id, value],
    )?;
    Ok(())
}
impl CurrentManagementStore {
    pub(crate) async fn operator_task_write_access(
        &self,
        profile: &ProfileAuthority,
    ) -> Result<bool, Error> {
        let authority = self.task_authority(profile)?;
        let after = authority.clone();
        let backing = self.clone();
        let result = self
            .registry
            .0
            .store
            .call(256, move |db| {
                let tx = db.db.unchecked_transaction()?;
                let supported = backing.task_read_proof(&tx, &authority)?;
                authority.check()?;
                tx.commit()?;
                Ok(supported)
            })
            .await?;
        after.check()?;
        self.check(profile)?;
        Ok(result)
    }

    pub(crate) async fn apply_operator_task(
        &self,
        profile: &ProfileAuthority,
        input: OperatorTaskCommand,
        author: String,
        receipt: CommandReceipt,
    ) -> Result<OperatorTaskResult, Error> {
        input.validate()?;
        let authority = self.task_authority(profile)?;
        if author != authority.profile.account || author.chars().count() > 128 {
            return Err(Error::LocalAuthority);
        }
        let backing = self.clone();
        let after = authority.clone();
        let out=self.registry.0.store.call(16384,move|db|{
            authority.check()?;
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !backing.task_read_proof(&tx,&authority)? {return Err(hagency_core::InvalidInput("canonical task history required").into())}
            // Exact historical result precedes current CAS/existence checks.
            if let Some(value)=receipt.lookup::<OperatorTaskResult>(&tx)? {authority.check()?;return Ok(value)}
            let profile=&authority.profile.digest;let id=&input.task_id;
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|Error::LocalAuthority)?.as_millis();
            let now=u64::try_from(now).ok().filter(|v|*v<=9_007_199_254_740_991).ok_or(Error::Capacity)?;
            let mut deleted=false;
            let (mut task,mut revision,mut comments)=if let OperatorTaskChange::Create(v)=&input.change {
                let count:i64=tx.query_row("SELECT COUNT(*) FROM owner_operator_tasks WHERE profile=?1",[profile],|r|r.get(0))?;
                if count>=100_000{return Err(Error::Capacity)}
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_operator_tasks WHERE profile=?1 AND id=?2)",params![profile,id],|r|r.get(0))?;
                if exists{return Err(Error::Conflict)}
                let parent_id=optional(v.parent_id.as_deref(),64);parent(&tx,profile,parent_id.as_deref())?;
                tx.execute("INSERT INTO owner_operator_tasks(profile,id,revision,title,description,status,priority,granularity,assignee,created_by,created_at,updated_at,parent_id,labels) VALUES(?1,?2,1,?3,?4,'created',?5,?6,?7,?8,?9,?9,?10,?11)",params![profile,id,required(&v.title,255)?,clean(v.description.as_deref().unwrap_or_default(),4096),optional(v.priority.as_deref(),8).unwrap_or_else(||"p2".into()),optional(v.granularity.as_deref(),16).unwrap_or_else(||"task".into()),optional(v.assignee.as_deref(),128),author,now,parent_id,serde_json::to_string(&task_labels(&v.labels))?])?;
                read(&tx,profile,id)?
            } else {
                let current=read(&tx,profile,id)?;
                if Some(current.1)!=input.change.expected_revision(){return Err(Error::OperatorTaskRevisionChanged)}
                current
            };
            let mut changed=false;
            match &input.change {
                OperatorTaskChange::Create(_)=>{},
                OperatorTaskChange::Edit(v)=>{
                    if let TaskField::Set(value)=&v.title && let Some(value)=optional(value.as_deref(),255){update_text(&tx,profile,id,"title",Some(&value))?;changed=true;}
                    if let TaskField::Set(value)=&v.description {update_text(&tx,profile,id,"description",Some(&clean(value.as_deref().unwrap_or_default(),4096)))?;changed=true;}
                    for (field,column,max) in [(&v.priority,"priority",8),(&v.granularity,"granularity",16),(&v.assignee,"assignee",128),(&v.parent_id,"parent_id",64)] {
                        if let TaskField::Set(value)=field {let value=optional(value.as_deref(),max);if column=="parent_id"{parent(&tx,profile,value.as_deref())?;} update_text(&tx,profile,id,column,value.as_deref())?;changed=true;}
                    }
                    if let TaskField::Set(labels)=&v.labels {update_text(&tx,profile,id,"labels",Some(&serde_json::to_string(&task_labels(labels.as_deref().unwrap_or_default()))?))?;changed=true;}
                },
                OperatorTaskChange::Accept(_)|OperatorTaskChange::Transition(_)=>{
                    let (status,reason,until)=match &input.change {OperatorTaskChange::Transition(v)=>(v.status.trim(),v.waiting_reason.as_deref(),v.waiting_until.as_deref()),_=>("accepted",None,None)};
                    if !crate::operator_transitions(&task.status).contains(&status){return Err(hagency_core::InvalidInput("invalid task transition").into())}
                    let (reason,until)=if status=="blocked" {(Some(required(reason.unwrap_or_default(),1024)?),Some(required(until.unwrap_or_default(),64)?))}else{(None,None)};
                    tx.execute("UPDATE owner_operator_tasks SET status=?3,started_at=CASE WHEN ?3 IN ('accepted','in_progress') THEN COALESCE(started_at,?4) ELSE started_at END,completed_at=CASE WHEN ?3='done' THEN ?4 ELSE completed_at END,waiting_reason=?5,waiting_until=?6 WHERE profile=?1 AND id=?2",params![profile,id,status,now,reason,until])?;changed=true;
                },
                OperatorTaskChange::Comment(v)=>{
                    if comments>=crate::MAX_TASK_COMMENTS{return Err(Error::Capacity)}
                    tx.execute("INSERT INTO owner_operator_task_comments(profile,task_id,ordinal,author,text,at_ms) VALUES(?1,?2,?3,?4,?5,?6)",params![profile,id,comments,author,required(&v.text,4096)?,now])?;changed=true;
                },
                OperatorTaskChange::Delete(_)=>{
                    tx.execute("DELETE FROM owner_operator_tasks WHERE profile=?1 AND id=?2",params![profile,id])?;
                    // Original delete leaves child parent references untouched.
                    deleted=true;revision+=1;
                }
            }
            if changed {revision+=1;tx.execute("UPDATE owner_operator_tasks SET revision=?3,updated_at=?4 WHERE profile=?1 AND id=?2",params![profile,id,revision,now])?;}
            if !deleted {(task,revision,comments)=read(&tx,profile,id)?;}
            let result=OperatorTaskResult{command_id:input.command_id,task_id:input.task_id,revision,deleted,status:task.status,updated_at:task.updated_at,comment_count:comments};
            receipt.record(&tx,&result)?;
            authority.check()?;tx.commit()?;Ok(result)
        }).await?;
        after.check().map_err(|_| Error::OutcomeUnknown)?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(out)
    }
}
