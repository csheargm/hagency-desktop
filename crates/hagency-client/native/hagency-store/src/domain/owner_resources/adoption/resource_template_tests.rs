use super::*;
use crate::management::{
    HostContext, Permission, Session, ResourceTemplateCommand, ResourceTemplateChange, Intent,
    IntentOperation,
};

fn context(f: &Fixture, epoch: u64) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        epoch,
        "a".repeat(64),
        epoch,
        "template_device".into(),
        1,
    )
    .unwrap()
}
fn session(b: &CurrentManagementStore, c: &HostContext) -> Session {
    Session::new_current(
        b.clone(),
        c.clone(),
        [
            Permission::ReadResourceTemplates,
            Permission::ManageResourceTemplates,
            Permission::ReadIntents,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn command(
    s: &Session,
    c: &HostContext,
    f: &Fixture,
    id: &str,
    operation: &str,
    name: &str,
) -> ResourceTemplateCommand {
    let page = s
        .resource_templates(c, f.resource.id(), None)
        .await
        .unwrap();
    ResourceTemplateCommand {
        command_id: id.into(),
        resource_id: f.resource.id(),
        definition_id: "rad_original".into(),
        expected_revision: page.revision,
        change: ResourceTemplateChange {
            operation: operation.into(),
            name: name.into(),
            role: page.roles[0].clone(),
            enabled: true,
        },
    }
}
async fn counts(f: &Fixture) -> (u64, u64) {
    f.registry
        .0
        .store
        .call(1, |d| {
            Ok((
                d.db.query_row("SELECT COUNT(*) FROM agent_definitions", [], |r| r.get(0))?,
                d.db.query_row(
                    "SELECT COUNT(*) FROM management_commands WHERE operation='resource.template'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn template_original_receipt_survives_update_delete_and_registry_restart() {
    let mut f = fixture().await;
    let b = resource_name_tests::active(&f).await;
    let c = context(&f, 1);
    let s = session(&b, &c);
    let page = s.resource_templates(&c,f.resource.id(),None).await.unwrap();
    assert_eq!(page.resource_name,f.resource.model);
    assert!(!page.resource_name.starts_with("owner_ns_"));
    let original = command(&s, &c, &f, "create", "create", "helper").await;
    let intent = Intent::new(
        IntentOperation::TemplateCreate,
        original.command_id.clone(),
        &original,
    )
    .unwrap();
    s.prepare_intent(&c, intent).await.unwrap();
    let first = s
        .apply_resource_template(&c, original.clone())
        .await
        .unwrap();
    let created = s
        .resource_template(&c, f.resource.id(), original.definition_id.clone())
        .await
        .unwrap();
    assert_eq!(created.definition.status, "defined");
    assert!(created.can_edit_identity);
    let second = command(&s, &c, &f, "update", "update", "renamed").await;
    s.apply_resource_template(&c, second).await.unwrap();
    assert_eq!(
        s.resource_template(&c, f.resource.id(), original.definition_id.clone())
            .await
            .unwrap()
            .definition
            .created_at,
        created.definition.created_at
    );
    assert_eq!(
        s.apply_resource_template(&c, original.clone())
            .await
            .unwrap(),
        first
    );
    let mut changed = original.clone();
    changed.change.name = "forged".into();
    assert!(matches!(
        s.apply_resource_template(&c, changed).await,
        Err(Error::Conflict)
    ));
    let mut stale = original.clone();
    stale.command_id = "stale".into();
    assert!(matches!(
        s.apply_resource_template(&c, stale).await,
        Err(Error::Conflict)
    ));
    let delete = command(&s, &c, &f, "delete", "delete", "renamed").await;
    let removed = s.apply_resource_template(&c, delete.clone()).await.unwrap();
    assert!(removed.deleted);
    assert_eq!(removed.definition_id, "rad_original");
    assert_eq!(
        s.apply_resource_template(&c, delete).await.unwrap(),
        removed
    );
    assert!(
        s.resource_templates(&c, f.resource.id(), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(counts(&f).await, (0, 3));
    assert_eq!(
        s.pending_intents(&c).await.unwrap()[0].input,
        serde_json::to_value(&original).unwrap()
    );
    s.retire();
    drop(s);
    b.shutdown().await.unwrap();
    drop(b);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let b = current_backing(&f).await;
    let s = session(&b, &context(&f, 2));
    assert_eq!(
        s.apply_resource_template(&context(&f, 2), original)
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&f).await, (0, 3));
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn template_in_use_disable_only_and_global_names_do_not_rewrite_approval() {
    let f = fixture().await;
    let b = resource_name_tests::active(&f).await;
    let c = context(&f, 1);
    let s = session(&b, &c);
    let page = s.resource_templates(&c,f.resource.id(),None).await.unwrap();
    assert_eq!(page.resource_name,f.resource.model);
    assert!(!page.resource_name.starts_with("owner_ns_"));
    let original = command(&s, &c, &f, "create", "create", "helper").await;
    s.apply_resource_template(&c, original).await.unwrap();
    let resource = f.resource.id();
    f.registry.0.store.call(4096,move|db|{
        db.db.execute("INSERT INTO registrations VALUES('fixture',1,'{}')",[])?;
        db.db.execute("INSERT INTO projects VALUES('fixture','project',1,'!p:test','@owner:test','!o:test')",[])?;
        db.db.execute("INSERT INTO engagements(id,fleet_id,generation,request_id,digest,context,evidence,project_id,name,resource_id,tokens,state,projection) VALUES('approval','fixture',1,'request','digest','{}','{}','project','helper',?1,1,'active','{\"immutable\":true}')",[resource])?;
        Ok(())
    }).await.unwrap();
    let detail = s
        .resource_template(&c, f.resource.id(), "rad_original".into())
        .await
        .unwrap();
    assert_eq!(detail.definition.active_engagements, 1);
    assert_eq!(detail.definition.status, "provisioned");
    assert!(!detail.can_edit_identity);
    assert!(!detail.can_delete);
    assert!(detail.can_toggle);
    for (op, name) in [("update", "changed"), ("delete", "helper")] {
        let input = command(&s, &c, &f, op, op, name).await;
        assert!(matches!(
            s.apply_resource_template(&c, input).await,
            Err(Error::State)
        ));
    }
    let mut disabled = command(&s, &c, &f, "disable", "update", "helper").await;
    disabled.change.enabled = false;
    s.apply_resource_template(&c, disabled).await.unwrap();
    assert!(
        !s.resource_template(&c, f.resource.id(), "rad_original".into())
            .await
            .unwrap()
            .definition
            .enabled
    );
    let mut duplicate = command(&s, &c, &f, "duplicate", "create", "helper").await;
    duplicate.definition_id = "rad_duplicate".into();
    assert!(matches!(
        s.apply_resource_template(&c, duplicate).await,
        Err(Error::Conflict)
    ));
    let resource = f.resource.clone();
    f.registry.0.store.call(4096,move|db|{
        let mut other=resource;other.preset_id="other_template_resource".into();
        db.db.execute("INSERT INTO resources VALUES(?1,?2,?3)",params![other.id(),other.preset_id,serde_json::to_string(&other)?])?;
        db.db.execute("INSERT INTO agent_definitions VALUES('foreign_definition',?1,'other_name','coding',1,1)",[other.id()])?;
        assert_eq!(db.db.query_row("SELECT projection FROM engagements WHERE id='approval'",[],|r|r.get::<_,String>(0))?,"{\"immutable\":true}");Ok(())
    }).await.unwrap();
    let mut duplicate = command(&s, &c, &f, "global_duplicate", "create", "other_name").await;
    duplicate.definition_id = "rad_global".into();
    assert!(matches!(
        s.apply_resource_template(&c, duplicate).await,
        Err(Error::Conflict)
    ));
    assert_eq!(counts(&f).await, (2, 2));
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn template_receipt_failure_and_queued_retirement_rollback_all_rows() {
    let f = fixture().await;
    let b = resource_name_tests::active(&f).await;
    let c = context(&f, 1);
    let s = session(&b, &c);
    let input = command(&s, &c, &f, "rollback", "create", "helper").await;
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER reject_template_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='resource.template' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).await.unwrap();
    assert!(s.apply_resource_template(&c, input.clone()).await.is_err());
    assert_eq!(counts(&f).await, (0, 0));
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db
                .execute_batch("DROP TRIGGER reject_template_receipt")?;
            Ok(())
        })
        .await
        .unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = f.registry.0.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .call(1, move |_| {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    ready_rx.await.unwrap();
    let request = s.apply_resource_template(&c, input.clone());
    tokio::pin!(request);
    tokio::select! {biased;r=&mut request=>panic!("unexpected: {r:?}"),_=tokio::task::yield_now()=>{}}
    s.retire();
    release_tx.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(request.await.is_err());
    assert_eq!(counts(&f).await, (0, 0));
    let fresh = session(&b, &c);
    fresh.apply_resource_template(&c, input).await.unwrap();
    assert_eq!(counts(&f).await, (1, 1));
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn template_exact_grants_account_switch_bounded_pages_and_uninitialized_refusal() {
    let f = fixture().await;
    let b = resource_name_tests::active(&f).await;
    let c = context(&f, 1);
    let s = session(&b, &c);
    let input = command(&s, &c, &f, "new", "create", "helper").await;
    for rights in [
        vec![Permission::ReadResources],
        vec![Permission::ManageResourceTemplates],
    ] {
        let denied = Session::new_current(
            b.clone(),
            c.clone(),
            rights,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        assert!(matches!(
            denied.resource_templates(&c, f.resource.id(), None).await,
            Err(Error::LocalAuthority)
        ));
    }
    let read = Session::new_current(
        b.clone(),
        c.clone(),
        [Permission::ReadResourceTemplates],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        !read
            .resource_templates(&c, f.resource.id(), None)
            .await
            .unwrap()
            .can_create
    );
    assert!(matches!(
        read.apply_resource_template(&c, input.clone()).await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        s.apply_resource_template(&context(&f, 2), input).await,
        Err(Error::LocalAuthority)
    ));
    let resource = f.resource.id();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            for i in 0..200 {
                db.db.execute(
                    "INSERT INTO agent_definitions VALUES(?1,?2,?3,'coding',1,1)",
                    params![format!("rad_{i:03}"), resource, format!("helper_{i}")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let first = s
        .resource_templates(&c, f.resource.id(), None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 16);
    assert_eq!(first.next_after.as_deref(), Some("rad_015"));
    assert!(!first.can_create);
    let next = s
        .resource_templates(&c, f.resource.id(), first.next_after)
        .await
        .unwrap();
    assert_eq!(next.items[0].id, "rad_016");
    let mut too_many = command(&s, &c, &f, "max", "create", "extra").await;
    too_many.definition_id = "rad_extra".into();
    assert!(matches!(
        s.apply_resource_template(&c, too_many).await,
        Err(Error::Capacity)
    ));
    assert_eq!(counts(&f).await, (200, 0));
    let server_dir = tempfile::tempdir().unwrap();
    let journal = crate::management::ServerManagementJournal::open(&server_dir.path().join("server"), &f.profile.digest, &f.profile.account).unwrap();
    let server_session = Session::new_server(journal.clone(),c.clone(),
        [Permission::ReadResourceTemplates,Permission::ManageResourceTemplates],Instant::now()+Duration::from_secs(30)).unwrap();
    assert!(matches!(server_session.resource_templates(&c,f.resource.id(),None).await,Err(Error::ResourceSetupRequired)));
    journal.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn template_cancel_races_on_original_receipt_without_replaying_or_mutating_the_change() {
    let f = fixture().await;
    let b = resource_name_tests::active(&f).await;
    let c = context(&f, 1);
    let s = session(&b, &c);
    let names = |s: &Session| {
        let (s, c, id) = (s.clone(), c.clone(), f.resource.id());
        async move {
            s.resource_templates(&c, id, None)
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
        }
    };
    // Withdrawn before apply: nothing is written, and the command never applies.
    let original = command(&s, &c, &f, "cancel_before_apply", "create", "helper").await;
    assert!(s.cancel_resource_template(&c, original.clone()).await.unwrap().is_none());
    assert!(matches!(
        s.apply_resource_template(&c, original.clone()).await,
        Err(Error::ManagementChangeCancelled)
    ));
    assert!(s.cancel_resource_template(&c, original.clone()).await.unwrap().is_none());
    let mut conflicting = original.clone();
    conflicting.change.name = "forged".into();
    assert!(matches!(
        s.cancel_resource_template(&c, conflicting).await,
        Err(Error::Conflict)
    ));
    assert!(names(&s).await.is_empty());
    assert_eq!(counts(&f).await, (0, 1));
    // Withdrawn after apply: the applied effect is recovered, never contradicted.
    let committed = command(&s, &c, &f, "cancel_after_apply", "create", "helper").await;
    let receipt = s.apply_resource_template(&c, committed.clone()).await.unwrap();
    assert_eq!(
        s.cancel_resource_template(&c, committed.clone()).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(s.apply_resource_template(&c, committed).await.unwrap(), receipt);
    assert_eq!(names(&s).await, ["helper"]);
    // Independent sessions still arbitrate through the canonical receipt.
    let other = session(&b, &c);
    let racing = command(&s, &c, &f, "race_cancel_apply", "update", "renamed").await;
    let (cancelled, applied) = tokio::join!(
        s.cancel_resource_template(&c, racing.clone()),
        other.apply_resource_template(&c, racing.clone())
    );
    match (cancelled.unwrap(), applied) {
        (None, Err(Error::ManagementChangeCancelled)) => assert_eq!(names(&s).await, ["helper"]),
        (Some(result), Ok(applied)) => {
            assert_eq!(result, applied);
            assert_eq!(names(&s).await, ["renamed"]);
        }
        unexpected => panic!("inconsistent cancellation race: {unexpected:?}"),
    }
    assert_eq!(counts(&f).await, (1, 3));
    // Reading templates is not authority to withdraw a change.
    let denied = Session::new_current(
        b.clone(),
        c.clone(),
        [Permission::ReadResourceTemplates],
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert!(denied.cancel_resource_template(&c, original).await.is_err());
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
