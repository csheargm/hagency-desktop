use hagency_store::{DomainRepository, DomainStore, Error, management::*};
use serde_json::json;
use std::time::{Duration, Instant};
fn context(owner: &str) -> HostContext {
    HostContext::new(owner.into(), 1, "a".repeat(64), 1, "device".into(), 1).unwrap()
}

fn bytes(path: &std::path::Path) -> (Vec<u8>, Option<Vec<u8>>) {
    (
        std::fs::read(path.join("domain.sqlite3")).unwrap(),
        std::fs::read(path.join("domain.sqlite3-wal")).ok(),
    )
}



#[test]
fn operator_read_inputs_reject_path_raw_command_and_unbounded_pages() {
    assert!(serde_json::from_value::<OperatorTaskQuery>(json!({"path":"foreign"})).is_err());
    assert!(
        serde_json::from_value::<OperatorTaskDetailQuery>(
            json!({"id":"original","command":"delete"})
        )
        .is_err()
    );
    assert!(
        OperatorTaskQuery {
            offset: 100001,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        OperatorTaskQuery {
            status: Some("running".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        OperatorBoardQuery {
            agent_after: Some("agent".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}
