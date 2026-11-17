//! Filed plans (ADR 0017): which GitHub issues a planner card became, kept in
//! this device's ledger until every issue is closed as completed and the
//! room has been told "Done". Written only by the owner API.
use crate::{key, Error, Ledger, Result};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS agent_plans (agent TEXT NOT NULL, id TEXT NOT NULL, record TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(agent,id)) STRICT;";
/// Open plans kept per Agent; older ones must finish first.
pub const MAX_OPEN_PLANS: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanRecord {
    pub id: String,
    pub repo: String,
    pub room_id: String,
    /// The filed issue numbers, in plan order.
    pub issues: Vec<u64>,
}
impl PlanRecord {
    fn validate(self) -> Result<Self> {
        let ok = self.id.starts_with("plan_") && self.id.len() <= 64 && crate::issue_watch::valid_repo(&self.repo)
            && self.room_id.starts_with('!') && !self.issues.is_empty() && self.issues.len() <= 12;
        if ok { Ok(self) } else { Err(Error::Invalid("a plan needs an id, owner/name, a room and its issues")) }
    }
}

impl Ledger {
    /// Records a filed plan; recording the same id again keeps the first.
    pub fn record_plan(&mut self, owner: &str, agent: &str, plan: PlanRecord) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        let plan = plan.validate()?;
        if self.open_plans(agent)?.len() >= MAX_OPEN_PLANS {
            return Err(Error::Invalid("too many open plans"));
        }
        let text = serde_json::to_string(&plan).map_err(|_| Error::Invalid("unreadable plan"))?;
        self.db.execute("INSERT OR IGNORE INTO agent_plans (agent,id,record) VALUES (?,?,?)", (agent, &plan.id, text))?;
        Ok(())
    }
    /// Plans whose "Done" has not been posted yet, oldest first.
    pub fn open_plans(&self, agent: &str) -> Result<Vec<PlanRecord>> {
        let mut statement = self.db.prepare("SELECT record FROM agent_plans WHERE agent=? AND done=0 ORDER BY rowid")?;
        let rows = statement.query_map([agent], |r| r.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).map_err(|_| Error::Invalid("stored plan is unreadable"))).collect()
    }
    /// Marks the plan done once its "Done" was posted.
    pub fn finish_plan(&mut self, owner: &str, agent: &str, id: &str) -> Result<()> {
        self.owner(owner)?;
        self.db.execute("UPDATE agent_plans SET done=1 WHERE agent=? AND id=?", (agent, id))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filed_plan_stays_open_until_finished_and_is_recorded_once() {
        let temp = tempfile::tempdir().unwrap();
        let owner = "@owner:example.org";
        let mut ledger = Ledger::open(temp.path().join("local.db"), owner).unwrap();
        let plan = PlanRecord { id: "plan_1".into(), repo: "acme/app".into(), room_id: "!r:x".into(), issues: vec![4, 5] };
        ledger.record_plan(owner, "anna", plan.clone()).unwrap();
        ledger.record_plan(owner, "anna", PlanRecord { issues: vec![9], ..plan.clone() }).unwrap();
        assert_eq!(ledger.open_plans("anna").unwrap(), vec![plan.clone()], "the first record holds");
        ledger.finish_plan(owner, "anna", "plan_1").unwrap();
        assert!(ledger.open_plans("anna").unwrap().is_empty());
        for bad in [PlanRecord { id: "x".into(), ..plan.clone() }, PlanRecord { issues: vec![], ..plan.clone() }, PlanRecord { repo: "acme".into(), ..plan }] {
            assert!(ledger.record_plan(owner, "anna", bad).is_err());
        }
    }
}
