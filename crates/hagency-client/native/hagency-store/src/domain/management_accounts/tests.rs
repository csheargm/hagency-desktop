use super::*;
use crate::management::{HostContext, Permission, Session};
use std::time::{Duration, Instant};

fn context() -> HostContext {
    HostContext::new(
        "@owner:test".into(),
        1,
        "a".repeat(64),
        1,
        "device".into(),
        1,
    )
    .unwrap()
}

#[test]
fn account_inventory_old_bindings_are_private_stable_and_do_not_duplicate_managed_members(){
    let root=tempfile::tempdir().unwrap();let path=root.path().join("first");let profile="a".repeat(64);
    let mut db=DomainRepository::open_managed(&path,&profile).unwrap();
    let seat=Seat{id:"old-private-seat".into(),declaration:None};db.put_seat(&seat).unwrap();
    let resource=|preset:&str|serde_json::from_value::<Resource>(json!({"presetId":preset,"seatId":seat.id,"framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","published":false})).unwrap();
    let old=resource("old");let managed=resource("managed");db.put_resource(&old).unwrap();db.put_resource(&managed).unwrap();
    let account=db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    db.db.execute("INSERT INTO resource_accounts VALUES(?1,?2,1)",params![managed.preset_id,account.id]).unwrap();
    let before=db.db.total_changes();
    let page=db.management_account_inventory(None,Default::default()).unwrap();assert_eq!(page.accounts.len(),2);
    let bound=page.accounts.iter().find(|a|a.kind=="resource_binding").unwrap();let id=bound.id.clone();assert_eq!(bound.resource_count,1);assert_eq!(bound.readiness,"unknown");
    assert!(!serde_json::to_string(&page).unwrap().contains(&seat.id));
    let members=db.management_account_inventory(None,AccountInventoryQuery{id:Some(id.clone()),after:None}).unwrap();assert_eq!(members.resources.len(),1);assert_eq!(members.resources[0].id,old.id());
    assert_eq!(page.accounts.iter().find(|a|a.id==account.id).unwrap().resource_count,1);assert_eq!(db.db.total_changes(),before);
    drop(db);let db=DomainRepository::open_managed(&path,&profile).unwrap();assert!(db.management_account_inventory(None,AccountInventoryQuery{id:Some(id.clone()),after:None}).is_ok());
    let mut other=DomainRepository::open_managed(&root.path().join("second"),&"b".repeat(64)).unwrap();other.put_seat(&seat).unwrap();other.put_resource(&old).unwrap();
    assert!(matches!(other.management_account_inventory(None,AccountInventoryQuery{id:Some(id),after:None}),Err(Error::NotFound)));
}

#[tokio::test]
async fn account_inventory_retains_namespace_states_and_latest_readiness_without_effects() {
    let root = tempfile::tempdir().unwrap();
    let mut db =
        DomainRepository::open_managed(&root.path().join("state"), &"a".repeat(64)).unwrap();
    let original = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    let active = db.materialize_account(&original.id).unwrap();
    let now = graphs::now_ms().unwrap();
    db.db.execute("INSERT INTO account_login_observations VALUES('observation_original',?1,1,1,?2,?3,'subscription','signed_in','observed')",params![active.id,now,now+60_000]).unwrap();
    let before = db.db.total_changes();
    let page = db
        .management_account_inventory(None, AccountInventoryQuery::default())
        .unwrap();
    assert_eq!(page.accounts.len(), 1);
    assert_eq!(page.accounts[0].id, active.id);
    assert_eq!(page.accounts[0].readiness, "subscription");
    assert_eq!(page.accounts[0].revision.as_ref(), Some(&active.revision));
    assert_eq!(
        db.db.total_changes(),
        before,
        "reads do not refresh login or alter state"
    );
    let json = serde_json::to_value(&page).unwrap();
    for private in [
        "seatId",
        "namespace_identity",
        "credentialRef",
        "authentication",
        "quota",
        "identity_tuple",
    ] {
        assert!(json["accounts"][0].get(private).is_none());
    }
    db.db.execute("INSERT INTO account_login_observations VALUES('observation_later',?1,1,2,?2,?3,'unknown','unobserved','uncertain')",params![active.id,now+1,now+60_000]).unwrap();
    assert_eq!(
        db.management_account_inventory(None, AccountInventoryQuery::default())
            .unwrap()
            .accounts[0]
            .readiness,
        "unknown"
    );
    db.db
        .execute("UPDATE account_login_observations SET expires_at_ms=0,outcome='observed',mode='subscription'", [])
        .unwrap();
    assert_eq!(
        db.management_account_inventory(None, AccountInventoryQuery::default())
            .unwrap()
            .accounts[0]
            .readiness,
        "unknown"
    );
    let detail = db.management_account_inventory(None, AccountInventoryQuery {
        id: Some(active.id), after: None,
    }).unwrap();
    assert_eq!(detail.account.unwrap().state, "active");
    assert!(detail.accounts.is_empty());

}

