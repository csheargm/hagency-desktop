//! Current canonical operator tasks bound to the current profile initializer.
use super::*;
mod commands;
use crate::management::{OperatorBoardPage,OperatorBoardQuery,OperatorTaskDetail,OperatorTaskDetailQuery,OperatorTaskPage,OperatorTaskQuery,TASK_COMMENT_PAGE,TASK_READ_PAGE};
use crate::{OperatorTask,OperatorTaskComment,TASK_STATUSES};
use std::collections::BTreeMap;
#[derive(Clone)]
pub(super) struct TaskBinding(pub(super) Arc<current::CurrentPlan>);
impl CurrentManagementStore {
    fn task_authority(&self, profile: &ProfileAuthority) -> Result<ProfileInitializationAuthority, Error> {
        self.check(profile)?;
        let retained = self.clone();
        ProfileInitializationAuthority::new(
            self.registry.clone(),
            profile.clone(),
            Instant::now() + Duration::from_secs(30),
            move || retained.is_live(),
        )
    }
    fn task_read_proof(
        &self,
        db: &Connection,
        authority: &ProfileInitializationAuthority,
    ) -> Result<bool, Error> {
        let Some(binding) = &self.task_history else {
            return Ok(false);
        };
        current::task_history(db,authority,&binding.0)
    }

