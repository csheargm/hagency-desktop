//! Personal policy edits reuse the original execution-device command. The
//! private intent pins requester identity before POST; no paths or credentials
//! are accepted by the script, and unknown replies never mint another key.
use super::{Command, LocalAction, NativeError, NativeManagement, failure};
use crate::console::device_execution::DeviceIdentity;
use hagency_agent_local::{Budget, Limit, Period, Policy, RequestPolicy, ToolPolicy};
use hagency_store::management::{Intent, IntentOperation, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_NUMBER: u64 = 9_007_199_254_740_991;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavePersonalPolicy {
    pub command_id: String,
    pub agent_id: String,
    pub expected_generation: i64,
    pub expected_revision: i64,
    pub policy: PersonalPolicy,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonalPolicy {
    pub budget: PersonalBudget,
    pub requests: Requests,
    pub high_risk_tools: HighRisk,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonalBudget {
    pub kind: BudgetKind,
    pub tokens: Option<u64>,
    pub period: BudgetPeriod,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    Unset,
    Unlimited,
    Tokens,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Lifetime,
    UtcDay,
    UtcMonth,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requests {
    Allow,
    Deny,
    AskOwner,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighRisk {
    Deny,
    AskOwner,
    /// Every tool without asking; see `ToolPolicy::full_trust`.
    FullTrust,
}
impl SavePersonalPolicy {
    pub fn validate(&self) -> Result<(), NativeError> {
        super::key(&self.command_id)?;
        super::key(&self.agent_id)?;
        if self.expected_generation < 1
            || self.expected_generation as u64 > MAX_NUMBER
            || self.expected_revision < 0
            || self.expected_revision as u64 >= MAX_NUMBER
        {
            return Err(failure(400, "invalid_personal_policy"));
        }
        self.policy.native()?;
        Ok(())
    }
}
impl PersonalPolicy {
    fn native(&self) -> Result<Policy, NativeError> {
        let limit = match (self.budget.kind, self.budget.tokens) {
            (BudgetKind::Unset, None) => Limit::Unset,
            (BudgetKind::Unlimited, None) => Limit::Unlimited,
            (BudgetKind::Tokens, Some(n)) if n <= MAX_NUMBER => Limit::Tokens(n),
            _ => return Err(failure(400, "invalid_personal_policy")),
        };
        Ok(Policy {
            budget: Budget {
                limit,
                period: match self.budget.period {
                    BudgetPeriod::Lifetime => Period::Lifetime,
                    BudgetPeriod::UtcDay => Period::UtcDay,
                    BudgetPeriod::UtcMonth => Period::UtcMonth,
                },
            },
            requests: match self.requests {
                Requests::Allow => RequestPolicy::Allow,
                Requests::Deny => RequestPolicy::Deny,
                Requests::AskOwner => RequestPolicy::AskOwner,
            },
            high_risk: match self.high_risk_tools {
                HighRisk::Deny => ToolPolicy::Deny,
                HighRisk::AskOwner => ToolPolicy::AskOwner,
                HighRisk::FullTrust => ToolPolicy::full_trust(),
            },
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    command_id: String,
    input: SavePersonalPolicy,
    requester: DeviceIdentity,
}
// These errors establish non-application only on a genuinely first attempt.
// Auth, transport, generic 404/400, target replacement, expiry and ambiguous
// executor failures deliberately remain pending in the original journal.
// Capability discovery is repeated while polling, so configuration_unsupported
// may follow a committed POST and is never proof of non-application.
pub(super) fn known_failure(e: &NativeError) -> bool {
    matches!(
        e.code.as_str(),
        "invalid_personal_policy"
            | "personal_agent_not_found"
            | "personal_agent_settings_changed"
            | "personal_policy_observation_required"
            | "configuration_device_offline"
            | "configuration_device_unavailable"
            | "configuration_queue_full"
            | "configuration_request_busy"
            | "execution_device_required"
            | "agent_retired"
            | "local_revision_conflict"
            | "invalid_local_policy"
    )
}
impl NativeManagement {
    pub async fn save_personal_policy(
        &self,
        input: SavePersonalPolicy,
    ) -> Result<Value, NativeError> {
        input.validate()?;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::EditPersonalAgentPolicy)
            .map_err(|_| failure(403, "management_permission_required"))?;
        // A fresh script draft never changes the private original requester pin.
        let existing = self
            .saved_commands()
            .await?
            .into_iter()
            .find(|row| row.command_id == input.command_id);
        let recovering = existing.is_some();
        let saved = if let Some(existing) = existing {
            if existing.operation != IntentOperation::PersonalAgentPolicy {
                return Err(failure(409, "management_command_conflict"));
            }
            let saved: Saved = serde_json::from_value(existing.input)
                .map_err(|_| failure(503, "management_saved_command_invalid"))?;
            if serde_json::to_value(&saved.input).ok() != serde_json::to_value(&input).ok() {
                return Err(failure(409, "management_command_conflict"));
            }
            saved
        } else {
            Saved {
                command_id: input.command_id.clone(),
                input,
                requester: super::configuration::pin(&self.owner).await?,
            }
        };
        self.apply_personal_policy(saved, recovering).await
    }
    pub(super) async fn retry_personal_policy(&self, input: Value) -> Result<Value, NativeError> {
        let saved: Saved = serde_json::from_value(input)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        saved.input.validate()?;
        if saved.command_id != saved.input.command_id {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        self.apply_personal_policy(saved, true).await
    }
    async fn apply_personal_policy(
        &self,
        saved: Saved,
        recovering: bool,
    ) -> Result<Value, NativeError> {
        let intent = Intent::new(
            IntentOperation::PersonalAgentPolicy,
            saved.command_id.clone(),
            &saved,
        )
        .map_err(super::management::store_error)?;
        self.with_intent(intent, async {
            if super::configuration::pin(&self.owner).await? != saved.requester {
                return Err(failure(409, "personal_policy_original_device_required"));
            }
            // Current personal-owner visibility is always required. A known
            // saved command may recover its old result after target generation
            // advances: the server checks the original idempotency key before
            // current target CAS. Missing original history cannot start a fresh
            // command at an old generation. Fresh drafts still fail locally.
            // Shared agents never enter this route.
            let detail = self.personal_agent_settings(saved.input.agent_id.clone()).await?;
            if !recovering && detail["agent"]["generation"] != saved.input.expected_generation {
                return Err(failure(409, "personal_agent_settings_changed"));
            }
            let policy = saved.input.policy.native()?;
            let result = super::configuration::remote_pinned(&self.owner, Command::AgentConfiguration {
                agent_id:saved.input.agent_id.clone(),
                expected_generation:saved.input.expected_generation,
                idempotency_key:saved.command_id.clone(),
                command:Box::new(Command::Local {
                    agent_id:saved.input.agent_id.clone(), action:LocalAction::SaveAgentPolicy,
                    input:Some(json!({"expectedRevision":saved.input.expected_revision,"policy":policy})),
                    binding_id:None, requester:None,
                }),
            },Some(&saved.requester)).await?;
            let revision = result["policy"]["revision"].as_i64()
                .filter(|r| *r > saved.input.expected_revision && *r as u64 <= MAX_NUMBER)
                .ok_or_else(||failure(409,"management_outcome_unknown"))?;
            if result["policy"]["policy"] != json!(policy) {
                return Err(failure(409,"management_outcome_unknown"));
            }
            // Only public, historical confirmation crosses the script boundary.
            Ok(json!({"protocol":"hagency-personal-settings-v1","agentId":saved.input.agent_id,
                "commandId":saved.command_id,"policyRevision":revision,"saved":true}))
        }).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// The owner's full-trust choice is saved as the full-trust rule, and
    /// the form knows that rule again when it reads the policy back.
    #[test]
    fn the_full_trust_choice_saves_the_full_trust_rule() {
        let save = |risk: &str| -> SavePersonalPolicy {
            serde_json::from_value(json!({"commandId":"command_a","agentId":"agent_a",
                "expectedGeneration":1,"expectedRevision":0,
                "policy":{"budget":{"kind":"unlimited","tokens":null,"period":"lifetime"},
                    "requests":"allow","highRiskTools":risk}}))
            .unwrap()
        };
        let trusted = save("full_trust");
        trusted.validate().unwrap();
        let policy = trusted.policy.native().unwrap();
        assert!(policy.high_risk.is_full_trust());
        let rule = serde_json::to_value(&policy.high_risk).unwrap();
        assert_eq!(rule, crate::native_owner::full_trust_rule());
        assert!(crate::native_owner::is_full_trust_rule(&rule));
        for (risk, expected) in [("ask_owner", ToolPolicy::AskOwner), ("deny", ToolPolicy::Deny)] {
            let policy = save(risk).policy.native().unwrap();
            assert_eq!(policy.high_risk, expected);
            assert!(!crate::native_owner::is_full_trust_rule(
                &serde_json::to_value(&policy.high_risk).unwrap()
            ));
        }
    }
}