#[test]
fn account_inventory_preserves_uncertain_preparation_and_bounded_input() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    let mut db = DomainRepository::open_managed(&path, &"a".repeat(64)).unwrap();
    let original = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    crate::private::create_directory_new(&path.join(&original.id)).unwrap();
    assert!(db.materialize_account(&original.id).is_err());
    assert_eq!(
        db.management_account_inventory(None, AccountInventoryQuery::default())
            .unwrap()
            .accounts[0]
            .state,
        "uncertain"
    );
    assert!(matches!(
        db.management_account_inventory(
            None,
            AccountInventoryQuery {
                id: Some("missing".into()),
                after: None
            }
        ),
        Err(Error::NotFound)
    ));
    for id in ["", "../outside", "bad\nkey"] {
        assert!(
            AccountInventoryQuery {
                id: Some(id.into()),
                after: None
            }
            .validate()
            .is_err()
        );
    }
}

#[tokio::test]
async fn account_preparation_original_identity_permissions_and_reopen() {
    use crate::management::PrepareAccount;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    let store = crate::owner_resources::adoption::TestCurrentManagement::open(&path,"@owner:test").await;
    let reader = Session::new_current(store.backing.clone(),context(),[Permission::ReadAccounts],Instant::now()+Duration::from_secs(60)).unwrap();
    let input = || PrepareAccount{command_id:"original_prepare".into()};
    assert!(reader.prepare_account(&context(),input()).await.is_err());
    assert!(reader.account_inventory(&context(),AccountInventoryQuery::default()).await.unwrap().accounts.is_empty());
    let writer = Session::new_current(store.backing.clone(),context(),[Permission::PrepareAccounts],Instant::now()+Duration::from_secs(60)).unwrap();
    let prepared = writer.prepare_account(&context(),input()).await.unwrap();
    assert_eq!(prepared.state,"active");
    assert!(path.join("canonical").join(&prepared.account_id).is_dir());
    assert_eq!(writer.prepare_account(&context(),input()).await.unwrap().account_id,prepared.account_id);
    assert!(writer.account_inventory(&context(),AccountInventoryQuery::default()).await.is_err(),"prepare does not grant inventory reads");
    let page=reader.account_inventory(&context(),AccountInventoryQuery::default()).await.unwrap();
    assert_eq!(page.accounts.len(),1); assert_eq!(page.accounts[0].readiness,"unknown");
    writer.retire(); assert!(writer.prepare_account(&context(),input()).await.is_err());
    store.shutdown().await.unwrap(); drop(writer); drop(reader); drop(store);
    let store=crate::owner_resources::adoption::TestCurrentManagement::open(&path,"@owner:test").await;
    let restored = Session::new_current(store.backing.clone(),context(),[Permission::PrepareAccounts,Permission::ReadAccounts],Instant::now()+Duration::from_secs(60)).unwrap();
    let result=restored.prepare_account(&context(),input()).await.unwrap();
    assert_eq!(result.account_id,prepared.account_id);assert_eq!(result.state,"active");
    assert_eq!(restored.account_inventory(&context(),AccountInventoryQuery::default()).await.unwrap().accounts.len(),1);
    store.shutdown().await.unwrap();
}

