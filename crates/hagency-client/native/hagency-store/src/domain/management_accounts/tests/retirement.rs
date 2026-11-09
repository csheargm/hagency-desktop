use super::*;
use crate::management::{CommandReceipt, RetireAccount};

fn prepared(db: &mut DomainRepository) -> RetireAccount {
    let input = crate::management::PrepareAccount { command_id: "prepare-retirement-original".into() };
    let receipt = CommandReceipt::new(&context(), "account.prepare", &input.command_id, &input).unwrap();
    let account = db.management_prepare_account("", receipt, || Ok(())).unwrap();
    let choice = db.account_choices().unwrap().into_iter().find(|a|a.id == account.account_id).unwrap();
    RetireAccount { command_id:"retire-original".into(), account_id:choice.id, expected_revision:choice.revision }
}
fn receipt(input: &RetireAccount) -> CommandReceipt {
    CommandReceipt::new(&context(), "account.retire", &input.command_id, input).unwrap()
}
fn count(db: &DomainRepository, table: &str) -> u64 {
    db.db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r|r.get(0)).unwrap()
}

#[test]
fn account_retirement_original_receipt_survives_reopen_and_never_logs_out_or_deletes_namespace() {
    let root=tempfile::tempdir().unwrap(); let path=root.path().join("state");
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    let input=prepared(&mut db);
    let namespace=path.join(&input.account_id); let sentinel=namespace.join("unchanged-test-data");
    std::fs::write(&sentinel,b"original private data").unwrap();
    let launch=db.managed_account(&input.account_id).unwrap().prepare_login().unwrap();
    let result=db.management_retire_account("",&input,receipt(&input),||Ok(())).unwrap();
    assert_eq!(result.state,"retired"); assert_eq!(result.provider_logout,"unobserved");
    assert_ne!(result.revision,input.expected_revision); assert!(launch.check().is_err());
    assert_eq!(db.management_retire_account("",&input,receipt(&input),||Ok(())).unwrap(),result);
    drop(db);
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    assert_eq!(db.management_retire_account("",&input,receipt(&input),||Ok(())).unwrap(),result);
    assert!(db.managed_account(&input.account_id).is_err());
    assert_eq!(count(&db,"account_logout_receipts"),1); assert_eq!(count(&db,"account_login_observations"),1);
    assert_eq!(count(&db,"account_login_attempts"),0); assert_eq!(count(&db,"resources"),0);
    assert_eq!(std::fs::read(sentinel).unwrap(),b"original private data");
    assert!(db.management_retire_account("other",&input,receipt(&input),||Ok(())).is_err());
    let mut changed=input.clone();changed.expected_revision="b".repeat(64);
    assert!(matches!(db.management_retire_account("",&changed,receipt(&changed),||Ok(())),Err(Error::Conflict)));
}

#[test]
fn account_retirement_checks_owner_revision_pending_login_and_queued_authority_before_fencing() {
    let root=tempfile::tempdir().unwrap();let mut db=DomainRepository::open_managed(&root.path().join("state"),&"a".repeat(64)).unwrap();
    let input=prepared(&mut db);let launch=db.managed_account(&input.account_id).unwrap().prepare_login().unwrap();
    assert!(db.management_retire_account("other",&input,receipt(&input),||Ok(())).is_err());
    let mut stale=input.clone();stale.expected_revision="b".repeat(64);
    assert!(matches!(db.management_retire_account("",&stale,receipt(&stale),||Ok(())),Err(Error::Conflict)));
    let checks=std::cell::Cell::new(0);
    assert!(db.management_retire_account("",&input,receipt(&input),|| {checks.set(checks.get()+1);if checks.get()==1{Ok(())}else{Err(Error::LocalAuthority)}}).is_err());
    launch.check().unwrap();
    let attempt=db.begin_account_login(&input.account_id,graphs::now_ms().unwrap()).unwrap();
    assert!(matches!(db.management_retire_account("",&input,receipt(&input),||Ok(())),Err(Error::State)));
    launch.check().unwrap();
    db.settle_account_login(attempt,crate::LoginVerdict{mode:crate::AccountReadinessMode::Unknown,provider_state:"cancelled".into(),outcome:crate::LoginOutcome::Uncertain,expires_at_ms:None},graphs::now_ms().unwrap()).unwrap();
    db.management_retire_account("",&input,receipt(&input),||Ok(())).unwrap();
    assert_eq!(count(&db,"account_logout_receipts"),1);
}

