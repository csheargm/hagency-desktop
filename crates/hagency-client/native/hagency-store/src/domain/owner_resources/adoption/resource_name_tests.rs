use super::*;
use crate::management::{
    HostContext, Permission, PrivateManagementJournal, RenameResource, Session,
};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "name_device".into(),
        1,
    )
    .unwrap()
}
fn session(store: AdoptedManagementStore, context: HostContext) -> Session {
    Session::new_adopted(
        store,
        context,
        [Permission::ReadResources, Permission::RenameResources],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
pub(super) async fn active(f: &Fixture) -> AdoptedManagementStore {
    prepare(f, "name_transfer").await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(f), "name_transfer".into())
        .await
        .unwrap();
    fenced_connections(f, &transfer);
    f.registry
        .record_adoption_sources(authority(f), archive_proof(f, &transfer).unwrap())
        .await
        .unwrap();
    f.registry
        .import_resource_adoption(authority(f), archive_proof(f, &transfer).unwrap())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    f.registry
        .record_adoption_custody(
            authority(f),
            transfer.verify_private_journal(&journal).await.unwrap(),
        )
        .await
        .unwrap();
    f.registry
        .activate_resource_adoption(
            authority(f),
            transfer.verify_private_journal(&journal).await.unwrap(),
            provider(),
        )
        .await
        .unwrap();
    transfer.adopted_management(journal).await.unwrap()
}
fn input(f: &Fixture, command: &str, revision: u64, name: &str) -> RenameResource {
    RenameResource {
        command_id: command.into(),
        resource_id: f.resource.id(),
        expected_name_revision: revision,
        name: name.into(),
    }
}
async fn counts(f: &Fixture) -> (u64, u64) {
    f.registry.0.store.call(1,|db|Ok((
        db.db.query_row("SELECT count(*) FROM owner_resource_display_names",[],|r|r.get(0))?,
        db.db.query_row("SELECT count(*) FROM management_commands WHERE operation='resource.local_name'",[],|r|r.get(0))?
    ))).await.unwrap()
}
#[tokio::test]
async fn local_resource_name_original_receipt_and_effective_identity_are_separate() {
    let mut f = fixture().await;
    let backing = active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let canonical = serde_json::to_value(
        f.registry
            .resource(&f.profile, f.resource.id())
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(canonical.get("localName").is_none());
    let first = input(&f, "name_first", 0, "写作资源");
    let result = s.rename_resource(&ctx, first.clone()).await.unwrap();
    assert_eq!(result.local_name.revision, 1);
    let detail = s
        .resource_detail(
            &ctx,
            crate::management::ResourceDetailQuery {
                id: f.resource.id(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        detail.resource.local_name.as_ref().unwrap().name,
        "写作资源"
    );
    assert!(!detail.editing.allowed);
    let mut projected = serde_json::to_value(detail.resource).unwrap();
    projected.as_object_mut().unwrap().remove("localName");
    assert_eq!(projected, canonical);
    assert_eq!(
        serde_json::to_value(
            f.registry
                .resource(&f.profile, f.resource.id())
                .await
                .unwrap()
        )
        .unwrap(),
        canonical
    );
    let query = crate::management::ResourceQuery {
        after: String::new(),
        limit: 16,
        framework: None,
        capability: None,
        published: None,
    };
    assert_eq!(
        s.resources(&ctx, query).await.unwrap().resources[0]
            .local_name
            .as_ref()
            .unwrap()
            .name,
        "写作资源"
    );
    assert_eq!(
        s.rename_resource(&ctx, input(&f, "name_second", 1, "Research"))
            .await
            .unwrap()
            .local_name
            .revision,
        2
    );
    // A lost reply/original request recovers its exact original result, never
    // overwrites a newer name or fabricates the current projection.
    assert_eq!(
        s.rename_resource(&ctx, first.clone()).await.unwrap(),
        result
    );
    assert_eq!(
        s.resource(&ctx, f.resource.id())
            .await
            .unwrap()
            .local_name
            .unwrap()
            .name,
        "Research"
    );
    let mut changed = first;
    changed.name = "different".into();
    assert!(matches!(
        s.rename_resource(&ctx, changed).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        s.rename_resource(&ctx, input(&f, "stale", 1, "Other"))
            .await,
        Err(Error::ResourceNameRevisionChanged)
    ));
    assert_eq!(counts(&f).await, (1, 2));
    assert_eq!(
        s.rename_resource(&ctx, input(&f, "clear", 2, ""))
            .await
            .unwrap()
            .local_name
            .revision,
        3
    );
    assert_eq!(
        s.resource(&ctx, f.resource.id())
            .await
            .unwrap()
            .local_name
            .unwrap()
            .name,
        ""
    );
    s.retire();
    drop(s);
    let fresh = session(backing.clone(), ctx.clone());
    assert_eq!(
        fresh
            .rename_resource(&ctx, input(&f, "name_first", 0, "写作资源"))
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        fresh
            .resource(&ctx, f.resource.id())
            .await
            .unwrap()
            .local_name
            .unwrap()
            .revision,
        3
    );
    fresh.retire();
    drop(fresh);
    backing.shutdown().await.unwrap();
    drop(backing);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let reopened = transfer.adopted_management(journal).await.unwrap();
    let s = session(reopened.clone(), ctx.clone());
    assert_eq!(
        s.rename_resource(&ctx, input(&f, "name_first", 0, "写作资源"))
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        s.resource(&ctx, f.resource.id())
            .await
            .unwrap()
            .local_name
            .unwrap()
            .revision,
        3
    );
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn local_resource_name_cas_concurrency_rollback_and_scope_checks() {
    let f = fixture().await;
    let backing = active(&f).await;
    let ctx = context(&f);
    let a = session(backing.clone(), ctx.clone());
    let b = session(backing.clone(), ctx.clone());
    let (a_result, b_result) = tokio::join!(
        a.rename_resource(&ctx, input(&f, "concurrent_a", 0, "A")),
        b.rename_resource(&ctx, input(&f, "concurrent_b", 0, "B"))
    );
    assert_eq!(
        usize::from(a_result.is_ok()) + usize::from(b_result.is_ok()),
        1
    );
    assert_eq!(counts(&f).await, (1, 1));
    let badctx = HostContext::new(
        f.profile.account.clone(),
        2,
        "a".repeat(64),
        1,
        "name_device".into(),
        1,
    )
    .unwrap();
    assert!(matches!(
        a.rename_resource(&badctx, input(&f, "wrong_scope", 1, "X"))
            .await,
        Err(Error::LocalAuthority)
    ));
    let readonly = Session::new_adopted(
        backing.clone(),
        ctx.clone(),
        [Permission::ReadResources],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(matches!(
        readonly
            .rename_resource(&ctx, input(&f, "no_permission", 1, "X"))
            .await,
        Err(Error::LocalAuthority)
    ));
    f.registry.0.store.call(1,|db| {db.db.execute_batch("CREATE TRIGGER fail_name_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='resource.local_name' BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")?;Ok(())}).await.unwrap();
    assert!(
        a.rename_resource(&ctx, input(&f, "rollback", 1, "Rollback"))
            .await
            .is_err()
    );
    assert_eq!(counts(&f).await, (1, 1));
    assert_eq!(
        a.resource(&ctx, f.resource.id())
            .await
            .unwrap()
            .local_name
            .unwrap()
            .revision,
        1
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db.execute_batch("DROP TRIGGER fail_name_receipt")?;
            Ok(())
        })
        .await
        .unwrap();
    let result = a
        .rename_resource(&ctx, input(&f, "rollback", 1, "Recovered"))
        .await
        .unwrap();
    assert_eq!(result.local_name.revision, 2);
    a.retire();
    assert!(matches!(
        a.rename_resource(&ctx, input(&f, "rollback", 1, "Recovered"))
            .await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn local_resource_name_queued_retirement_has_no_effect() {
    let f = fixture().await;
    let backing = active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let (started_rx_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = f.registry.0.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .call(1, move |_| {
                started_rx_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    let request = s.rename_resource(&ctx, input(&f, "queued", 0, "No write"));
    tokio::pin!(request);
    tokio::select! {biased; r=&mut request=>panic!("writer unexpectedly finished: {r:?}"), _=tokio::task::yield_now()=>{}}
    s.retire();
    release_tx.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(request.await.is_err());
    assert_eq!(counts(&f).await, (0, 0));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[test]
fn local_resource_name_input_is_closed_and_bounded() {
    let base = RenameResource {
        command_id: "original".into(),
        resource_id: "resource".into(),
        expected_name_revision: 0,
        name: "中文😀".repeat(20),
    };
    assert!(base.validate().is_ok());
    for name in [
        "x".repeat(81),
        " x".into(),
        "x\nvalue".into(),
        "x\u{2028}y".into(),
    ] {
        let mut input = base.clone();
        input.name = name;
        assert!(input.validate().is_err());
    }
    let mut v = serde_json::to_value(base).unwrap();
    v["profile"] = json!("forged");
    assert!(serde_json::from_value::<RenameResource>(v).is_err());
}
#[tokio::test]
async fn local_resource_name_source_metadata_blocks_adoption() {
    let f = fixture().await;
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.execute(
        "INSERT INTO owner_resource_display_names VALUES(?1,?2,'Original local label',1)",
        params![f.profile.digest, f.resource.id()],
    )
    .unwrap();
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    drop(d);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_resource_name_schema83_migration_rolls_back_without_changing_resources() {
    let mut f = fixture().await;
    let backing = active(&f).await;
    let before = serde_json::to_value(
        f.registry
            .resource(&f.profile, f.resource.id())
            .await
            .unwrap(),
    )
    .unwrap();
    let registry_id = f.registry.id().to_owned();
    backing.shutdown().await.unwrap();
    drop(backing);
    f.registry.shutdown().await.unwrap();
    let path = f._root.path().join("owner/domain.sqlite3");
    let db = Connection::open(&path).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE owner_resource_display_names;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=82;CREATE TABLE owner_resource_display_names(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._root.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        82
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    db.execute_batch("DROP TABLE owner_resource_display_names;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    assert_eq!(f.registry.id(), registry_id);
    assert_eq!(
        serde_json::to_value(
            f.registry
                .resource(&f.profile, f.resource.id())
                .await
                .unwrap()
        )
        .unwrap(),
        before
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))?,
                crate::domain::DOMAIN_SCHEMA_VERSION as u64
            );
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM owner_resource_display_names",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[path = "operator_task_tests.rs"]
mod operator_task_tests;

#[path = "operator_tasks_canonical_tests.rs"]
mod operator_tasks_canonical_tests;

#[path = "operator_task_command_tests.rs"]
mod operator_task_command_tests;

#[path = "task_transfer_tests.rs"]
mod task_transfer_tests;
