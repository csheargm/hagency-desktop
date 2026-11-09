use super::*;
use crate::management::{
    CreateResourceConfiguration, HostContext, Permission,  Session,
};
use crate::{CeilingChange, ProfileChange};

#[tokio::test]
async fn additional_resource_default_is_inherited_or_explicitly_changed_without_quota_changes() {
    use hagency_core::execution::ExecutionPolicy;
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let before = rows(&f, QUOTA).await;
    let mut request = input(&f, &f.resource.id(), "default_source").await;
    assert!(serde_json::to_value(&request).unwrap().get("executionPolicy").is_none());
    request.execution_policy = Some(ExecutionPolicy { yolo:true });
    let first = s.create_resource_configuration(&ctx, copy(&request), deadline()).await.unwrap();
    assert_eq!(definition(&f, &first.resource_id).await.execution_policy, request.execution_policy);
    let preserved = input(&f, &first.resource_id, "default_copy").await;
    let second = s.create_resource_configuration(&ctx, preserved, deadline()).await.unwrap();
    assert_eq!(definition(&f, &second.resource_id).await.execution_policy, request.execution_policy);
    let mut override_mode = input(&f, &first.resource_id, "default_standard").await;
    override_mode.execution_policy = Some(ExecutionPolicy { yolo:false });
    let third = s.create_resource_configuration(&ctx, override_mode, deadline()).await.unwrap();
    assert_eq!(definition(&f, &third.resource_id).await.execution_policy, Some(ExecutionPolicy { yolo:false }));
    assert_eq!(definition(&f, &first.resource_id).await.execution_policy, request.execution_policy);
    let snapshot = rows(&f, EFFECTS).await;
    let replayed = s.create_resource_configuration(&ctx, copy(&request), deadline()).await.unwrap();
    assert_eq!(replayed.resource_id, first.resource_id);
    let mut changed = copy(&request);
    changed.execution_policy = Some(ExecutionPolicy { yolo:false });
    assert!(matches!(s.recover_resource_configuration(&ctx, &changed).await, Err(Error::Conflict)));
    assert_eq!(rows(&f, EFFECTS).await, snapshot);
    assert_eq!(rows(&f, QUOTA).await, before);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "create_device".into(),
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
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
async fn input(f: &Fixture, id: &str, command: &str) -> CreateResourceConfiguration {
    CreateResourceConfiguration {
        execution_policy: None,
        command_id: command.into(),
        source_resource_id: id.into(),
        expected_revision: f
            .registry
            .resource(&f.profile, id.into())
            .await
            .unwrap()
            .revision,
        profile: ProfileChange::Preserve {},
        ceiling: CeilingChange::Preserve {},
    }
}
fn copy(input: &CreateResourceConfiguration) -> CreateResourceConfiguration {
    serde_json::from_value(serde_json::to_value(input).unwrap()).unwrap()
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
async fn rows(f: &Fixture, tables: &[&str]) -> Value {
    let tables: Vec<String> = tables.iter().map(|v| v.to_string()).collect();
    f.registry
        .0
        .store
        .call(16 * 1024, move |db| {
            let mut result = Vec::new();
            for table in tables {
                let mut q = db
                    .db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
                let count = q.column_count();
                let data = q
                    .query_map([], |r| {
                        Ok((0..count)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>())
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                result.push(json!({"table":table,"rows":data}));
            }
            Ok(json!(result))
        })
        .await
        .unwrap()
}
const QUOTA: &[&str] = &[
    "seats",
    "owner_resource_accounts",
    "managed_accounts",
    "management_account_profiles",
    "owner_capacity_grants",
    "engagements",
    "owner_namespace_enrollments",
];
const EFFECTS: &[&str] = &[
    "resources",
    "resource_accounts",
    "owner_resource_namespaces",
    "owner_resource_identity",
    "owner_resource_grants",
    "owner_resource_receipts",
    "owner_resource_provenance",
    "management_commands",
];

#[tokio::test]
async fn additional_configuration_preserves_offered_source_and_shared_quota() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let original = definition(&f, &f.resource.id()).await;
    let request = input(&f, &original.id(), "new_config").await;
    f.registry
        .record_contribution_attempt(
            f.profile.clone(),
            original.id(),
            request.expected_revision.clone(),
            "offer_original".into(),
        )
        .await
        .unwrap();
    let before = rows(&f, QUOTA).await;
    let created = s
        .create_resource_configuration(&ctx, copy(&request), deadline())
        .await
        .unwrap();
    assert!(created.published);
    let target = definition(&f, &created.resource_id).await;
    let other = ProfileAuthority::new("b".repeat(64), "@other:a.test".into(), deadline()).unwrap();
    f.registry.prepare_link(other.clone(), "other_source_access".into(), original.id(), deadline()).unwrap().approve().await.unwrap();
    assert!(f.registry.resource(&other, original.id()).await.is_ok());
    assert!(f.registry.resource(&other, target.id()).await.is_err());
    assert_ne!(target.preset_id, original.preset_id);
    assert_eq!(target.seat_id, original.seat_id);
    assert_eq!(target.model, original.model);
    assert_eq!(target.reasoning, original.reasoning);
    assert_eq!(
        serde_json::to_value(&target.ceiling).unwrap(),
        serde_json::to_value(&original.ceiling).unwrap()
    );
    assert_eq!(
        serde_json::to_value(definition(&f, &original.id()).await).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(rows(&f, QUOTA).await, before);
    let new_id = created.resource_id.clone();
    let old_id = original.id();
    let registry = f.registry.id().to_owned();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            assert_eq!(
                provenance::history(&db.db, &registry, &old_id)?,
                provenance::History::Offered
            );
            assert_eq!(
                provenance::history(&db.db, &registry, &new_id)?,
                provenance::History::NeverOffered
            );
            Ok(())
        })
        .await
        .unwrap();
    let saved = s
        .recover_resource_configuration(&ctx, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::to_value(created).unwrap()
    );
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn additional_configuration_models_and_ceiling_semantics_do_not_change_account_quota() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let before = rows(&f, QUOTA).await;
    for (command, ceiling, expected, model) in [
        ("clear", CeilingChange::Clear {}, None, "gpt-5.6-sol"),
        (
            "zero",
            CeilingChange::Monthly {
                tokens: 0.try_into().unwrap(),
            },
            Some(0),
            "gpt-6.1-terra",
        ),
        (
            "finite",
            CeilingChange::Monthly {
                tokens: 20_000_000.try_into().unwrap(),
            },
            Some(20_000_000),
            "gpt-6-luna",
        ),
    ] {
        let mut request = input(&f, &f.resource.id(), command).await;
        request.ceiling = ceiling;
        request.profile = ProfileChange::Select {
            model: model.into(),
            reasoning: Some("high".into()),
        };
        let created = s
            .create_resource_configuration(&ctx, request, deadline())
            .await
            .unwrap();
        let target = definition(&f, &created.resource_id).await;
        assert_eq!(target.model, model);
        assert_eq!(target.reasoning.as_deref(), Some("high"));
        assert_eq!(
            target.ceiling.and_then(|c| c.tokens).map(u64::from),
            expected
        );
    }
    assert_eq!(rows(&f, QUOTA).await, before);
    let effects = rows(&f, EFFECTS).await;
    let mut bad = input(&f, &f.resource.id(), "unqualified").await;
    bad.profile = ProfileChange::Select {
        model: "unqualified".into(),
        reasoning: None,
    };
    assert!(matches!(
        s.create_resource_configuration(&ctx, bad, deadline()).await,
        Err(Error::Unqualified)
    ));
    assert_eq!(rows(&f, EFFECTS).await, effects);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn additional_configuration_original_receipt_survives_concurrency_source_change_and_restart()
{
    let mut f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let peer = session(backing.clone(), ctx.clone());
    let request = input(&f, &f.resource.id(), "lost_reply").await;
    let (one, two) = tokio::join!(
        s.create_resource_configuration(&ctx, copy(&request), deadline()),
        peer.create_resource_configuration(&ctx, copy(&request), deadline())
    );
    let original = serde_json::to_value(one.unwrap()).unwrap();
    assert_eq!(serde_json::to_value(two.unwrap()).unwrap(), original);
    let mut source = definition(&f, &f.resource.id()).await;
    source.published = false;
    f.registry
        .0
        .store
        .call(1024, move |db| {
            let tx = db.db.unchecked_transaction()?;
            write_resource_configuration(&tx, &source, false)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let after = rows(&f, EFFECTS).await;
    let mut collision = copy(&request);
    collision.ceiling = CeilingChange::Clear {};
    assert!(matches!(
        s.create_resource_configuration(&ctx, collision, deadline())
            .await,
        Err(Error::Conflict)
    ));
    let mut stale = copy(&request);
    stale.command_id = "different_request".into();
    assert!(matches!(
        s.create_resource_configuration(&ctx, stale, deadline())
            .await,
        Err(Error::ResourceConfigurationSourceChanged)
    ));
    s.retire();
    peer.retire();
    drop(s);
    drop(peer);
    backing.shutdown().await.unwrap();
    drop(backing);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let reopened = current_backing(&f).await;
    let fresh = session(reopened.clone(), ctx.clone());
    assert_eq!(
        serde_json::to_value(
            fresh
                .recover_resource_configuration(&ctx, &request)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        original
    );
    assert_eq!(
        serde_json::to_value(
            fresh
                .create_resource_configuration(&ctx, copy(&request), deadline())
                .await
                .unwrap()
        )
        .unwrap(),
        original
    );
    assert_eq!(rows(&f, EFFECTS).await, after);
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

async fn namespace_resource(f: &Fixture) -> (String, String) {
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
        command_id: "namespace_source".into(),
        account: account.clone(),
        model: "gpt-5.6-sol".into(),
        reasoning: Some("medium".into()),
        monthly_tokens: 700,
    };
    let receipt = f
        .registry
        .prepare_namespace_enrollment(f.profile.clone(), request, selected, provider())
        .unwrap()
        .approve()
        .await
        .unwrap();
    (receipt.resource_id, account.account_id)
}

#[tokio::test]
async fn additional_configuration_keeps_original_namespace_and_rejects_retired_account() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let (source, account) = namespace_resource(&f).await;
    let before = rows(&f, QUOTA).await;
    let original = input(&f, &source, "namespace_copy").await;
    let created = s
        .create_resource_configuration(&ctx, copy(&original), deadline())
        .await
        .unwrap();
    let old = f
        .registry
        .resource_namespace(
            f.profile.clone(),
            source.clone(),
            original.expected_revision.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let new = f
        .registry
        .resource_namespace(
            f.profile.clone(),
            created.resource_id.clone(),
            created.revision.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let mut a = std::collections::BTreeMap::new();
    let mut b = std::collections::BTreeMap::new();
    old.launch().apply_codex_environment(&mut a).unwrap();
    new.launch().apply_codex_environment(&mut b).unwrap();
    assert_eq!(a, b);
    assert_eq!(rows(&f, QUOTA).await, before);
    f.registry
        .0
        .store
        .call(1024, move |db| {
            db.db.execute(
                "UPDATE managed_accounts SET state='retired' WHERE id=?1",
                [account],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let effects = rows(&f, EFFECTS).await;
    let pending = input(&f, &source, "account_retired").await;
    assert!(matches!(
        s.create_resource_configuration(&ctx, pending, deadline())
            .await,
        Err(Error::LocalAuthority)
    ));
    assert_eq!(rows(&f, EFFECTS).await, effects);
    assert!(
        s.recover_resource_configuration(&ctx, &original)
            .await
            .unwrap()
            .is_some()
    );
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn additional_configuration_failed_receipt_rolls_back_all_new_associations() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let (source, _) = namespace_resource(&f).await;
    let request = input(&f, &source, "atomic_failure").await;
    let before = rows(&f, EFFECTS).await;
    let quota = rows(&f, QUOTA).await;
    let mut collision = copy(&request);
    collision.command_id = "namespace_source".into();
    assert!(matches!(s.create_resource_configuration(&ctx, collision, deadline()).await, Err(Error::Conflict)));
    assert_eq!(rows(&f, EFFECTS).await, before);
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fail_configuration_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='resource.create_configuration' BEGIN SELECT RAISE(ABORT,'fixture interrupted receipt'); END;")?;Ok(())}).await.unwrap();
    assert!(
        s.create_resource_configuration(&ctx, copy(&request), deadline())
            .await
            .is_err()
    );
    assert_eq!(rows(&f, EFFECTS).await, before);
    assert_eq!(rows(&f, QUOTA).await, quota);
    assert!(
        s.recover_resource_configuration(&ctx, &request)
            .await
            .unwrap()
            .is_none()
    );
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db
                .execute_batch("DROP TRIGGER fail_configuration_receipt;")?;
            Ok(())
        })
        .await
        .unwrap();
    s.create_resource_configuration(&ctx, request, deadline())
        .await
        .unwrap();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn additional_configuration_requires_live_permission_source_access_and_native_confirmation() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = context(&f);
    let s = session(backing.clone(), ctx.clone());
    let read = Session::new_current(
        backing.clone(),
        ctx.clone(),
        [Permission::ReadResources],
        deadline(),
    )
    .unwrap();
    let request = input(&f, &f.resource.id(), "not_authorized").await;
    let before = rows(&f, EFFECTS).await;
    assert!(
        read.create_resource_configuration(&ctx, copy(&request), deadline())
            .await
            .is_err()
    );
    assert!(
        s.create_resource_configuration(&ctx, copy(&request), Instant::now())
            .await
            .is_err()
    );
    assert!(
        s.create_resource_configuration(
            &ctx,
            copy(&request),
            Instant::now() + Duration::from_secs(121)
        )
        .await
        .is_err()
    );
    s.retire();
    assert!(
        s.create_resource_configuration(&ctx, copy(&request), deadline())
            .await
            .is_err()
    );
    assert_eq!(rows(&f, EFFECTS).await, before);
    let active = session(backing.clone(), ctx.clone());
    f.registry
        .prepare_revoke(
            f.profile.clone(),
            "revoke_source".into(),
            f.resource.id(),
            deadline(),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let revoked = rows(&f, EFFECTS).await;
    assert!(
        active
            .create_resource_configuration(&ctx, request, deadline())
            .await
            .is_err()
    );
    assert_eq!(rows(&f, EFFECTS).await, revoked);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[test]
fn additional_configuration_input_cannot_supply_account_quota_identity_or_publication() {
    let input = json!({"commandId":"closed","sourceResourceId":"res_source","expectedRevision":"a".repeat(64),"profile":{"kind":"preserve"},"ceiling":{"kind":"preserve"}});
    serde_json::from_value::<CreateResourceConfiguration>(input.clone())
        .unwrap()
        .validate()
        .unwrap();
    for key in [
        "seatId",
        "accountId",
        "provider",
        "namespace",
        "published",
        "resourceId",
        "create",
        "engagement",
        "quotaTokens",
    ] {
        let mut altered = input.clone();
        altered[key] = json!(true);
        assert!(
            serde_json::from_value::<CreateResourceConfiguration>(altered).is_err(),
            "{key}"
        );
    }
}
