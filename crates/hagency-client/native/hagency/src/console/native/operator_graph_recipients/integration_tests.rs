use super::*;
use crate::console::native::operator_graph_recipients::{
    OperatorGraphRecipientObserve, OperatorGraphBindRecipient, OperatorGraphRecipientOptionsQuery,
};
fn recipient_package(instance: u64) -> ManagementPackage {
    let mut p = package(instance);
    p.permissions.extend([
        Permission::ReadOperatorGraphRecipients,
        Permission::BindOperatorGraphRecipients,
        Permission::ReadProjectSetup,
        Permission::ReadProjects,
        Permission::ReadAgentRequests,
        Permission::ReadAgentStatus,
    ]);
    p
}
fn route(f: &Fixture, path: &str, value: Value) {
    f.recipient_routes
        .lock()
        .unwrap()
        .insert(path.into(), value);
}
fn personal_agent(generation: u64) -> Value {
    json!({"agent":{"id":"personal_1","ownerUserId":"uid","puppetMxid":"@personal:test","displayName":"Recorder","state":"active","generation":generation,"executionDeviceId":null,"ownerDirectRoomId":"!room:test"}})
}
fn shared_routes(f: &Fixture, manager: &str) {
    let definition = json!({"version":1,"setupId":"setup_1","projectId":"project_1","projectRevision":1,"allocationId":"allocation_1","allocationReceiptDigest":"d".repeat(64),"managerMxid":manager,"administratorMxid":"@owner:test","authorityRevision":1,"name":"Shared project","mode":"create"});
    let digest = hagency_core::canonical::digest(&definition).unwrap();
    let setup = json!({"id":"setup_1","projectId":"project_1","allocationId":"allocation_1","allocationReceiptDigest":"d".repeat(64),"projectRevision":1,"authorityRevision":1,"definitionDigest":digest,"mode":"create","state":"ready","revision":2,"name":"Shared project","managerMxid":manager,"administratorMxid":"@owner:test","spaceCreationCommandId":"space_cmd","roomCreationCommandId":"room_cmd","spaceId":"!space:test","roomId":"!room:test","domainProjectId":"domain_project","verifiedAtMs":1,"currentBinding":{"revision":1,"allocationId":"allocation_1","allocationReceiptDigest":"d".repeat(64),"projectRevision":1,"authorityRevision":1,"administratorMxid":"@owner:test"}});
    route(
        f,
        "/hagency/api/v1/management/projects/project_1/setup",
        json!({"readiness":{"projectId":"project_1","administrator":{"mxid":"@owner:test","revision":1},"setup":setup,"eligibleAllocation":{"id":"allocation_1","revision":1,"projectRevision":1,"receiptDigest":"d".repeat(64)},"capacityReady":true,"matrixReady":true,"canAssignAdministrator":false,"canBegin":false,"canConfirm":false,"canCancel":false,"canRebase":false,"blockingReason":null,"observedAtMs":1},"observedAtMs":1}),
    );
    let request = json!({"id":"request_1","projectId":"project_1","proposedAgentId":"shared_1","managerMxid":manager,"administratorMxid":"@owner:test","authorityRevision":1,"definition":{"name":"Shared name","capability":"coding","description":"Inspect","toolMode":"none"},"definitionDigest":"a".repeat(64),"requestedTokens":100,"periodKey":"2026-10","state":"approved","revision":2,"createdAtMs":1,"expiresAtMs":1000,"canDecide":false,"canApprove":false,"canReject":false,"canCancel":false,"blockingReason":null,"executionAvailable":false});
    route(
        f,
        "/hagency/api/v1/management/agent-requests/request_1",
        json!({"agentRequest":request,"observedAtMs":1}),
    );
    route(
        f,
        "/hagency/api/v1/management/agent-requests/list",
        json!({"protocol":"hagency-management-v1","items":[request],"nextAfter":null,"observedAtMs":1}),
    );
    route(
        f,
        "/hagency/api/v1/management/agent-requests/request_1/assignment",
        json!({"assignment":{"id":"assignment_1","requestId":"request_1","agentId":"shared_1","state":"pending_native","generation":1,"executionAvailable":false},"observedAtMs":1}),
    );
    route(
        f,
        "/hagency/api/v1/management/agent-requests/request_1/matrix",
        json!({"membership":{"assignmentId":"assignment_1","requestId":"request_1","agentId":"shared_1","puppetMxid":"@personal:test","roomId":"!room:test","state":"matrix_verified","verifiedAtMs":1,"cleanupObservedAtMs":null,"blockingReason":null,"workAvailable":false},"observedAtMs":1}),
    );
    route(
        f,
        "/hagency/api/v1/management/agent-requests/request_1/runtime",
        json!({"runtime":null,"observedAtMs":1}),
    );
    route(
        f,
        "/hagency/api/v1/management/agent-requests/request_1/execution",
        json!({"execution":null,"observedAtMs":1}),
    );
}
fn query(id: &str) -> OperatorGraphRecipientObserve {
    serde_json::from_value(json!({"id":id,"nodeId":"root","target":{"kind":"personal_agent","agentId":"personal_1","bindingId":"binding_1"}})).unwrap()
}
fn bind(observed: &Value) -> OperatorGraphBindRecipient {
    serde_json::from_value(json!({"id":observed["graphId"],"nodeId":observed["nodeId"],"expectedRevision":observed["revision"],"target":observed["target"],"observationDigest":observed["observationDigest"]})).unwrap()
}
fn private_outbox(f: &Fixture) -> Vec<(String, Vec<u8>)> {
    let db = rusqlite::Connection::open_with_flags(
        f.temp.path().join("canonical/domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db
        .prepare("SELECT state, payload FROM owner_operator_graph_outbox ORDER BY message_id")
        .unwrap();
    statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
#[test]
fn native_graph_recipient_restart_reobservation_and_no_dispatch() {
    let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["console::native::operator_graph_commands::tests::recipient_tests::native_graph_recipient_process_child","--exact","--nocapture"]).env("HAGENCY_GRAPH_RECIPIENT_FIXTURE","1").output().unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[tokio::test]
async fn native_graph_recipient_process_child() {
    if std::env::var("HAGENCY_GRAPH_RECIPIENT_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let (f, registry) = adopted().await;
    route(&f, "/hagency/api/v1/agents/personal_1", personal_agent(1));
    route(
        &f,
        "/hagency/api/v1/agents/personal_1/bindings",
        json!({"bindings":[{"id":"binding_1","agentId":"personal_1","projectId":null,"scopeKind":"owner_direct","roomId":"!room:test","state":"active","generation":1,"ownerServicePaused":false,"threadAutoReply":true}]}),
    );
    route(
        &f,
        "/_matrix/client/v3/rooms/!room:test/state",
        json!([
            {"type":"m.room.create","state_key":"","event_id":"$create","sender":"@owner:test","content":{"creator":"@owner:test"}},
            {"type":"m.room.member","state_key":"@owner:test","event_id":"$owner","content":{"membership":"join"}},
            {"type":"m.room.member","state_key":"@personal:test","event_id":"$agent","content":{"membership":"join"}},
            {"type":"m.room.member","state_key":"@human:test","event_id":"$human","content":{"membership":"join"}},
            {"type":"m.room.power_levels","state_key":"","event_id":"$power","content":{"users":{"@owner:test":50},"events_default":0}}
        ]),
    );
    route(
        &f,
        "/hagency/api/v1/projects",
        json!({"projects":[{"id":"project_1","spaceId":"!space:test","active":true,"creationPolicy":"members","revision":1}]}),
    );
    route(
        &f,
        "/hagency/api/v1/projects/project_1/rooms",
        json!({"rooms":[{"roomId":"!room:test","projectId":"project_1","active":true,"creationPolicy":"members","revision":1}]}),
    );
    let manager = f.owner.management(recipient_package(2)).await.unwrap();
    let prepared=manager.prepare_operator_graph_create(serde_json::from_value(json!({"label":"Recipient original","nodes":{"root":{"assignee":"Recorded label only","description":"Inspect"}}})).unwrap()).await.unwrap();
    let graph = prepared.graph_id.clone();
    manager
        .apply_saved_operator_graph(prepared.command_id)
        .await
        .unwrap();
    let before = private_outbox(&f);
    let observed = manager
        .operator_graph_recipient_observe(query(&graph))
        .await
        .unwrap();
    assert_eq!(observed["canBind"], true);
    assert_eq!(observed["runtimeDeliveryAuthorized"], false);
    let saved = manager
        .prepare_operator_graph_bind_recipient(bind(&observed))
        .await
        .unwrap();
    assert_eq!(saved.operation, IntentOperation::GraphBindRecipient);
    assert_eq!(private_outbox(&f), before);
    let review = manager
        .review_saved_operator_graph(saved.command_id.clone())
        .await
        .unwrap();
    assert_eq!(review.change_json, saved.change_json);
    let receipt = manager
        .apply_saved_operator_graph(saved.command_id.clone())
        .await
        .unwrap();
    assert_eq!(receipt.revision, 2);
    assert_eq!(receipt.delivery_state, "binding_saved");
    assert_eq!(receipt.queued_messages, 0);
    assert_eq!(private_outbox(&f), before);
    let get = manager
        .operator_graph_recipient_get(OperatorGraphNodeQuery {
            id: graph.clone(),
            node_id: "root".into(),
        })
        .await
        .unwrap();
    assert_eq!(get["state"], "binding_saved");
    let observed = manager
        .operator_graph_recipient_observe(query(&graph))
        .await
        .unwrap();
    let stale = manager
        .prepare_operator_graph_bind_recipient(bind(&observed))
        .await
        .unwrap();
    route(&f, "/hagency/api/v1/agents/personal_1", personal_agent(2));
    assert_eq!(
        manager
            .apply_saved_operator_graph(stale.command_id.clone())
            .await
            .unwrap_err()
            .code,
        "management_recipient_changed"
    );
    assert_eq!(
        manager
            .operator_graph_recipient_get(OperatorGraphNodeQuery {
                id: graph.clone(),
                node_id: "root".into()
            })
            .await
            .unwrap()["state"],
        "binding_stale"
    );
    manager.acknowledge_command(stale.command_id).await.unwrap();
    let observed = manager
        .operator_graph_recipient_observe(query(&graph))
        .await
        .unwrap();
    let prepared = manager
        .prepare_operator_graph_bind_recipient(bind(&observed))
        .await
        .unwrap();
    let original: OperatorGraphCommand = serde_json::from_value(
        manager
            .saved_commands()
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.command_id == prepared.command_id)
            .unwrap()
            .input,
    )
    .unwrap();
    let (context, session) = manager.authorized().await.unwrap();
    let (native_registry, profile) = session
        .operator_graph_access(&context, Permission::BindOperatorGraphRecipients)
        .unwrap();
    let journal =
        OperatorGraphJournal::new(native_registry, profile, OPERATOR_GRAPH_SCOPE.into()).unwrap();
    let committed = journal
        .apply(
            original.command_id.clone(),
            original.graph_id.clone(),
            original.expected_revision,
            original.change.clone(),
        )
        .await
        .unwrap();
    assert_eq!(committed.revision, 3);
    route(&f, "/hagency/api/v1/agents/personal_1", personal_agent(3));
    manager.retire();
    drop(manager);
    drop(journal);
    drop(session);
    let f = f.restart_current(registry, "current_fixture".into()).await;
    let manager = f.owner.management(recipient_package(3)).await.unwrap();
    // Original commit survives restart and remote drift. No rebind/no resend.
    let recovered = manager
        .retry_saved_command(prepared.command_id.clone())
        .await
        .unwrap();
    assert_eq!(recovered["operation"], "graph_bind_recipient");
    assert_eq!(recovered["data"]["revision"], 3);
    assert_eq!(private_outbox(&f), before);
    let human=manager.operator_graph_recipient_observe(serde_json::from_value(json!({"id":graph,"nodeId":"root","target":{"kind":"human","projectId":"project_1","roomId":"!room:test","userId":"@human:test"}})).unwrap()).await.unwrap();
    assert_eq!(human["canBind"], true);
    let options=manager.operator_graph_recipient_options(serde_json::from_value::<OperatorGraphRecipientOptionsQuery>(json!({"id":graph,"nodeId":"root","kind":"human","projectId":"project_1","roomId":"!room:test"})).unwrap()).await.unwrap();
    assert_eq!(options["items"].as_array().unwrap().len(), 3);
    assert_eq!(options["runtimeDeliveryAuthorized"], false);
    shared_routes(&f, "@owner:test");
    let shared_query:OperatorGraphRecipientObserve=serde_json::from_value(json!({"id":graph,"nodeId":"root","target":{"kind":"shared_agent","projectId":"project_1","agentRequestId":"request_1"}})).unwrap();
    let shared = manager
        .operator_graph_recipient_observe(shared_query.clone())
        .await
        .unwrap();
    assert_eq!(shared["canBind"], true);
    let choices = manager
        .operator_graph_recipient_options(
            serde_json::from_value(
                json!({"id":graph,"nodeId":"root","kind":"shared_agent","projectId":"project_1"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        choices["items"][0]["target"],
        serde_json::to_value(&shared_query.target).unwrap()
    );
    shared_routes(&f, "@other_manager:test");
    let admin = manager
        .operator_graph_recipient_observe(shared_query)
        .await
        .unwrap();
    assert_eq!(admin["canBind"], false);
    assert!(
        admin["blockingReasons"]
            .as_array()
            .unwrap()
            .contains(&json!("shared_project_manager_required"))
    );
    let mut readonly = recipient_package(4);
    readonly
        .permissions
        .retain(|p| *p != Permission::BindOperatorGraphRecipients);
    let readonly = f.owner.management(readonly).await.unwrap();
    assert!(
        readonly
            .review_saved_operator_graph(prepared.command_id.clone())
            .await
            .is_err()
    );
    assert!(
        readonly
            .apply_saved_operator_graph(prepared.command_id.clone())
            .await
            .is_err()
    );
    f.live.store(false, Ordering::Release);
    assert!(
        manager
            .retry_saved_command(prepared.command_id)
            .await
            .is_err()
    );
    assert!(
        manager
            .operator_graph_recipient_observe(query(&graph))
            .await
            .is_err()
    );
    f.live.store(true, Ordering::Release);
    assert_eq!(counts(&f).3, 0);
    let requests = f.recipient_requests.lock().unwrap().clone();
    assert!(
        requests.iter().all(|r| !r.contains("/send/")
            && !r.contains("createRoom")
            && !r.contains("/start")
            && !r.contains("/work")),
        "binding must not dispatch or invoke providers"
    );
    f.stop().await;
}
