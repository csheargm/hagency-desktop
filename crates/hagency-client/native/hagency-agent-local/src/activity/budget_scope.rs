//! Whether a new task would be refused for its budget in one room, as
//! `Ledger::begin_postpaid` judges it: every layer of the task's scope (the
//! Agent, its room binding, and the requester there) whose token budget for
//! the period of `now` is spent. The Agent-wide check alone missed a used-up
//! room budget, so hand-offs burned attempts and re-sends ran out while every
//! run was refused (integration GPT review, Nov 18).
use crate::{Ledger, Limit, Result, Scope};
use rusqlite::OptionalExtension;

impl Ledger {
    /// Whether `agent`'s next task in `room` from `requester` would be
    /// refused with `Error::Budget`: its own budget, or the room's or the
    /// requester's on the Agent's newest binding in that room. A room the
    /// Agent has no binding in has only the Agent-wide budget.
    pub fn room_budget_used_up(&self, owner: &str, agent: &str, room: &str, requester: &str, now: i64) -> Result<bool> {
        if self.agent_budget_used_up(owner, agent, now)? {
            return Ok(true);
        }
        let Some(binding) = self.newest_binding(agent, room)? else { return Ok(false) };
        let scope = Scope { agent: agent.into(), binding, room: room.into(), requester: requester.into(), thread: String::new() };
        for layer in scope.layers()? {
            if self.layer_spent(&layer, now)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The rooms where `agent`'s next task from `requester` would be refused
    /// for a room's or requester's budget (the Agent-wide one is
    /// [`Ledger::agent_budget_used_up`]), sorted.
    pub fn budget_used_up_rooms(&self, owner: &str, agent: &str, requester: &str, now: i64) -> Result<Vec<String>> {
        let rooms: Vec<String> = {
            let mut statement = self.db.prepare("SELECT DISTINCT room FROM bindings WHERE agent=? ORDER BY room")?;
            let rows = statement.query_map([agent], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for room in rooms {
            if self.room_budget_used_up(owner, agent, &room, requester, now)? {
                out.push(room);
            }
        }
        Ok(out)
    }

    /// The binding `agent` was registered with in `room` last (rowid order).
    fn newest_binding(&self, agent: &str, room: &str) -> Result<Option<String>> {
        const NEWEST: &str = "SELECT id FROM bindings WHERE agent=? AND room=? ORDER BY rowid DESC LIMIT 1";
        Ok(self.db.query_row(NEWEST, (agent, room), |r| r.get(0)).optional()?)
    }

    /// One scope layer's spend reached its token limit (a new task's check in `begin_postpaid`).
    fn layer_spent(&self, layer: &str, now: i64) -> Result<bool> {
        let policy = crate::policy(&self.db, layer, 0)?;
        let Limit::Tokens(limit) = policy.budget.limit else { return Ok(false) };
        let window = policy.budget.period.window(now)?;
        let spent: Option<u64> = self.db.query_row("SELECT spent FROM accounts WHERE scope=? AND window=?", (layer, &window), |r| r.get(0)).optional()?;
        Ok(spent.unwrap_or(0) >= limit)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Budget, Layer, Ledger, Limit, Period, Scope};

    const OWNER: &str = "@alice:test";
    const NOW: i64 = 1_795_018_600;

    fn scope(binding: &str, room: &str) -> Scope {
        Scope { agent: "agent".into(), binding: binding.into(), room: room.into(), requester: OWNER.into(), thread: "t".into() }
    }
    /// `layer` of `scope` gets a 100-token day budget, already spent.
    fn spend(l: &mut Ledger, scope: &Scope, layer: Layer) {
        let mut policy = l.agent_policy(OWNER, "agent").unwrap().policy;
        policy.budget = Budget { limit: Limit::Tokens(100), period: Period::UtcDay };
        let index = match layer { Layer::Agent => 0, Layer::Room => 1, Layer::Requester => 2 };
        l.set_policy(OWNER, scope, layer, 0, &policy).unwrap();
        let window = Period::UtcDay.window(NOW).unwrap();
        l.db.execute("INSERT INTO accounts VALUES (?,?,100,0)", (&scope.layers().unwrap()[index], &window)).unwrap();
    }

    #[test]
    fn a_used_up_room_budget_counts_only_in_that_room() {
        let temp = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
        let (a, b) = (scope("bind-a", "!a:test"), scope("bind-b", "!b:test"));
        l.register_binding(OWNER, &a).unwrap();
        l.register_binding(OWNER, &b).unwrap();
        spend(&mut l, &a, Layer::Room);
        assert!(!l.agent_budget_used_up(OWNER, "agent", NOW).unwrap(), "the precondition: the Agent-wide check misses it");
        assert!(matches!(l.begin_postpaid(&a, "probe", "d", NOW), Err(crate::Error::Budget)), "the ledger refuses the run");
        assert!(l.room_budget_used_up(OWNER, "agent", "!a:test", OWNER, NOW).unwrap());
        assert!(!l.room_budget_used_up(OWNER, "agent", "!b:test", OWNER, NOW).unwrap(), "another room still runs");
        assert!(!l.room_budget_used_up(OWNER, "agent", "!a:test", OWNER, NOW + 86_400).unwrap(), "a new UTC day allows again");
        assert!(!l.room_budget_used_up(OWNER, "agent", "!none:test", OWNER, NOW).unwrap());
        assert_eq!(l.budget_used_up_rooms(OWNER, "agent", OWNER, NOW).unwrap(), ["!a:test"]);
    }

    #[test]
    fn a_used_up_requester_or_agent_budget_counts_too() {
        let temp = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
        let a = scope("bind-a", "!a:test");
        l.register_binding(OWNER, &a).unwrap();
        spend(&mut l, &a, Layer::Requester);
        assert!(l.room_budget_used_up(OWNER, "agent", "!a:test", OWNER, NOW).unwrap());
        assert!(!l.room_budget_used_up(OWNER, "agent", "!a:test", "@other:test", NOW).unwrap(), "another requester's layer is its own");
        spend(&mut l, &scope("bind-a", "!a:test"), Layer::Agent);
        assert!(l.room_budget_used_up(OWNER, "agent", "!z:test", "@other:test", NOW).unwrap(), "the Agent-wide budget counts everywhere");
    }
}
