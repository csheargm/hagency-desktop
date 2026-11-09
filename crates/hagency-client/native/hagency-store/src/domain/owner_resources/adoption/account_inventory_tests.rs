use super::*;
use crate::management::{AccountInventoryQuery, HostContext, Permission, Session};

#[tokio::test]
async fn account_inventory_canonical_scope_pagination_and_revoked_visibility() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let ctx = HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "account_device".into(),
        1,
    )
    .unwrap();
    let session = Session::new_current(
        backing.clone(),
        ctx.clone(),
        [Permission::ReadAccounts],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let first = session
        .account_inventory(&ctx, AccountInventoryQuery::default())
        .await
        .unwrap();
    assert_eq!(first.accounts.len(), 1);
    let account = &first.accounts[0];
    assert_eq!(account.kind, "resource_binding");
    assert_eq!(account.state, "linked");
    assert_eq!(account.readiness, "unknown");
    assert_eq!(account.resource_count, 1);
    assert!(account.revision.is_none());
    let id = account.id.clone();
    let profile = f.profile.digest.clone();
    let original = f.resource.id();
    let secret = f
        .registry
        .0
        .store
        .call(1024, move |db| {
            let mut resource = db.resource_configuration(&original)?;
            let secret = resource.seat_id.clone();
            for n in 0..18 {
                resource.preset_id = format!("account_visible_{n:02}");
                db.put_resource(&resource)?;
                db.db.execute(
                    "INSERT INTO owner_resource_grants VALUES(?1,?2,1,1)",
                    params![profile, resource.id()],
                )?;
            }
            resource.preset_id = "account_hidden".into();
            db.put_resource(&resource)?;
            db.db.execute(
                "INSERT INTO owner_resource_grants VALUES(?1,?2,1,1)",
                params!["d".repeat(64), resource.id()],
            )?;
            Ok(secret)
        })
        .await
        .unwrap();
    let a = session
        .account_inventory(
            &ctx,
            AccountInventoryQuery {
                id: Some(id.clone()),
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(a.account.as_ref().unwrap().resource_count, 19);
    assert_eq!(a.resources.len(), 16);
    let encoded = serde_json::to_string(&a).unwrap();
    assert!(!encoded.contains(&secret));
    assert!(!encoded.contains("account_hidden"));
    assert!(!encoded.contains("provider_digest"));
    let b = session
        .account_inventory(
            &ctx,
            AccountInventoryQuery {
                id: Some(id.clone()),
                after: a.next_after,
            },
        )
        .await
        .unwrap();
    assert_eq!(b.resources.len(), 3);
    assert!(b.next_after.is_none());
    let mut ids = a
        .resources
        .iter()
        .chain(&b.resources)
        .map(|r| r.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 19);
    let wrong = HostContext::new(
        "@other:a.test".into(),
        1,
        "a".repeat(64),
        1,
        "account_device".into(),
        1,
    )
    .unwrap();
    assert!(
        session
            .account_inventory(&wrong, AccountInventoryQuery::default())
            .await
            .is_err()
    );
    let denied = Session::new_current(
        backing,
        ctx.clone(),
        [Permission::ReadProviders],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        denied
            .account_inventory(&ctx, AccountInventoryQuery::default())
            .await
            .is_err()
    );
    let profile = f.profile.digest.clone();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            db.db.execute(
                "UPDATE owner_resource_grants SET active=0 WHERE profile=?1",
                [profile],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        session
            .account_inventory(&ctx, AccountInventoryQuery::default())
            .await
            .unwrap()
            .accounts
            .is_empty()
    );
    assert!(matches!(
        session
            .account_inventory(
                &ctx,
                AccountInventoryQuery {
                    id: Some(id),
                    after: None
                }
            )
            .await,
        Err(Error::NotFound)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn account_preparation_canonical_owner_is_retained_without_resource_or_login_claims() {
    use crate::management::PrepareAccount;
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;
    let ctx=HostContext::new(f.profile.account.clone(),1,"a".repeat(64),1,"account_device".into(),1).unwrap();
    let session=Session::new_current(backing,ctx.clone(),[Permission::ReadAccounts,Permission::PrepareAccounts],Instant::now()+Duration::from_secs(60)).unwrap();
    let input=||PrepareAccount{command_id:"prepare_canonical_original".into()};
    let prepared=session.prepare_account(&ctx,input()).await.unwrap();
    assert_eq!(prepared.state,"active");
    assert_eq!(session.prepare_account(&ctx,input()).await.unwrap().account_id,prepared.account_id);
    let page=session.account_inventory(&ctx,AccountInventoryQuery::default()).await.unwrap();
    assert_eq!(page.accounts.len(),2);
    let created=page.accounts.iter().find(|a|a.id==prepared.account_id).unwrap();
    assert_eq!(created.kind,"managed_namespace");assert_eq!(created.readiness,"unknown");assert_eq!(created.resource_count,0);
    let detail=session.account_inventory(&ctx,AccountInventoryQuery{id:Some(prepared.account_id.clone()),after:None}).await.unwrap();
    assert!(detail.resources.is_empty());assert_eq!(detail.account.unwrap().id,prepared.account_id);
    f.registry.0.store.call(1024,move|db|{
        assert!(db.management_account_inventory(Some(&"d".repeat(64)),AccountInventoryQuery::default())?.accounts.is_empty());
        assert_eq!(db.db.query_row("SELECT COUNT(*) FROM owner_resource_accounts",[],|r|r.get::<_,u64>(0))?,1);
        assert_eq!(db.db.query_row("SELECT COUNT(*) FROM account_login_observations",[],|r|r.get::<_,u64>(0))?,0);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn managed_login_canonical_store_keeps_private_profile_and_resource_authority() {
    use crate::management::{PrepareAccount,AccountLoginRequest};
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;
    let ctx=HostContext::new(f.profile.account.clone(),1,"a".repeat(64),1,"account_device".into(),1).unwrap();
    let session=Session::new_current(backing,ctx.clone(),[Permission::ReadAccounts,Permission::PrepareAccounts,Permission::LoginAccounts],Instant::now()+Duration::from_secs(60)).unwrap();
    let account=session.prepare_account(&ctx,PrepareAccount{command_id:"prepare_login_original".into()}).await.unwrap();
    let page=session.account_inventory(&ctx,AccountInventoryQuery::default()).await.unwrap();
    let selected=page.accounts.iter().find(|a|a.id==account.account_id).unwrap();
    let input=AccountLoginRequest{command_id:"login_canonical_original".into(),account_id:selected.id.clone(),expected_revision:selected.revision.clone().unwrap()};
    let start=session.begin_account_login(&ctx,input.clone()).await.unwrap();assert!(start.handle.is_some());
    assert!(session.begin_account_login(&ctx,input.clone()).await.unwrap().handle.is_none());
    start.handle.unwrap().settle(crate::LoginVerdict{mode:crate::AccountReadinessMode::Subscription,provider_state:"signed_in".into(),outcome:crate::LoginOutcome::Observed,expires_at_ms:None}).await.unwrap();
    assert_eq!(session.account_login_status(&ctx,input.clone()).await.unwrap().state,"authenticated");
    let other=input.clone();let original_context=ctx.clone();f.registry.0.store.call(4096,move|db|{
        let receipt=crate::management::CommandReceipt::new(&original_context,"account.login",&other.command_id,&other)?;
        assert!(db.management_begin_account_login(&"d".repeat(64),other,receipt,||Ok(())).is_err());
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_resource_accounts",[],|r|r.get::<_,u64>(0))?,1);
        assert_eq!(db.db.query_row("SELECT count(*) FROM resources",[],|r|r.get::<_,u64>(0))?,1);
        Ok(())
    }).await.unwrap();
    session.retire();assert!(session.account_login_status(&ctx,input).await.is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn account_retirement_canonical_hidden_resource_binding_and_owner_authority() {
    use crate::management::{PrepareAccount,RetireAccount};
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;
    let ctx=HostContext::new(f.profile.account.clone(),1,"a".repeat(64),1,"account_device".into(),1).unwrap();
    let until=Instant::now()+Duration::from_secs(60);
    let session=Session::new_current(backing,ctx.clone(),[Permission::ReadAccounts,Permission::PrepareAccounts,Permission::RetireAccounts],until).unwrap();
    let empty=session.prepare_account(&ctx,PrepareAccount{command_id:"retire-empty-prepare".into()}).await.unwrap();
    let row=session.account_inventory(&ctx,AccountInventoryQuery{id:Some(empty.account_id.clone()),after:None}).await.unwrap().account.unwrap();
    let input=RetireAccount{command_id:"retire-canonical-original".into(),account_id:empty.account_id,expected_revision:row.revision.unwrap()};
    let done=session.retire_account(&ctx,input.clone(),until).await.unwrap();assert_eq!(done.state,"retired");
    assert_eq!(session.retire_account(&ctx,input.clone(),until).await.unwrap(),done);
    let bound=session.prepare_account(&ctx,PrepareAccount{command_id:"retire-hidden-prepare".into()}).await.unwrap();
    let row=session.account_inventory(&ctx,AccountInventoryQuery{id:Some(bound.account_id.clone()),after:None}).await.unwrap().account.unwrap();
    let bound_input=RetireAccount{command_id:"retire-hidden".into(),account_id:bound.account_id.clone(),expected_revision:row.revision.unwrap()};
    let resource=f.resource.id();let profile=f.profile.digest.clone();
    f.registry.0.store.call(4096,move|db|{
        db.db.execute("INSERT INTO owner_resource_namespaces VALUES(?1,?2,1,?3)",params![resource,bound.account_id,profile])?;
        db.db.execute("UPDATE owner_resource_grants SET active=0 WHERE resource_id=?1",[resource])?;
        Ok(())
    }).await.unwrap();
    let row=session.account_inventory(&ctx,AccountInventoryQuery{id:Some(bound_input.account_id.clone()),after:None}).await.unwrap().account.unwrap();
    assert_eq!(row.resource_count,0,"visible resource count cannot authorize retirement");
    assert!(matches!(session.retire_account(&ctx,bound_input,until).await,Err(Error::State)));
    session.retire();assert!(session.retire_account(&ctx,input,until).await.is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn account_inventory_deduplicates_owned_bindings_and_scopes_recorded_identity() {
    let f = fixture().await;
    let _backing = resource_name_tests::active(&f).await;
    let profile = f.profile.digest.clone();
    let resource_id = f.resource.id();
    f.registry.0.store.call(4096, move |db| {
        let initial = db.management_account_inventory(Some(&profile), Default::default())?;
        let binding = initial.accounts[0].id.clone();
        let identity = initial.accounts[0].identity_key.clone().expect("verified fixture identity");
        let account = db.reserve_account(crate::ACCOUNT_PROFILE)?;
        db.db.execute("INSERT INTO management_account_profiles VALUES(?1,?2)", params![account.id, profile])?;
        db.db.execute("INSERT INTO owner_resource_namespaces VALUES(?1,?2,1,?3)", params![resource_id, account.id, profile])?;
        let before = db.db.total_changes();
        let page = db.management_account_inventory(Some(&profile), Default::default())?;
        assert_eq!(page.accounts.len(), 1, "the resource binding is not a second account");
        assert_eq!(page.accounts[0].id, account.id);
        assert_eq!(page.accounts[0].resource_count, 1);
        assert_eq!(page.accounts[0].identity_key.as_ref(), Some(&identity));
        assert_eq!(page.accounts[0].readiness, "unknown", "identity is not login proof");
        let old = db.management_account_inventory(Some(&profile), AccountInventoryQuery { id: Some(binding.clone()), after: None })?;
        assert!(old.resources.is_empty(), "old binding cannot duplicate managed resources");
        assert_eq!(old.account.unwrap().resource_count, 0);
        assert_eq!(before, db.db.total_changes());

        // A legacy resource on the same provider account stays reachable once.
        let mut resource = db.resource_configuration(&resource_id)?;
        resource.preset_id = "additional_legacy".into();
        db.put_resource(&resource)?;
        db.db.execute("INSERT INTO owner_resource_grants VALUES(?1,?2,1,1)", params![profile, resource.id()])?;
        let page = db.management_account_inventory(Some(&profile), Default::default())?;
        assert_eq!(page.accounts.len(), 2);
        assert!(page.accounts.iter().all(|a| a.identity_key.as_ref() == Some(&identity)));
        assert_eq!(page.accounts.iter().map(|a| a.resource_count).sum::<u64>(), 2);
        let old = db.management_account_inventory(Some(&profile), AccountInventoryQuery { id: Some(binding), after: None })?;
        assert_eq!(old.resources.len(), 1);
        assert_eq!(old.resources[0].id, resource.id());

        // A grant to another profile reveals resources, never a private namespace.
        let other = "d".repeat(64);
        db.db.execute("INSERT INTO owner_resource_grants VALUES(?1,?2,1,1)", params![other, resource_id])?;
        let foreign = db.management_account_inventory(Some(&other), Default::default())?;
        assert_eq!(foreign.accounts.len(), 1);
        assert_eq!(foreign.accounts[0].kind, "resource_binding");
        assert_eq!(foreign.accounts[0].resource_count, 1);
        assert_ne!(foreign.accounts[0].identity_key.as_ref(), Some(&identity));
        assert!(matches!(db.management_account_inventory(Some(&other), AccountInventoryQuery { id: Some(account.id), after: None }), Err(Error::NotFound)));
        let encoded = serde_json::to_string(&page)?;
        let private: String = db.db.query_row("SELECT provider_digest FROM owner_resource_accounts LIMIT 1", [], |r| r.get(0))?;
        assert!(!encoded.contains(&private));
        assert!(!encoded.contains(&resource.seat_id));
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}
