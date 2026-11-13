//! Read-only personal-agent configuration observations. Never opens the personal
//! ledger, submits an environment request or exposes provider/path credentials.
use super::{Command, NativeError, NativeManagement, failure};
use hagency_agent_local::{Limit, ModelProfile, Period, PolicyVersion, RequestPolicy, ToolPolicy};
use hagency_store::management::Permission;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const MAX_NUMBER: u64 = 9_007_199_254_740_991;
#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonalAgentQuery {
    pub after: Option<String>,
    pub limit: Option<u16>,
}
impl PersonalAgentQuery {
    pub fn validate(&self) -> Result<(), NativeError> {
        if let Some(id) = &self.after {
            super::key(id)?;
        }
        if self.limit.is_some_and(|n| !(1..=16).contains(&n)) {
            return Err(failure(400, "invalid_arguments"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Agent {
    id: String,
    display_name: String,
    state: String,
    generation: i64,
    execution_device_id: Option<String>,
}
fn invalid() -> NativeError {
    failure(503, "personal_agent_settings_invalid")
}
fn bounded_text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}
fn number(n: u64) -> Result<u64, NativeError> {
    if n > MAX_NUMBER {
        Err(invalid())
    } else {
        Ok(n)
    }
}
fn optional_time(value: &Value) -> Result<Option<u64>, NativeError> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_u64()
            .filter(|n| *n > 0 && *n <= MAX_NUMBER)
            .map(Some)
            .ok_or_else(invalid)
    }
}
fn agents(value: Value, owner: &str) -> Result<Vec<Agent>, NativeError> {
    if value["ownerMxid"] != owner {
        return Err(failure(401, "management_scope_changed"));
    }
    let rows = value["agents"]
        .as_array()
        .filter(|r| r.len() <= 1024)
        .ok_or_else(invalid)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut seen = BTreeSet::new();
    for row in rows {
        let a: Agent = serde_json::from_value(row.clone()).map_err(|_| invalid())?;
        super::key(&a.id).map_err(|_| invalid())?;
        if let Some(id) = &a.execution_device_id {
            super::key(id).map_err(|_| invalid())?;
        }
        if !seen.insert(a.id.clone())
            || a.generation < 1
            || a.generation as u64 > MAX_NUMBER
            || !bounded_text(&a.display_name, 256)
            || a.display_name.is_empty()
            || !bounded_text(&a.state, 80)
        {
            return Err(invalid());
        }
        out.push(a);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
fn settings(value: Value, agent: &Agent) -> Result<Value, NativeError> {
    let c = &value["configuration"];
    if c["agentId"] != agent.id
        || c["agentGeneration"] != agent.generation
        || c["executionDeviceId"] != json!(agent.execution_device_id)
    {
        return Err(failure(409, "personal_agent_settings_changed"));
    }
    let online = c["online"].as_bool().ok_or_else(invalid)?;
    let cached = c["cached"].as_bool().ok_or_else(invalid)?;
    let observed = optional_time(&c["observedAtMs"])?;
    let seen = optional_time(&c["lastSeenAtMs"])?;
    let device_generation = optional_time(&c["deviceGeneration"])?;
    let configuration = &c["environment"]["configuration"];
    let observation = if c["environment"].is_null() {
        Value::Null
    } else {
        if observed.is_none() || configuration["agentId"] != agent.id {
            return Err(invalid());
        }
        let policy: PolicyVersion =
            serde_json::from_value(configuration["policy"].clone()).map_err(|_| invalid())?;
        if policy.revision < 0 || policy.revision as u64 > MAX_NUMBER {
            return Err(invalid());
        }
        let (limit_kind, limit) = match policy.policy.budget.limit {
            Limit::Unset => ("unset", None),
            Limit::Unlimited => ("unlimited", None),
            Limit::Tokens(n) => ("tokens", Some(number(n)?)),
        };
        let period = match policy.policy.budget.period {
            Period::Lifetime => "lifetime",
            Period::UtcDay => "utc_day",
            Period::UtcMonth => "utc_month",
        };
        let requests = match policy.policy.requests {
            RequestPolicy::Allow => "allow",
            RequestPolicy::Deny => "deny",
            RequestPolicy::AskOwner => "ask_owner",
        };
        let (tools, tool_count, directory_count) = match policy.policy.high_risk {
            ToolPolicy::Deny => ("deny", 0, 0),
            ToolPolicy::AskOwner => ("ask_owner", 0, 0),
            ref trusted if trusted.is_full_trust() => ("full_trust", 0, 0),
            ToolPolicy::AllowWithRules { tools, directories } => {
                if tools.len() > 1024 || directories.len() > 1024 {
                    return Err(invalid());
                }
                ("rules", tools.len(), directories.len())
            }
        };
        let model = if configuration["modelProfile"].is_null() {
            Value::Null
        } else {
            let p: ModelProfile = serde_json::from_value(configuration["modelProfile"].clone())
                .map_err(|_| invalid())?;
            if !bounded_text(&p.model, 256) || !bounded_text(&p.reasoning_effort, 80) {
                return Err(invalid());
            }
            json!({"model":p.model,"reasoningEffort":p.reasoning_effort,"webSearch":p.web_search,"workspaceConfigured":!p.workspace_root.is_empty()})
        };
        let consumed = number(
            configuration["usage"]["spent"]
                .as_u64()
                .ok_or_else(invalid)?,
        )?;
        let held = number(
            configuration["usage"]["held"]
                .as_u64()
                .ok_or_else(invalid)?,
        )?;
        json!({"policyRevision":policy.revision,"budget":{"kind":limit_kind,"tokens":limit,"period":period},"requests":requests,"highRiskTools":tools,"toolRuleCount":tool_count,"directoryRuleCount":directory_count,"knownConsumedTokens":consumed,"heldTokens":held,"model":model})
    };
    Ok(
        json!({"supported":true,"online":online,"cached":cached,"deviceGeneration":device_generation,"lastSeenAtMs":seen,"observedAtMs":observed,"observation":observation}),
    )
}
impl NativeManagement {
    pub async fn personal_agents(&self, query: PersonalAgentQuery) -> Result<Value, NativeError> {
        query.validate()?;
        let (scope, session) = self.authorized().await?;
        session
            .authorize(&scope, Permission::ReadPersonalAgentSettings)
            .map_err(|_| failure(403, "management_permission_required"))?;
        let all = agents(
            self.owner.execute(Command::Agents).await?,
            &self.owner.0.expected_owner,
        )?;
        let mut rows = all
            .into_iter()
            .filter(|a| query.after.as_ref().is_none_or(|after| a.id > *after))
            .collect::<Vec<_>>();
        let limit = query.limit.unwrap_or(16) as usize;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = more.then(|| rows.last().unwrap().id.clone());
        let (current, session) = self.authorized().await?;
        if current != scope {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&current, Permission::ReadPersonalAgentSettings)
            .map_err(|_| failure(403, "management_permission_required"))?;
        Ok(json!({"protocol":"hagency-personal-settings-v1","items":rows,"nextAfter":next}))
    }
    pub async fn personal_agent_settings(&self, id: String) -> Result<Value, NativeError> {
        super::key(&id)?;
        let (scope, session) = self.authorized().await?;
        session
            .authorize(&scope, Permission::ReadPersonalAgentSettings)
            .map_err(|_| failure(403, "management_permission_required"))?;
        let agent = agents(
            self.owner.execute(Command::Agents).await?,
            &self.owner.0.expected_owner,
        )?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| failure(404, "personal_agent_not_found"))?;
        let observed = match self
            .owner
            .execute(Command::AgentConfigurationStatus { agent_id: id })
            .await
        {
            Ok(v) => settings(v, &agent)?,
            Err(e) if e.status == 409 && e.code == "configuration_unsupported" => {
                json!({"supported":false,"observation":null})
            }
            Err(e) => return Err(e),
        };
        let (current, session) = self.authorized().await?;
        if current != scope {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&current, Permission::ReadPersonalAgentSettings)
            .map_err(|_| failure(403, "management_permission_required"))?;
        Ok(json!({"protocol":"hagency-personal-settings-v1","agent":agent,"settings":observed}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent() -> Agent {
        Agent {
            id: "agent_a".into(),
            display_name: "A".into(),
            state: "active".into(),
            generation: 3,
            execution_device_id: Some("device_a".into()),
        }
    }
    pub(super) fn snapshot() -> Value {
        json!({"configuration":{"agentId":"agent_a","agentGeneration":3,"executionDeviceId":"device_a","deviceGeneration":2,"online":false,"cached":true,"lastSeenAtMs":1000,"observedAtMs":900,"environment":{"configuration":{"agentId":"agent_a","policy":{"revision":4,"policy":{"budget":{"limit":{"Tokens":100},"period":"UtcMonth"},"requests":"AskOwner","high_risk":{"AllowWithRules":{"tools":["private-command"],"directories":["/private/home"]}}}},"usage":{"spent":17,"held":30},"modelProfile":{"model":"fixture-model","reasoning_effort":"high","credential_ref":"private-reference","workspace_root":"/private/workspace","web_search":true}},"providers":[{"secret":"never-public"}]}}})
    }
    #[test]
    fn personal_settings_projection_is_bounded_and_drops_private_values() {
        let value = settings(snapshot(), &agent()).unwrap();
        let raw = value.to_string();
        for secret in [
            "private-command",
            "/private/home",
            "private-reference",
            "/private/workspace",
            "never-public",
        ] {
            assert!(!raw.contains(secret));
        }
        assert_eq!(value["observation"]["knownConsumedTokens"], 17);
        assert_eq!(value["observation"]["heldTokens"], 30);
        assert_eq!(value["observation"]["model"]["workspaceConfigured"], true);
        assert_eq!(value["observation"]["directoryRuleCount"], 1);
        assert_eq!(value["cached"], true);
        let mut changed = snapshot();
        changed["configuration"]["agentGeneration"] = json!(4);
        assert_eq!(
            settings(changed, &agent()).unwrap_err().code,
            "personal_agent_settings_changed"
        );
        let mut bad = snapshot();
        bad["configuration"]["environment"]["configuration"]["usage"]["spent"] =
            json!(MAX_NUMBER + 1);
        assert!(settings(bad, &agent()).is_err());
        let mut missing = snapshot();
        missing["configuration"]["observedAtMs"] = Value::Null;
        assert!(settings(missing, &agent()).is_err());
    }
    /// Full trust reads as its own setting, not as a list of rules.
    #[test]
    fn personal_settings_report_full_trust_as_its_own_choice() {
        let mut trusted = snapshot();
        trusted["configuration"]["environment"]["configuration"]["policy"]["policy"]["high_risk"] =
            crate::native_owner::full_trust_rule();
        let value = settings(trusted, &agent()).unwrap();
        assert_eq!(value["observation"]["highRiskTools"], "full_trust");
        assert_eq!(value["observation"]["toolRuleCount"], 0);
        assert_eq!(value["observation"]["directoryRuleCount"], 0);
        assert_eq!(
            settings(snapshot(), &agent()).unwrap()["observation"]["highRiskTools"],
            "rules"
        );
    }
    #[test]
    fn personal_settings_absent_observation_does_not_invent_zero() {
        let mut v = snapshot();
        v["configuration"]["environment"] = Value::Null;
        v["configuration"]["observedAtMs"] = Value::Null;
        let out = settings(v, &agent()).unwrap();
        assert!(out["observation"].is_null());
        assert!(serde_json::from_value::<PersonalAgentQuery>(json!({"path":"/tmp"})).is_err());
        assert!(
            PersonalAgentQuery {
                after: Some("../other".into()),
                limit: None
            }
            .validate()
            .is_err()
        );
        assert!(
            PersonalAgentQuery {
                after: None,
                limit: Some(17)
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn personal_agent_index_rejects_other_scope_and_duplicate_identity() {
        let row = json!(agent());
        let value = json!({"ownerMxid":"@owner:test","agents":[row.clone()]});
        assert_eq!(agents(value.clone(), "@owner:test").unwrap().len(), 1);
        assert!(agents(value, "@other:test").is_err());
        assert!(
            agents(
                json!({"ownerMxid":"@owner:test","agents":[row.clone(),row]}),
                "@owner:test"
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "personal_agent_settings_http_tests.rs"]
mod http_tests;