#[test]
fn account_preparation_restart_after_reservation_never_rearms_filesystem_work() {
    use crate::management::{CommandReceipt,PrepareAccount};
    let root=tempfile::tempdir().unwrap();let path=root.path().join("state");
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    let input=PrepareAccount{command_id:"lost_original_reply".into()};
    let receipt=||CommandReceipt::new(&context(),"account.prepare",&input.command_id,&input).unwrap();
    let reserved=db.reserve_account_checked(crate::ACCOUNT_PROFILE,|tx,choice|{
        tx.execute("INSERT INTO management_account_profiles VALUES(?1,'')",[&choice.id])?;
        receipt().record(tx,&ReservedAccount{account_id:choice.id.clone()})
    }).unwrap();
    assert!(!path.join(&reserved.id).exists());drop(db);
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();let before=db.db.total_changes();
    let recovered=db.management_prepare_account("",receipt(),||Ok(())).unwrap();
    assert_eq!(recovered.account_id,reserved.id);assert_eq!(recovered.state,"uncertain");
    assert_eq!(db.db.total_changes(),before);assert!(!path.join(&reserved.id).exists());
    assert_eq!(db.management_account_inventory(None,AccountInventoryQuery::default()).unwrap().accounts[0].state,"uncertain");
    let forged=CommandReceipt::new(&context(),"different.operation",&input.command_id,&input).unwrap();
    assert!(matches!(db.management_prepare_account("",forged,||Ok(())),Err(Error::Conflict)));
    assert_eq!(db.account_choices().unwrap().len(),1);
}

#[test]
fn account_preparation_rechecks_authority_inside_atomic_reservation() {
    use crate::management::{CommandReceipt,PrepareAccount};
    use std::cell::Cell;
    let root=tempfile::tempdir().unwrap();let path=root.path().join("state");
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    let input=PrepareAccount{command_id:"authority_changed".into()};let checks=Cell::new(0);
    let receipt=CommandReceipt::new(&context(),"account.prepare",&input.command_id,&input).unwrap();
    assert!(matches!(db.management_prepare_account("",receipt,||{checks.set(checks.get()+1);if checks.get()==1{Ok(())}else{Err(Error::LocalAuthority)}}),Err(Error::LocalAuthority)));
    assert!(db.account_choices().unwrap().is_empty());
    for table in ["management_account_profiles","management_commands","account_identity_key"] {
        assert_eq!(db.db.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get::<_,u64>(0)).unwrap(),0);
    }
}

#[test]
fn account_preparation_schema_upgrade_does_not_claim_existing_namespaces() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("state");
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    let account=db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();db.materialize_account(&account.id).unwrap();
    db.db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_namespace_enrollments; DROP TABLE owner_resource_namespaces; DROP TABLE management_account_profiles; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=90;").unwrap();drop(db);
    let db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    assert_eq!(db.db.pragma_query_value(None,"user_version",|r|r.get::<_,i32>(0)).unwrap(),crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(db.account_choices().unwrap()[0].id,account.id);
    assert!(db.management_account_inventory(Some(&"b".repeat(64)),AccountInventoryQuery::default()).unwrap().accounts.is_empty());
    assert_eq!(db.management_account_inventory(None,AccountInventoryQuery::default()).unwrap().accounts.len(),1);
}

mod retirement;

#[test]
fn account_inventory_groups_only_consistent_verified_provider_identities() {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("state"), &"a".repeat(64)).unwrap();
    let first = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    let second = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    let unused = db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    for (i, kind) in [(1, "verified_provider"), (2, "explicit_owner_mapping")] {
        let seat = Seat { id: format!("private_seat_{i}"), declaration: None };
        db.put_seat(&seat).unwrap();
        db.db.execute("INSERT INTO owner_resource_accounts VALUES(?1,?2,?3)", params![i.to_string().repeat(64), seat.id, kind]).unwrap();
    }
    let put = |db: &mut DomainRepository, preset: &str, seat: &str, account: &str| {
        let resource: Resource = serde_json::from_value(json!({"presetId":preset,"seatId":seat,"framework":"codex","model":"test","reasoning":"medium","published":false})).unwrap();
        db.put_resource(&resource).unwrap();
        db.db.execute("INSERT INTO resource_accounts VALUES(?1,?2,1)", params![preset, account]).unwrap();
    };
    put(&mut db, "first", "private_seat_1", &first.id);
    put(&mut db, "second", "private_seat_1", &second.id);
    let page = db.management_account_inventory(None, Default::default()).unwrap();
    let identity = |id: &str| page.accounts.iter().find(|a|a.id==id).unwrap().identity_key.clone();
    assert!(identity(&first.id).is_some());
    assert_eq!(identity(&first.id), identity(&second.id));
    assert!(identity(&unused.id).is_none());
    put(&mut db, "unverified", "private_seat_2", &second.id);
    let page = db.management_account_inventory(None, Default::default()).unwrap();
    assert!(page.accounts.iter().find(|a|a.id==second.id).unwrap().identity_key.is_none(), "inconsistent or unverified records cannot merge accounts");
}