#[test]
fn account_retirement_receipt_failure_rolls_back_database_and_preserves_original_retry() {
    let root=tempfile::tempdir().unwrap();let path=root.path().join("state");
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();let input=prepared(&mut db);
    db.db.execute_batch("CREATE TRIGGER refuse_retirement BEFORE INSERT ON management_commands WHEN NEW.operation='account.retire' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;").unwrap();
    assert!(db.management_retire_account("",&input,receipt(&input),||Ok(())).is_err());
    assert!(matches!(db.account_choices().unwrap()[0].state,crate::AccountState::Active));
    assert_eq!(count(&db,"account_logout_receipts"),0);assert_eq!(count(&db,"account_login_observations"),0);
    db.db.execute_batch("DROP TRIGGER refuse_retirement;").unwrap();drop(db);
    let mut db=DomainRepository::open_managed(&path,&"a".repeat(64)).unwrap();
    assert_eq!(db.management_retire_account("",&input,receipt(&input),||Ok(())).unwrap().state,"retired");
    assert_eq!(count(&db,"account_logout_receipts"),1);
}

#[tokio::test]
async fn account_retirement_requires_current_exact_permission_and_native_confirmation_deadline() {
    let root=tempfile::tempdir().unwrap();
    let store=crate::owner_resources::adoption::TestCurrentManagement::open(&root.path().join("state"),"@owner:test").await;
    let until=Instant::now()+Duration::from_secs(60);
    let prepare=Session::new_current(store.backing.clone(),context(),[Permission::PrepareAccounts,Permission::ReadAccounts],until).unwrap();
    let prepared=prepare.prepare_account(&context(),crate::management::PrepareAccount{command_id:"prepare".into()}).await.unwrap();
    let page=prepare.account_inventory(&context(),crate::management::AccountInventoryQuery::default()).await.unwrap();
    let input=RetireAccount{command_id:"retire".into(),account_id:prepared.account_id,expected_revision:page.accounts[0].revision.clone().unwrap()};
    let reader=Session::new_current(store.backing.clone(),context(),[Permission::ReadAccounts],until).unwrap();
    assert!(reader.retire_account(&context(),input.clone(),until).await.is_err());
    let writer=Session::new_current(store.backing.clone(),context(),[Permission::RetireAccounts],until).unwrap();
    assert!(writer.retire_account(&context(),input.clone(),Instant::now()).await.is_err());
    let result=writer.retire_account(&context(),input.clone(),until).await.unwrap();
    assert_eq!(result.state,"retired");writer.retire();
    assert!(writer.retire_account(&context(),input,until).await.is_err());store.shutdown().await.unwrap();
}

#[test]
fn account_retirement_refuses_retained_legacy_association_even_with_no_visible_resources() {
    let root=tempfile::tempdir().unwrap();let mut db=DomainRepository::open_managed(&root.path().join("state"),&"a".repeat(64)).unwrap();let input=prepared(&mut db);
    db.db.execute("INSERT INTO resource_accounts VALUES('retained-hidden-preset',?1,1)",[&input.account_id]).unwrap();
    assert_eq!(db.management_account_inventory(None,AccountInventoryQuery::default()).unwrap().accounts[0].resource_count,0);
    assert!(matches!(db.management_retire_account("",&input,receipt(&input),||Ok(())),Err(Error::State)));
    db.managed_account(&input.account_id).unwrap().prepare_login().unwrap().check().unwrap();
    assert_eq!(count(&db,"account_logout_receipts"),0);
}
