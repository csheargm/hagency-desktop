use super::*;
use hagency_store::management::{
    AgentExecutionPolicy, EditExecutionPolicy, ExecutionPolicyQuery, ExecutionPolicyValue,
    HostContext, Permission, Session,
};
use std::time::{Duration, Instant};

fn context() -> HostContext {
    HostContext::new(
        "@owner:example.test".into(),
        1,
        "a".repeat(64),
        1,
        "policy_device".into(),
        1,
    )
    .unwrap()
}

fn query(agent: &str) -> ExecutionPolicyQuery {
    ExecutionPolicyQuery {
        resource_id: resource("pool", "seat", 1000).id(),
        engagement_id: agent.into(),
    }
}
fn edit(current: &AgentExecutionPolicy, id: &str, yolo: bool) -> EditExecutionPolicy {
    EditExecutionPolicy {
        web_search: None,
        command_id: id.into(),
        resource_id: current.resource_id.clone(),
        engagement_id: current.engagement_id.clone(),
        expected_revision: current.revision.clone(),
        execution_policy: ExecutionPolicyValue { yolo },
    }
}







#[test]
fn management_execution_policy_input_rejects_extra_authority_and_nonboolean_mode() {
    for value in [
        json!({"yolo":1}),
        json!({"yolo":true,"owner":"@other:example.test"}),
    ] {
        assert!(serde_json::from_value::<ExecutionPolicyValue>(value).is_err());
    }
    assert!(
        serde_json::from_value::<ExecutionPolicyQuery>(
            json!({"resourceId":"r","engagementId":"e","path":"/tmp/other"})
        )
        .is_err()
    );
}