    pub(crate) async fn operator_task_page(
        &self,
        profile: &ProfileAuthority,
        query: OperatorTaskQuery,
        observed: u64,
    ) -> Result<Option<OperatorTaskPage>, Error> {
        query.validate()?;
        let authority = self.task_authority(profile)?;
        let backing = self.clone();
        let after = authority.clone();
        let value=self.registry.0.store.call(1024,move|db|{
            let tx=db.db.unchecked_transaction()?;
            if !backing.task_read_proof(&tx,&authority)? { return Ok(None) }
            let statuses=query.status.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|s|!s.is_empty()).collect::<Vec<_>>();
            let status_json=serde_json::to_string(&statuses)?;
            let mut statement=tx.prepare(&format!("{SELECT} WHERE profile=?1 AND (?2 IS NULL OR ?2='' OR assignee=?2) AND (?3='[]' OR status IN (SELECT value FROM json_each(?3))) AND (?4 IS NULL OR ?4='' OR priority=?4) AND (?5 IS NULL OR ?5='' OR EXISTS(SELECT 1 FROM json_each(labels) WHERE value=?5)) ORDER BY created_at DESC,id DESC LIMIT 17 OFFSET ?6"))?;
            let tasks=statement.query_map(params![authority.profile.digest,query.assignee,status_json,query.priority,query.label,query.offset],task_row)?.collect::<Result<Vec<_>,_>>()?;
            drop(statement);
            let more=tasks.len()>TASK_READ_PAGE;
            let tasks=tasks.into_iter().take(TASK_READ_PAGE).map(|task|task.map(Into::into)).collect::<Result<Vec<_>,_>>()?;
            let value=OperatorTaskPage{write_supported:true,write_operations:vec![],supported:true,reason:None,next_offset:more.then_some(query.offset+tasks.len() as u64),tasks,observed_at_ms:observed,unavailable:vec!["health"]};
            authority.check()?;tx.commit()?;Ok(Some(value))
        }).await?;
        after.check()?;
        self.check(profile)?;
        Ok(value)
    }
    pub(crate) async fn operator_task_detail(
        &self,
        profile: &ProfileAuthority,
        query: OperatorTaskDetailQuery,
        observed: u64,
    ) -> Result<Option<OperatorTaskDetail>, Error> {
        query.validate()?;
        let authority = self.task_authority(profile)?;
        let backing = self.clone();
        let after = authority.clone();
        let value=self.registry.0.store.call(512,move|db|{
            let tx=db.db.unchecked_transaction()?;
            if !backing.task_read_proof(&tx,&authority)? { return Ok(None) }
            let mut task=tx.query_row(&format!("{SELECT} WHERE profile=?1 AND id=?2"),params![authority.profile.digest,query.id],task_row).optional()?.ok_or(Error::NotFound)??;
            let comment_count=tx.query_row("SELECT COUNT(*) FROM owner_operator_task_comments WHERE profile=?1 AND task_id=?2",params![authority.profile.digest,query.id],|r|r.get::<_,usize>(0))?;
            if comment_count>crate::MAX_TASK_COMMENTS {return Err(Error::Capacity)}
            let mut statement=tx.prepare("SELECT author,text,at_ms FROM owner_operator_task_comments WHERE profile=?1 AND task_id=?2 ORDER BY ordinal LIMIT ?3 OFFSET ?4")?;
            task.comments=statement.query_map(params![authority.profile.digest,query.id,TASK_COMMENT_PAGE as u64,query.comment_offset as u64],|r|Ok(OperatorTaskComment{author:r.get(0)?,text:r.get(1)?,ts:time(r.get(2)?)}))?.collect::<Result<Vec<_>,_>>()?;
            drop(statement);
            let end=query.comment_offset+task.comments.len();
            let revision=tx.query_row("SELECT revision FROM owner_operator_tasks WHERE profile=?1 AND id=?2",params![authority.profile.digest,query.id],|r|r.get(0))?;
            let value=OperatorTaskDetail{write_supported:true,write_operations:vec![],revision:Some(revision),supported:true,reason:None,next:crate::operator_transitions(&task.status).iter().map(|s|(*s).into()).collect(),task:Some(task),comment_count,next_comment_offset:(end<comment_count).then_some(end),observed_at_ms:observed,unavailable:vec!["health"]};
            authority.check()?;tx.commit()?;Ok(Some(value))
        }).await?;
        after.check()?;
        self.check(profile)?;
        Ok(value)
    }
    pub(crate) async fn operator_board(
        &self,
        profile: &ProfileAuthority,
        query: OperatorBoardQuery,
        observed: u64,
    ) -> Result<Option<OperatorBoardPage>, Error> {
        query.validate()?;
        let authority = self.task_authority(profile)?;
        let backing = self.clone();
        let after = authority.clone();
        let value=self.registry.0.store.call(512,move|db|{
            let tx=db.db.unchecked_transaction()?;
            if !backing.task_read_proof(&tx,&authority)? { return Ok(None) }
            // The reviewed v1 source contract also excludes projects and
            // engagements. No central project or running-agent inference.
            if query.project_id.is_some() {return Err(Error::NotFound)}
            let mut totals=BTreeMap::new();
            for status in TASK_STATUSES {totals.insert(status.into(),tx.query_row("SELECT COUNT(*) FROM owner_operator_tasks WHERE profile=?1 AND status=?2",params![authority.profile.digest,status],|r|r.get(0))?);}
            let value=OperatorBoardPage{supported:true,reason:None,projects:vec![],next_after:None,project_count:0,agent_count:0,task_totals:totals,observed_at_ms:observed,stale_after_ms:300_000,unavailable:vec!["createdAt","binding","health","repositories","worktrees","specs","issues","activity"]};
            authority.check()?;tx.commit()?;Ok(Some(value))
        }).await?;
        after.check()?;
        self.check(profile)?;
        Ok(value)
    }
}
const SELECT: &str = "SELECT id,title,description,status,priority,granularity,assignee,created_by,created_at,updated_at,started_at,completed_at,heartbeat_at,waiting_reason,waiting_until,parent_id,labels FROM owner_operator_tasks";
fn time(ms: u64) -> String {
    crate::domain::operator_tasks::iso8601(ms)
}
fn task_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<OperatorTask, Error>> {
    let labels: String = r.get(16)?;
    let labels = match serde_json::from_str::<Vec<String>>(&labels) {
        Ok(labels) if labels.len() <= 20 && labels.iter().all(|s| s.chars().count() <= 64) => {
            labels
        }
        _ => return Ok(Err(Error::Quarantined)),
    };
    Ok(Ok(OperatorTask {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        status: r.get(3)?,
        priority: r.get(4)?,
        granularity: r.get(5)?,
        assignee: r.get(6)?,
        created_by: r.get(7)?,
        created_at: time(r.get(8)?),
        updated_at: time(r.get(9)?),
        started_at: r.get::<_, Option<u64>>(10)?.map(time),
        completed_at: r.get::<_, Option<u64>>(11)?.map(time),
        heartbeat_at: r.get::<_, Option<u64>>(12)?.map(time),
        waiting_reason: r.get(13)?,
        waiting_until: r.get(14)?,
        parent_id: r.get(15)?,
        labels,
        comments: vec![],
    }))
}
