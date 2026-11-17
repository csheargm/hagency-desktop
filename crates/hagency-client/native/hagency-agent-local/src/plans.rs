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
    /// The issue numbers in plan order; 0 for a step not filed yet. A plan
    /// is reserved with all zeros before anything is written on GitHub.
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
    /// Reserves a plan before filing; reserving the same id again keeps the
    /// first record (done or not), so a card is never filed twice.
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
    /// The plan `id`, open or done.
    pub fn plan(&self, agent: &str, id: &str) -> Result<Option<(PlanRecord, bool)>> {
        use rusqlite::OptionalExtension;
        let row: Option<(String, i64)> = self.db.query_row("SELECT record, done FROM agent_plans WHERE agent=? AND id=?", (agent, id), |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        row.map(|(text, done)| serde_json::from_str(&text).map(|p| (p, done != 0)).map_err(|_| Error::Invalid("stored plan is unreadable"))).transpose()
    }
    /// Records the number GitHub gave step `index`, once.
    pub fn set_plan_issue(&mut self, owner: &str, agent: &str, id: &str, index: usize, number: u64) -> Result<()> {
        self.owner(owner)?;
        let Some((mut plan, false)) = self.plan(agent, id)? else { return Err(Error::Invalid("no open plan with that id")) };
        match plan.issues.get_mut(index) {
            Some(slot) if *slot == 0 && number > 0 => *slot = number,
            _ => return Err(Error::Invalid("that step is filed already or does not exist")),
        }
        let text = serde_json::to_string(&plan).map_err(|_| Error::Invalid("unreadable plan"))?;
        self.db.execute("UPDATE agent_plans SET record=? WHERE agent=? AND id=?", (text, agent, id))?;
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
        let plan = PlanRecord { id: "plan_1".into(), repo: "acme/app".into(), room_id: "!r:x".into(), issues: vec![0, 0] };
        ledger.record_plan(owner, "anna", plan.clone()).unwrap();
        ledger.record_plan(owner, "anna", PlanRecord { issues: vec![0], ..plan.clone() }).unwrap();
        assert_eq!(ledger.open_plans("anna").unwrap(), vec![plan.clone()], "the first reservation holds");
        ledger.set_plan_issue(owner, "anna", "plan_1", 0, 4).unwrap();
        assert!(ledger.set_plan_issue(owner, "anna", "plan_1", 0, 7).is_err(), "a step is filed once");
        assert!(ledger.set_plan_issue(owner, "anna", "plan_1", 2, 7).is_err());
        ledger.set_plan_issue(owner, "anna", "plan_1", 1, 5).unwrap();
        assert_eq!(ledger.plan("anna", "plan_1").unwrap().unwrap().0.issues, [4, 5]);
        ledger.finish_plan(owner, "anna", "plan_1").unwrap();
        assert!(ledger.open_plans("anna").unwrap().is_empty());
        assert!(ledger.plan("anna", "plan_1").unwrap().unwrap().1, "a done plan is still known, so it is never filed again");
        for bad in [PlanRecord { id: "x".into(), ..plan.clone() }, PlanRecord { issues: vec![], ..plan.clone() }, PlanRecord { repo: "acme".into(), ..plan }] {
            assert!(ledger.record_plan(owner, "anna", bad).is_err());
        }
    }
}
