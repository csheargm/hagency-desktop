use super::*;
use hagency_store::management::{
    ClearWorkspaceDirty, HostContext, Permission, Session, WorkspaceListQuery, WorkspaceQuery,
    WorkspaceReleaseBlock, WorkspaceStatus,
};
use std::time::{Duration, Instant};

fn context() -> HostContext {
    HostContext::new(
        "@owner:example.test".into(),
        1,
        "a".repeat(64),
        1,
        "workspace_device".into(),
        1,
    )
    .unwrap()
}

fn query() -> WorkspaceQuery {
    WorkspaceQuery {
        resource_id: resource("pool", "seat", 1000).id(),
        workspace_id: "workspace_a".into(),
    }
}
fn clear(row: &WorkspaceStatus, command: &str) -> ClearWorkspaceDirty {
    ClearWorkspaceDirty {
        command_id: command.into(),
        resource_id: row.resource_id.clone(),
        workspace_id: row.workspace_id.clone(),
        expected_revision: row.revision.clone(),
    }
}
fn idle() -> Fixture {
    let mut f = Fixture::new(true);
    for cap in &f.caps {
        f.db.complete_dispatch(cap, &json!({"result":"done"}), 1020)
            .unwrap();
    }
    f.sql()
        .execute("UPDATE workspace_resources SET dirty=1", [])
        .unwrap();
    f
}
fn dirty(sql: &rusqlite::Connection) -> bool {
    sql.query_row(
        "SELECT dirty FROM workspace_resources WHERE id='workspace_a'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}











#[test]
fn workspace_inputs_reject_paths_and_unknown_fields() {
    assert!(
        serde_json::from_value::<WorkspaceQuery>(
            json!({"resourceId":"r","workspaceId":"w","owner":"@other:test"})
        )
        .is_err()
    );
    let mut q = query();
    q.workspace_id = "/tmp/owner".into();
    assert!(q.validate().is_err());
    let input = ClearWorkspaceDirty {
        command_id: "command".into(),
        resource_id: "r".into(),
        workspace_id: "w".into(),
        expected_revision: "A".repeat(64),
    };
    assert!(input.validate().is_err());
}
