//! Closed original personal-agent policy services. No provider/path arguments.
use super::{parse, Id, ManagementPermission, Value};
use ::hagency::native_owner::{
    ObservePersonalSettings, PersonalAgentQuery, SavePersonalPolicy,
};
use ::hagency::daemon::bridge::ManagementClient as NativeManagement;
use std::sync::Arc;
pub(super) const LIST: &str = "hagency.personal_agents.list";
pub(super) const GET: &str = "hagency.personal_agents.get";
pub(super) const OBSERVE: &str = "hagency.personal_agents.observe";
pub(super) const SAVE: &str = "hagency.personal_agents.policy.save";
pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        LIST | GET => &[ReadPersonalAgentSettings],
        OBSERVE => &[
            ReadPersonalAgentSettings,
            RequestPersonalAgentSettings,
            ReadIntents,
        ],
        SAVE => &[
            ReadPersonalAgentSettings,
            EditPersonalAgentPolicy,
            ReadIntents,
        ],
        _ => &[],
    }
}
pub(super) enum Call {
    List(PersonalAgentQuery),
    Get(String),
    Save(SavePersonalPolicy),
    Observe(ObservePersonalSettings),
}
pub(super) fn parse_call(service: &str, args: Value) -> Result<Option<Call>, String> {
    Ok(Some(match service {
        LIST => {
            let q: PersonalAgentQuery = parse(args)?;
            q.validate().map_err(|e| e.code)?;
            Call::List(q)
        }
        GET => Call::Get(parse::<Id>(args)?.id),
        OBSERVE => {
            let input: ObservePersonalSettings = parse(args)?;
            input.validate().map_err(|e| e.code)?;
            Call::Observe(input)
        }
        SAVE => {
            let input: SavePersonalPolicy = parse(args)?;
            input.validate().map_err(|e| e.code)?;
            Call::Save(input)
        }
        _ => return Ok(None),
    }))
}
impl Call {
    pub(super) async fn execute(self, manager: Arc<NativeManagement>) -> Result<Value, String> {
        let mut reply = match self {
            Self::List(q) => manager.personal_agents(q).await,
            Self::Get(id) => manager.personal_agent_settings(id).await,
            Self::Observe(input) => manager.observe_personal_settings(input).await,
            Self::Save(input) => manager.save_personal_policy(input).await,
        }
        .map_err(|e| e.code)?;
        if let Some(settings) = reply.get_mut("settings") {
            let at = &settings["observedAtMs"];
            let (utc, local) = if at.is_null() {
                (Value::Null, Value::Null)
            } else {
                let timestamp = at
                    .as_i64()
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .ok_or("personal_agent_settings_invalid")?;
                let local = timestamp.with_timezone(&chrono::Local);
                (
                    Value::String(timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                    Value::String(local.format("%Y-%m-%d %H:%M").to_string()),
                )
            };
            settings["observedAtUtc"] = utc;
            settings["observedAtLocal"] = local;
        }
        Ok(reply)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn personal_policy_host_contract_has_no_raw_command_or_path_surface() {
        let observe =
            json!({"commandId":"original_read","agentId":"agent_a","expectedGeneration":3});
        assert!(matches!(
            parse_call(OBSERVE, observe.clone()).unwrap(),
            Some(Call::Observe(_))
        ));
        for key in [
            "command",
            "requester",
            "path",
            "executionDeviceId",
            "provider",
        ] {
            let mut bad = observe.clone();
            bad[key] = json!("forged");
            assert!(parse_call(OBSERVE, bad).is_err());
        }
        assert!(permissions(OBSERVE).contains(&ManagementPermission::RequestPersonalAgentSettings));
        assert!(!permissions(GET).contains(&ManagementPermission::RequestPersonalAgentSettings));
        let good = json!({"commandId":"original","agentId":"agent_a","expectedGeneration":3,"expectedRevision":4,
            "policy":{"budget":{"kind":"tokens","tokens":250,"period":"utc_month"},"requests":"ask_owner","highRiskTools":"deny"}});
        assert!(matches!(
            parse_call(SAVE, good.clone()).unwrap(),
            Some(Call::Save(_))
        ));
        for key in [
            "command",
            "requester",
            "executionDeviceId",
            "workspace",
            "credentialRef",
        ] {
            let mut bad = good.clone();
            bad[key] = json!("forged");
            assert!(parse_call(SAVE, bad).is_err());
        }
        let mut bad = good.clone();
        bad["policy"]["highRiskTools"] = json!("rules");
        assert!(parse_call(SAVE, bad).is_err());
        bad = good;
        bad["policy"]["budget"]["tokens"] = json!(9_007_199_254_740_992u64);
        assert!(parse_call(SAVE, bad).is_err());
        assert!(parse_call(LIST, json!({"limit":17})).is_err());
        assert_eq!(
            permissions(GET),
            &[ManagementPermission::ReadPersonalAgentSettings]
        );
        assert!(!permissions(GET).contains(&ManagementPermission::EditPersonalAgentPolicy));
        assert!(permissions(SAVE).contains(&ManagementPermission::EditPersonalAgentPolicy));
    }
}
