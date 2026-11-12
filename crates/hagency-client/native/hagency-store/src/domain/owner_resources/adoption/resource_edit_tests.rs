use super::*;
use crate::management::{EditLocalResource, HostContext, Permission, Session, ResourceDetailQuery, ResourceEditBlocker};
use crate::{CeilingChange, ProfileChange};

#[tokio::test]
async fn resource_default_edit_is_scoped_preserves_omission_and_recovers_original_receipt() {
    use hagency_core::execution::ExecutionPolicy;
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let id = enroll(&f, "policy_resource").await;
    let invariants = invariant_rows(&f).await;
    let mut original = input(&f, &id, "default_enable").await;
    assert!(serde_json::to_value(&original).unwrap().get("executionPolicy").is_none());
    original.profile = ProfileChange::Preserve {};
    original.ceiling = CeilingChange::Preserve {};
    original.execution_policy = Some(ExecutionPolicy { yolo:true });
    let saved = s.edit_local_resource(&ctx, copy(&original), binding(&f, &id).await).await.unwrap();
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id:id.clone() }).await.unwrap();
    assert!(detail.execution_policy.yolo);
    assert_ne!(saved.revision, original.expected_revision);
    let preserve = input(&f, &id, "default_preserve").await;
    s.edit_local_resource(&ctx, preserve, binding(&f, &id).await).await.unwrap();
    assert_eq!(definition(&f, &id).await.execution_policy, original.execution_policy);
    let mut disable = input(&f, &id, "default_disable").await;
    disable.execution_policy = Some(ExecutionPolicy { yolo:false });
    s.edit_local_resource(&ctx, disable, binding(&f, &id).await).await.unwrap();
    let recovered = s.recover_local_resource_edit(&ctx, &original).await.unwrap().unwrap();
    assert_eq!(recovered.revision, saved.revision);
    assert!(!s.resource_detail(&ctx, ResourceDetailQuery { id:id.clone() }).await.unwrap().execution_policy.yolo);
    let mut changed = copy(&original);
    changed.execution_policy = Some(ExecutionPolicy { yolo:false });
    assert!(matches!(s.recover_local_resource_edit(&ctx, &changed).await, Err(Error::Conflict)));
    assert_eq!(invariant_rows(&f).await, invariants);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "edit_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: CurrentManagementStore, context: HostContext) -> Session {
    Session::new_current(
        backing,
        context,
        [Permission::ReadResources, Permission::ConfigureResources],
        Instant::now() + Duration::from_secs(90),
    )
    .unwrap()
}
async fn enroll(f: &Fixture, command: &str) -> String {
    f.registry
        .prepare_enrollment(
            f.profile.clone(),
            command.into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            700,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id
}
async fn definition(f: &Fixture, id: &str) -> Resource {
    let id = id.to_owned();
    f.registry
        .0
        .store
        .call(1024, move |db| read_resource(&db.db, &id))
        .await
        .unwrap()
}
async fn input(f: &Fixture, id: &str, command: &str) -> EditLocalResource {
    EditLocalResource {
        execution_policy: None,
        expected_ceiling_sequence: None,
        command_id: command.into(),
        resource_id: id.into(),
        expected_revision: f
            .registry
            .resource(&f.profile, id.into())
            .await
            .unwrap()
            .revision,
        profile: ProfileChange::Select {
            model: "gpt-5.6-sol".into(),
            reasoning: Some("high".into()),
        },
        ceiling: CeilingChange::Monthly {
            tokens: 450.try_into().unwrap(),
        },
    }
}
fn copy(input: &EditLocalResource) -> EditLocalResource {
    serde_json::from_value(serde_json::to_value(input).unwrap()).unwrap()
}
async fn binding(f: &Fixture, id: &str) -> ResourceEditBinding {
    let revision = f
        .registry
        .resource(&f.profile, id.into())
        .await
        .unwrap()
        .revision;
    let namespace = f
        .registry
        .resource_namespace(f.profile.clone(), id.into(), revision)
        .await
        .unwrap();
    let verified = f
        .registry
        .verify_provider_resource(f.profile.clone(), id.into(), provider())
        .await
        .unwrap();
    ResourceEditBinding::new(verified, namespace).unwrap()
}
async fn receipts(f: &Fixture) -> u64 {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT count(*) FROM management_commands WHERE operation='resource.edit_local'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}
async fn invariant_rows(f: &Fixture) -> serde_json::Value {
    f.registry
        .0
        .store
        .call(4096, |db| {
            let mut rows = Vec::new();
            for table in [
                "seats",
                "owner_resource_accounts",
                "owner_resource_grants",
                "owner_resource_receipts",
                "owner_resource_provenance",
                "owner_resource_namespaces",
                "owner_namespace_enrollments",
                "owner_capacity_grants",
            ] {
                let mut q = db
                    .db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
                let count = q.column_count();
                let data = q
                    .query_map([], |r| {
                        Ok((0..count)
                            .map(|c| format!("{:?}", r.get_ref(c).unwrap()))
                            .collect::<Vec<_>>())
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.push(json!({"table":table,"rows":data}));
            }
            Ok(json!(rows))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn local_edit_keeps_quota_identity_and_recovers_original_after_offer_and_restart() {
    let mut f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let id = enroll(&f, "edit_create").await;
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(!detail.editing.allowed);
    assert_eq!(detail.editing.reason, Some(ResourceEditBlocker::ManagedNamespaceRequired));
    let before = definition(&f, &id).await;
    let invariants = invariant_rows(&f).await;
    let original = input(&f, &id, "edit_original").await;
    assert!(
        s.recover_local_resource_edit(&ctx, &original)
            .await
            .unwrap()
            .is_none()
    );
    let result = s
        .edit_local_resource(&ctx, copy(&original), binding(&f, &id).await)
        .await
        .unwrap();
    let after = definition(&f, &id).await;
    assert_eq!(after.id(), before.id());
    assert_eq!(after.preset_id, before.preset_id);
    assert_eq!(after.seat_id, before.seat_id);
    assert_eq!(after.reasoning.as_deref(), Some("high"));
    assert_eq!(u64::from(after.ceiling.unwrap().tokens.unwrap()), 450);
    assert!(!after.published);
    assert_ne!(result.revision, original.expected_revision);
    assert_eq!(invariant_rows(&f).await, invariants);
    // A second command using the old enrollment selection does not silently
    // reinterpret the edited definition or create independent capacity.
    let reenroll = f
        .registry
        .prepare_enrollment(
            f.profile.clone(),
            "enroll_again".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            700,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await;
    assert!(matches!(reenroll, Err(Error::Conflict)));
    f.registry
        .record_contribution_attempt(
            f.profile.clone(),
            id.clone(),
            result.revision.clone(),
            "offer_after_edit".into(),
        )
        .await
        .unwrap();
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(!detail.editing.allowed);
    assert_eq!(detail.editing.reason, Some(ResourceEditBlocker::PreviouslyOffered));
    let next = input(&f, &id, "no_second_edit").await;
    assert!(matches!(
        s.edit_local_resource(&ctx, next, binding(&f, &id).await)
            .await,
        Err(Error::ResourceEditNotAvailable)
    ));
    let saved = s
        .recover_local_resource_edit(&ctx, &original)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::to_value(&result).unwrap()
    );
    let replay = s
        .edit_local_resource(&ctx, copy(&original), binding(&f, &id).await)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(&result).unwrap()
    );
    let mut changed = copy(&original);
    changed.ceiling = CeilingChange::Clear {};
    assert!(matches!(
        s.recover_local_resource_edit(&ctx, &changed).await,
        Err(Error::Conflict)
    ));
    assert_eq!(receipts(&f).await, 1);
    s.retire();
    drop(s);
    backing.shutdown().await.unwrap();
    drop(backing);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let reopened = current_backing(&f).await;
    let fresh = session(reopened.clone(), ctx.clone());
    assert_eq!(
        serde_json::to_value(
            fresh
                .recover_local_resource_edit(&ctx, &original)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(result).unwrap()
    );
    assert_eq!(receipts(&f).await, 1);
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_edit_rejects_unknown_history_stale_revision_unsupported_model_and_rolls_back_receipt_failure()
 {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let imported_id = f.resource.id();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            let mut imported = read_resource(&db.db, &imported_id)?;
            imported.published = false;
            db.db.execute(
                "UPDATE resources SET config=?2 WHERE id=?1",
                params![imported_id, serde_json::to_string(&imported)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        s.edit_local_resource(
            &ctx,
            input(&f, &f.resource.id(), "old_resource").await,
            binding(&f, &f.resource.id()).await
        )
        .await,
        Err(Error::ResourceEditNotAvailable)
    ));
    let id = enroll(&f, "new_resource").await;
    let before = serde_json::to_value(definition(&f, &id).await).unwrap();
    let mut stale = input(&f, &id, "stale_edit").await;
    stale.expected_revision = "f".repeat(64);
    assert!(matches!(
        s.edit_local_resource(&ctx, stale, binding(&f, &id).await)
            .await,
        Err(Error::ResourceEditRevisionChanged)
    ));
    let mut invalid = input(&f, &id, "invalid_model").await;
    invalid.profile = ProfileChange::Select {
        model: "fictional-model".into(),
        reasoning: None,
    };
    assert!(matches!(
        s.edit_local_resource(&ctx, invalid, binding(&f, &id).await)
            .await,
        Err(Error::Unqualified)
    ));
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER reject_edit_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='resource.edit_local' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;")?;Ok(())}).await.unwrap();
    let request = input(&f, &id, "receipt_failure").await;
    assert!(
        s.edit_local_resource(&ctx, copy(&request), binding(&f, &id).await)
            .await
            .is_err()
    );
    assert_eq!(receipts(&f).await, 0);
    assert_eq!(
        serde_json::to_value(definition(&f, &id).await).unwrap(),
        before
    );
    assert!(
        s.recover_local_resource_edit(&ctx, &request)
            .await
            .unwrap()
            .is_none()
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db.execute_batch("DROP TRIGGER reject_edit_receipt")?;
            Ok(())
        })
        .await
        .unwrap();
    s.edit_local_resource(&ctx, request, binding(&f, &id).await)
        .await
        .unwrap();
    assert_eq!(receipts(&f).await, 1);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_edit_and_offer_are_serialized_in_both_orders() {
    for edit_first in [true, false] {
        let f = fixture().await;
        let backing = resource_name_tests::active(&f).await;
        let ctx = context(&f);
        let s = session(backing.clone(), ctx.clone());
        let id = enroll(&f, "race_create").await;
        let request = input(&f, &id, "race_edit").await;
        let old_revision = request.expected_revision.clone();
        let proof = binding(&f, &id).await;
        let edit = s.edit_local_resource(&ctx, request, proof);
        let offer = f.registry.record_contribution_attempt(
            f.profile.clone(),
            id.clone(),
            old_revision.clone(),
            "race_offer".into(),
        );
        let (edit, offer) = if edit_first {
            tokio::join!(biased;edit,offer)
        } else {
            let (o, e) = tokio::join!(biased;offer,edit);
            (e, o)
        };
        assert_ne!(edit.is_ok(), offer.is_ok());
        let now = f.registry.resource(&f.profile, id.clone()).await.unwrap();
        if edit_first {
            assert!(edit.is_ok());
            assert!(matches!(offer, Err(Error::Conflict)));
            assert_ne!(now.revision, old_revision);
            f.registry
                .record_contribution_attempt(
                    f.profile.clone(),
                    id.clone(),
                    now.revision.clone(),
                    "fresh_offer".into(),
                )
                .await
                .unwrap();
        } else {
            assert!(offer.is_ok());
            assert!(matches!(edit, Err(Error::ResourceEditNotAvailable)));
            assert_eq!(now.revision, old_revision);
        }
        let resource = id.clone();
        let expected = now.revision.clone();
        f.registry.0.store.call(1,move|db|{let offered:String=db.db.query_row("SELECT first_offer_revision FROM owner_resource_provenance WHERE resource_id=?1",[resource],|r|r.get(0))?;assert_eq!(offered,expected);Ok(())}).await.unwrap();
        assert!(matches!(
            s.edit_local_resource(
                &ctx,
                input(&f, &id, "late_edit").await,
                binding(&f, &id).await
            )
            .await,
            Err(Error::ResourceEditNotAvailable)
        ));
        backing.shutdown().await.unwrap();
        f.registry.shutdown().await.unwrap();
    }
}

async fn namespace_resource(f: &Fixture, command: &str) -> (String, String) {
    let profile = f.profile.digest.clone();
    let account = f
        .registry
        .0
        .store
        .call(1024, move |db| {
            let account = db.reserve_account(crate::ACCOUNT_PROFILE)?;
            db.db.execute(
                "INSERT INTO management_account_profiles VALUES(?1,?2)",
                params![account.id, profile],
            )?;
            let account = db.materialize_account(&account.id)?;
            Ok(crate::management::AccountModelQuery {
                account_id: account.id,
                expected_revision: account.revision,
            })
        })
        .await
        .unwrap();
    let selected = f
        .registry
        .select_resource_account(f.profile.clone(), account.clone())
        .await
        .unwrap();
    let request = NamespaceEnrollment {
        command_id: command.into(),
        account: account.clone(),
        model: "gpt-5.6-sol".into(),
        reasoning: Some("medium".into()),
        monthly_tokens: 700,
    };
    let id = f
        .registry
        .prepare_namespace_enrollment(f.profile.clone(), request, selected, provider())
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    (id, account.account_id)
}

#[tokio::test]
async fn local_edit_namespace_is_exact_and_current_not_an_optional_fallback() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let (id, account) = namespace_resource(&f, "namespace_create").await;
    let before_read = invariant_rows(&f).await;
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(detail.editing.allowed);
    assert_eq!(detail.editing.reason, None);
    assert_eq!(before_read, invariant_rows(&f).await);
    assert_eq!(receipts(&f).await, 0);
    let read_only = Session::new_current(backing.clone(), ctx.clone(), [Permission::ReadResources], Instant::now() + Duration::from_secs(60)).unwrap();
    let read_detail = read_only.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(!read_detail.editing.allowed);
    assert_eq!(read_detail.editing.reason, Some(ResourceEditBlocker::ReadOnly));
    read_only.retire();
    let verified = f
        .registry
        .verify_provider_resource(f.profile.clone(), id.clone(), provider())
        .await
        .unwrap();
    let absent = ResourceEditBinding::new(verified, None).unwrap();
    assert!(matches!(
        s.edit_local_resource(&ctx, input(&f, &id, "missing_namespace").await, absent)
            .await,
        Err(Error::LocalAuthority)
    ));
    let before = invariant_rows(&f).await;
    s.edit_local_resource(
        &ctx,
        input(&f, &id, "namespace_edit").await,
        binding(&f, &id).await,
    )
    .await
    .unwrap();
    assert_eq!(invariant_rows(&f).await, before);
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(detail.editing.allowed);
    assert_eq!(detail.resource.reasoning.as_deref(), Some("high"));
    assert_eq!(u64::from(detail.ceiling.unwrap().tokens.unwrap()), 450);
    let request = input(&f, &id, "changed_account").await;
    let proof = binding(&f, &id).await;
    f.registry
        .0
        .store
        .call(1, move |db| {
            db.db.execute(
                "UPDATE managed_accounts SET state='retired' WHERE id=?1",
                [account],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let detail = s.resource_detail(&ctx, ResourceDetailQuery { id: id.clone() }).await.unwrap();
    assert!(!detail.editing.allowed);
    assert_eq!(detail.editing.reason, Some(ResourceEditBlocker::AccountUnavailable));
    assert!(matches!(
        s.edit_local_resource(&ctx, request, proof).await,
        Err(Error::LocalAuthority)
    ));
    assert_eq!(receipts(&f).await, 1);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_edit_queued_retirement_and_permission_checks_prevent_writes() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let id = enroll(&f, "queue_create").await;
    let readonly = Session::new_current(
        backing.clone(),
        ctx.clone(),
        [Permission::ReadResources],
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert!(matches!(
        readonly
            .edit_local_resource(
                &ctx,
                input(&f, &id, "no_permission").await,
                binding(&f, &id).await
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    let request = input(&f, &id, "queued_edit").await;
    let proof = binding(&f, &id).await;
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = f.registry.0.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .call(1, move |_| {
                start_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    start_rx.await.unwrap();
    let edit = s.edit_local_resource(&ctx, request, proof);
    tokio::pin!(edit);
    tokio::select! {biased;r=&mut edit=>panic!("writer unexpectedly finished: {r:?}"),_=tokio::task::yield_now()=>{}}
    s.retire();
    release_tx.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(edit.await.is_err());
    assert_eq!(receipts(&f).await, 0);
    assert_eq!(
        definition(&f, &id).await.reasoning.as_deref(),
        Some("medium")
    );
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_edit_limit_changes_keep_the_explicit_shared_declaration() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let id = enroll(&f, "limit_create").await;
    let before = invariant_rows(&f).await;
    // Clearing the resource ceiling means unknown headroom, not unlimited
    // capacity. The shared monthly account declaration remains unchanged.
    for (command, ceiling, remaining) in [
        (
            "large",
            CeilingChange::Monthly {
                tokens: 5000.try_into().unwrap(),
            },
            Some(700),
        ),
        (
            "zero",
            CeilingChange::Monthly {
                tokens: 0.try_into().unwrap(),
            },
            Some(0),
        ),
        ("clear", CeilingChange::Clear {}, None),
    ] {
        let mut request = input(&f, &id, command).await;
        request.profile = ProfileChange::Preserve {};
        request.ceiling = ceiling;
        s.edit_local_resource(&ctx, request, binding(&f, &id).await)
            .await
            .unwrap();
        let current = f.registry.resource(&f.profile, id.clone()).await.unwrap();
        assert_eq!(current.budget.seat.quota.map(u64::from), Some(700));
        assert_eq!(current.budget.remaining_tokens.map(u64::from), remaining);
        assert_eq!(invariant_rows(&f).await, before);
    }
    assert_eq!(receipts(&f).await, 3);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[test]
fn local_edit_input_rejects_creation_publication_engagement_and_unbounded_values() {
    let base = json!({"commandId":"edit","resourceId":"resource","expectedRevision":"a".repeat(64),"profile":{"kind":"preserve"},"ceiling":{"kind":"preserve"}});
    serde_json::from_value::<EditLocalResource>(base.clone())
        .unwrap()
        .validate()
        .unwrap();
    for field in [
        "create",
        "engagement",
        "published",
        "seatId",
        "provider",
        "editable",
        "history",
    ] {
        let mut value = base.clone();
        value[field] = json!(true);
        assert!(serde_json::from_value::<EditLocalResource>(value).is_err());
    }
    for ceiling in [
        json!({"kind":"monthly","tokens":9_007_199_254_740_992u64}),
        json!({"kind":"monthly","tokens":-1}),
        json!({"kind":"monthly","tokens":1.5}),
    ] {
        let mut value = base.clone();
        value["ceiling"] = ceiling;
        assert!(serde_json::from_value::<EditLocalResource>(value).is_err());
    }
    let mut value = base;
    value["profile"] = json!({"kind":"select","model":"x".repeat(257),"reasoning":null});
    assert!(
        serde_json::from_value::<EditLocalResource>(value)
            .unwrap()
            .validate()
            .is_err()
    );
}
