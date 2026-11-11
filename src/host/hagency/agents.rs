//! Closed human agent-admission services; no runtime or provider operations.
use super::{parse, Id, ManagementCommand, ManagementPermission, Value};
use ::hagency::native_owner::AgentRequestInput;
use serde::Deserialize;
pub(super) const READINESS: &str = "hagency.agents.readiness";
pub(super) const LIST: &str = "hagency.agents.list";
pub(super) const GET: &str = "hagency.agents.get";
pub(super) const STATUS: &str = "hagency.agents.status";
pub(super) const WORK: &str = "hagency.agents.work";
pub(super) const WORK_LIST: &str = "hagency.agents.work.list";
pub(super) const REQUEST: &str = "hagency.agents.request";
pub(super) const DECIDE: &str = "hagency.agents.decide";
pub(super) const CANCEL: &str = "hagency.agents.cancel";
pub(super) const SETTLEMENT: &str = "hagency.agents.settlement.get";
pub(super) const SETTLE: &str = "hagency.agents.settlement.request";
pub(super) const REMOVAL: &str = "hagency.agents.removal.get";
pub(super) const REMOVALS: &str = "hagency.agents.removal.list";
pub(super) const REVOKE: &str = "hagency.agents.revoke";
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Project {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct List {
    project_id: Option<String>,
    after: Option<String>,
    #[serde(default)]
    needs_my_action: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkList {
    project_id: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkIndex {
    project_id: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
    #[serde(default)]
    include_pool_agents: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Work {
    id: String,
    after: Option<String>,
    limit: Option<u16>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    command_id: String,
    project_id: String,
    request: AgentRequestInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Decide {
    command_id: String,
    request_id: String,
    expected_revision: i64,
    expected_authority_revision: i64,
    approve: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cancel {
    command_id: String,
    request_id: String,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettlementRequest { command_id: String, request_id: String, expected_request_revision: i64, expected_fence_receipt_digest: String }
pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        READINESS | LIST | GET => &[ReadAgentRequests],
        STATUS => &[ReadAgentRequests, ReadAgentStatus],
        WORK | WORK_LIST => &[ReadAgentWork],
        REMOVAL | REMOVALS => &[ReadAgentCleanup],
        SETTLEMENT => &[ReadAgentSettlement],
        SETTLE => &[RequestAgentSettlement],
        REVOKE => &[RevokeAgentRequests],
        REQUEST => &[RequestAgents],
        DECIDE => &[DecideAgentRequests],
        CANCEL => &[CancelAgentRequests],
        _ => &[],
    }
}
pub(super) fn status_id(service: &str, args: Value) -> Result<Option<String>, String> {
    if service != STATUS {
        return Ok(None);
    }
    let a: Id = parse(args)?;
    if a.id.is_empty()
        || a.id.len() > 128
        || !a
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid_management_arguments".into());
    }
    Ok(Some(a.id))
}
pub(super) fn command(service: &str, args: Value) -> Result<Option<ManagementCommand>, String> {
    Ok(Some(match service {
        READINESS => {
            let a: Project = parse(args)?;
            ManagementCommand::AgentReadiness {
                project_id: a.project_id,
            }
        }
        LIST => {
            let a: List = parse(args)?;
            ManagementCommand::AgentRequests {
                project_id: a.project_id,
                after: a.after,
                needs_my_action: a.needs_my_action,
            }
        }
        GET => {
            let a: Id = parse(args)?;
            ManagementCommand::Agent { id: a.id }
        }
        SETTLEMENT => {let a: Id = parse(args)?; ManagementCommand::AgentSettlement { id: a.id }}
        SETTLE => {let a: SettlementRequest = parse(args)?; ManagementCommand::AgentSettlementRequest { command_id: a.command_id, request_id: a.request_id, expected_request_revision: a.expected_request_revision, expected_fence_receipt_digest: a.expected_fence_receipt_digest }}
        REMOVAL => {
            let a: Id = parse(args)?;
            ManagementCommand::AgentRemoval { id: a.id }
        }
        REMOVALS => {
            let a: WorkList = parse(args)?;
            if a.limit.is_some_and(|n| !(1..=16).contains(&n)) {
                return Err("invalid_management_arguments".into());
            }
            ManagementCommand::AgentRemovals {
                project_id: a.project_id,
                after: a.after,
                limit: a.limit,
            }
        }
        REVOKE => {
            let a: Cancel = parse(args)?;
            ManagementCommand::AgentRevoke {
                command_id: a.command_id,
                request_id: a.request_id,
                expected_revision: a.expected_revision,
            }
        }
        WORK_LIST => {
            let a: WorkIndex = parse(args)?;
            if a.limit.is_some_and(|n| !(1..=16).contains(&n)) {
                return Err("invalid_management_arguments".into());
            }
            ManagementCommand::AgentWorkList {
                include_pool_agents: a.include_pool_agents,
                project_id: a.project_id,
                after: a.after,
                limit: a.limit,
            }
        }
        WORK => {
            let a: Work = parse(args)?;
            if a.limit.is_some_and(|n| !(1..=16).contains(&n)) {
                return Err("invalid_management_arguments".into());
            }
            ManagementCommand::AgentWork {
                id: a.id,
                after: a.after,
                limit: a.limit,
            }
        }
        REQUEST => {
            let a: Request = parse(args)?;
            a.request.validate().map_err(|e| e.code)?;
            ManagementCommand::AgentRequest {
                command_id: a.command_id,
                project_id: a.project_id,
                request: a.request,
            }
        }
        DECIDE => {
            let a: Decide = parse(args)?;
            ManagementCommand::AgentDecide {
                command_id: a.command_id,
                request_id: a.request_id,
                expected_revision: a.expected_revision,
                expected_authority_revision: a.expected_authority_revision,
                approve: a.approve,
            }
        }
        CANCEL => {
            let a: Cancel = parse(args)?;
            ManagementCommand::AgentCancel {
                command_id: a.command_id,
                request_id: a.request_id,
                expected_revision: a.expected_revision,
            }
        }
        _ => return Ok(None),
    }))
}
/// Stable UTC labels are derived from validated observations, not local timezone
/// defaults in the script VM. Null times stay null; no current time is invented.
pub(super) fn display_reply(service: &str, mut value: Value) -> Result<Value, String> {
    if ![WORK, WORK_LIST, REMOVAL, REMOVALS, SETTLEMENT, SETTLE].contains(&service) {
        return Ok(value);
    }
    fn time(value: &Value) -> Result<Value, String> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        let at = value
            .as_i64()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .ok_or_else(|| "management_agent_work_invalid".to_owned())?;
        Ok(Value::String(
            at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ))
    }
    value["observedAtUtc"] = time(&value["observedAtMs"])?;
    if service == REMOVAL {
        if !value["removal"]["matrix"].is_null() {
            value["removal"]["matrix"]["cleanupObservedAtUtc"] =
                time(&value["removal"]["matrix"]["cleanupObservedAtMs"])?;
        }
        return Ok(value);
    }
    if service == WORK_LIST || service == REMOVALS || service == SETTLEMENT || service == SETTLE {
        return Ok(value);
    }
    let rows = value["work"]["items"]
        .as_array_mut()
        .ok_or_else(|| "management_agent_work_invalid".to_owned())?;
    for row in rows {
        for (number, label) in [
            ("createdAtMs", "createdAtUtc"),
            ("startedAtMs", "startedAtUtc"),
            ("completedAtMs", "completedAtUtc"),
        ] {
            row[label] = time(&row[number])?;
        }
    }
    Ok(value)
}
/// Execution is independently observed; older status replies may omit it.
pub(super) fn status_display(mut value: Value) -> Result<Value, String> {
    fn time(value: &Value) -> Result<Value, String> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        let at = value
            .as_i64()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .ok_or_else(|| "management_agent_status_invalid".to_owned())?;
        Ok(Value::String(
            at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ))
    }
    if let Some(stage) = value.get_mut("execution").filter(|s| !s.is_null()) {
        if !stage.is_object() {
            return Err("management_agent_status_invalid".into());
        }
        stage["observedAtUtc"] = time(&stage["observedAtMs"])?;
        if let Some(record) = stage.get_mut("record").filter(|r| !r.is_null()) {
            if !record.is_object() {
                return Err("management_agent_status_invalid".into());
            }
            record["activityExpiresAtUtc"] = time(&record["activityExpiresAtMs"])?;
        }
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn agent_execution_status_preserves_absence_null_readiness_and_separate_observation() {
        let legacy = json!({"runtime":{"record":{"state":"qualified"}}});
        assert_eq!(status_display(legacy.clone()).unwrap(), legacy);
        let input = json!({"execution":{"supported":true,"observedAtMs":1000,"record":{
            "state":"qualified","policy":"codex_text_v1","workAvailable":false,
            "activityExpiresAtMs":null,"capacityReleased":false}},"runtime":{"record":null}});
        let out = status_display(input).unwrap();
        assert_eq!(out["execution"]["observedAtUtc"], "1970-01-01 00:00:01 UTC");
        assert!(out["execution"]["record"]["activityExpiresAtUtc"].is_null());
        assert_eq!(out["execution"]["record"]["workAvailable"], false);
        assert!(out["runtime"]["record"].is_null());
        let out = status_display(
            json!({"execution":{"supported":false,"observedAtMs":null,"record":null}}),
        )
        .unwrap();
        assert!(out["execution"]["observedAtUtc"].is_null());
        for bad in [json!(-1), json!("forged"), json!(9_007_199_254_740_992u64)] {
            assert!(
                status_display(json!({"execution":{"observedAtMs":bad,"record":null}})).is_err()
            );
        }
    }
    #[test]
    fn agent_settlement_host_has_closed_inputs_and_separate_human_permissions() {
        assert_eq!(permissions(SETTLEMENT), &[ManagementPermission::ReadAgentSettlement]);
        assert_eq!(permissions(SETTLE), &[ManagementPermission::RequestAgentSettlement]);
        let input=json!({"commandId":"original","requestId":"request","expectedRequestRevision":3,"expectedFenceReceiptDigest":"d".repeat(64)});
        assert!(matches!(command(SETTLE,input.clone()).unwrap(),Some(ManagementCommand::AgentSettlementRequest {expected_request_revision:3,..})));
        for key in ["token","url","execute","receipt","ownerMxid","refund"] {
            let mut bad=input.clone();bad[key]=json!(true);assert!(command(SETTLE,bad).is_err());
        }
        assert!(command(SETTLEMENT,json!({"id":"request","commandId":"forged"})).is_err());
        let shown=display_reply(SETTLEMENT,json!({"observedAtMs":1000,"settlement":{"knownConsumedTokens":null}})).unwrap();
        assert_eq!(shown["observedAtUtc"],"1970-01-01 00:00:01 UTC");
        assert!(shown["settlement"]["knownConsumedTokens"].is_null());
    }
    #[test]
    fn agent_cleanup_host_keeps_read_revoke_permissions_and_nullable_observations_separate() {
        assert_eq!(
            permissions(REMOVAL),
            &[ManagementPermission::ReadAgentCleanup]
        );
        assert_eq!(
            permissions(REVOKE),
            &[ManagementPermission::RevokeAgentRequests]
        );
        assert_eq!(
            permissions(CANCEL),
            &[ManagementPermission::CancelAgentRequests]
        );
        assert!(matches!(
            command(
                REMOVALS,
                json!({"projectId":"project","after":"request","limit":16})
            )
            .unwrap(),
            Some(ManagementCommand::AgentRemovals { .. })
        ));
        assert!(matches!(
            command(
                REVOKE,
                json!({"commandId":"original","requestId":"request","expectedRevision":7})
            )
            .unwrap(),
            Some(ManagementCommand::AgentRevoke {
                expected_revision: 7,
                ..
            })
        ));
        for bad in [
            json!({"id":"request","receipt":"forged"}),
            json!({"id":"request","url":"https://elsewhere"}),
            json!({"id":"request","execute":true}),
        ] {
            assert!(command(REMOVAL, bad).is_err());
        }
        for bad in [
            json!({"limit":17}),
            json!({"limit":0}),
            json!({"administratorMxid":"@forged:test"}),
        ] {
            assert!(command(REMOVALS, bad).is_err());
        }
        assert!(command(REVOKE,json!({"commandId":"original","requestId":"request","expectedRevision":7,"refund":true})).is_err());
        let observed=display_reply(REMOVAL,json!({"observedAtMs":1000,"removal":{"matrix":{"cleanupObservedAtMs":null},"usage":{"knownConsumedTokens":null,"retainedTokensFloor":100,"fenceUsageComplete":null}}})).unwrap();
        assert_eq!(observed["observedAtUtc"], "1970-01-01 00:00:01 UTC");
        assert_eq!(
            observed["removal"]["matrix"]["cleanupObservedAtUtc"],
            Value::Null
        );
        assert_eq!(
            observed["removal"]["usage"]["knownConsumedTokens"],
            Value::Null
        );
        assert_eq!(observed["removal"]["usage"]["retainedTokensFloor"], 100);
    }
    #[test]
    fn agent_work_host_input_and_dates_are_bounded_without_execution_permission() {
        assert!(matches!(
            command(WORK, json!({"id":"request","after":"work_a","limit":16})).unwrap(),
            Some(ManagementCommand::AgentWork { .. })
        ));
        for bad in [
            json!({"id":"request","limit":0}),
            json!({"id":"request","limit":17}),
            json!({"id":"request","limit":1.5}),
            json!({"id":"request","prompt":"private"}),
            json!({"id":"request","token":"forged"}),
            json!({"id":"request","url":"https://other"}),
            json!({"id":"request","commandId":"effect"}),
        ] {
            assert!(command(WORK, bad).is_err());
        }
        assert_eq!(permissions(WORK), &[ManagementPermission::ReadAgentWork]);
        assert_eq!(
            permissions(WORK_LIST),
            &[ManagementPermission::ReadAgentWork]
        );
        assert!(matches!(
            command(
                WORK_LIST,
                json!({"projectId":"project","after":"request_a","limit":16})
            )
            .unwrap(),
            Some(ManagementCommand::AgentWorkList { .. })
        ));
        for bad in [
            json!({"projectId":"project","administratorMxid":"@forged:test"}),
            json!({"projectId":"project","commandId":"effect"}),
            json!({"limit":17}),
            json!({"limit":0}),
        ] {
            assert!(command(WORK_LIST, bad).is_err());
        }
        assert_eq!(
            display_reply(
                WORK_LIST,
                json!({"observedAtMs":1000,"requests":{"items":[],"nextAfter":null}})
            )
            .unwrap()["observedAtUtc"],
            "1970-01-01 00:00:01 UTC"
        );
        let source = json!({"observedAtMs":1000,"work":{"items":[{"createdAtMs":1000,"startedAtMs":null,"completedAtMs":2000,"usage":null}]}});
        let display = display_reply(WORK, source).unwrap();
        assert_eq!(display["observedAtUtc"], "1970-01-01 00:00:01 UTC");
        assert_eq!(display["work"]["items"][0]["startedAtUtc"], Value::Null);
        assert_eq!(display["work"]["items"][0]["usage"], Value::Null);
        assert_eq!(
            display_reply(STATUS, json!({"observedAtMs":1000})).unwrap(),
            json!({"observedAtMs":1000})
        );
    }
    #[test]
    fn agent_admission_host_input_is_closed_and_permissions_are_distinct() {
        let input = json!({"commandId":"original","projectId":"project","request":{"expectedSetupRevision":2,"expectedSetupBindingRevision":1,"expectedAuthorityRevision":1,"expectedAllocationRevision":3,"definition":{"name":"Writer","capability":"documentation","description":"Draft text","toolMode":"none"},"requestedTokens":100}});
        assert!(matches!(
            command(REQUEST, input.clone()).unwrap(),
            Some(ManagementCommand::AgentRequest { .. })
        ));
        for key in [
            "ownerMxid",
            "administratorMxid",
            "enrollmentId",
            "url",
            "token",
            "execute",
            "autoApprove",
        ] {
            let mut bad = input.clone();
            bad[key] = json!("forged");
            assert!(command(REQUEST, bad).is_err());
        }
        for key in ["runtime", "roomId", "nativeAvailableTokens"] {
            let mut bad = input.clone();
            bad["request"][key] = json!("forged");
            assert!(command(REQUEST, bad).is_err());
        }
        let mut tool = input;
        tool["request"]["definition"]["toolMode"] = json!("shell");
        assert!(command(REQUEST, tool).is_err());
        assert_eq!(
            permissions(DECIDE),
            [ManagementPermission::DecideAgentRequests]
        );
        assert_ne!(permissions(DECIDE), [ManagementPermission::DecideProjects]);
        assert_ne!(permissions(DECIDE), [ManagementPermission::DecideInbox]);
        assert!(permissions("hagency.agents.execute").is_empty());
        assert_eq!(
            permissions(STATUS),
            [
                ManagementPermission::ReadAgentRequests,
                ManagementPermission::ReadAgentStatus
            ]
        );
        assert_eq!(
            status_id(STATUS, json!({"id":"request"})).unwrap(),
            Some("request".into())
        );
        for input in [
            json!({"id":"../request"}),
            json!({"id":"request","url":"https://other"}),
            json!({"id":"request","runtime":true}),
        ] {
            assert!(status_id(STATUS, input).is_err());
        }
        let manifest: Value =
            serde_json::from_str(include_str!("../../../apps/hagency/bundle/manifest.json"))
                .unwrap();
        let shipped = manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|service| service.starts_with("hagency.agents."))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            shipped,
            std::collections::BTreeSet::from([
                LIST,
                GET,
                STATUS,
                WORK,
                WORK_LIST,
                SETTLEMENT,
                SETTLE,
                REMOVAL,
                REMOVALS,
                super::super::interrupted_calls::LIST,
                super::super::interrupted_calls::RECONCILE,
                super::super::control::READINESS,
                super::super::control::SET,
                super::super::budget::GET,
                super::super::budget::LIST,
                super::super::budget::TOP_UP,
                super::super::budget::REQUEST,
                super::super::budget::DECIDE,
                super::super::budget::CANCEL,
            ])
        );
        // The current pool flow has no legacy project-admission entrypoint.
        for legacy in [READINESS, REQUEST, DECIDE, CANCEL, REVOKE] {
            assert!(!shipped.contains(legacy), "unused grant: {legacy}");
        }
        for service in [
            READINESS,
            LIST,
            GET,
            STATUS,
            WORK,
            WORK_LIST,
            REQUEST,
            DECIDE,
            CANCEL,
            SETTLEMENT,
            SETTLE,
            REMOVAL,
            REMOVALS,
            REVOKE,
            super::super::control::READINESS,
            super::super::control::SET,
            super::super::budget::GET,
            super::super::budget::LIST,
            super::super::budget::TOP_UP,
            super::super::budget::REQUEST,
            super::super::budget::DECIDE,
            super::super::budget::CANCEL,
        ] {
            assert!(octosense_app_contract::hagency::SERVICES.contains(&service));
            assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&service));
            assert!(octosense_app_contract::hagency::words(service).is_some());
        }
    }
}
