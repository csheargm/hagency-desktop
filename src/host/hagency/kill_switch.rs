//! My Agents' "Stop all agents" and "Resume all" (see `crate::hagency::kill_switch`).
//! No arguments: the switch acts only on the signed-in owner's Agents here.
use super::{parse, Empty, ManagementPermission, Value};
use crate::hagency::kill_switch;
pub(super) const STOP_ALL: &str = "hagency.personal_agents.stop_all";
pub(super) const RESUME_ALL: &str = "hagency.personal_agents.resume_all";
pub(super) const PAUSED: &str = "hagency.personal_agents.paused";
pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        PAUSED => &[ReadPersonalAgentSettings],
        STOP_ALL | RESUME_ALL => &[ReadPersonalAgentSettings, EditPersonalAgentPolicy],
        _ => &[],
    }
}
pub(super) enum Call {
    StopAll,
    ResumeAll,
    Paused,
}
pub(super) fn parse_call(service: &str, args: Value) -> Result<Option<Call>, String> {
    let call = match service {
        STOP_ALL => Call::StopAll,
        RESUME_ALL => Call::ResumeAll,
        PAUSED => Call::Paused,
        _ => return Ok(None),
    };
    let _: Empty = parse(args)?;
    Ok(Some(call))
}
impl Call {
    pub(super) async fn execute(self, account: &str) -> Result<Value, String> {
        let path = kill_switch::default_path();
        let paused = |path: &std::path::Path| kill_switch::load(path, account).is_some_and(|p| !p.agents.is_empty());
        if let Self::Paused = self {
            return Ok(kill_switch::Report::default().to_json(paused(&path)));
        }
        let service = crate::hagency::backend::Service::for_current()?;
        if crate::sliding_sync::current_user_id().is_none_or(|user| user.as_str() != account) {
            return Err("matrix_account_changing".into());
        }
        let agents = kill_switch::Live(service);
        let report = match self {
            Self::StopAll => kill_switch::stop_all(&agents, &path, account).await?,
            _ => kill_switch::resume_all(&agents, &path, account).await?,
        };
        Ok(report.to_json(paused(&path)))
    }
}
