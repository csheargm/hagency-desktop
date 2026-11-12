//! Real SDK loopback and SQLite original-intent recovery; no provider is started.
use super::*;
use hagency_store::management::{PrepareAccount, AccountInventoryQuery, Intent, IntentOperation};
fn account_package() -> ManagementPackage {
    ManagementPackage{digest:"c".repeat(64),instance:995,account_epoch:1,permissions:vec![Permission::PrepareAccounts,Permission::ReadAccounts,Permission::ReadIntents,Permission::AcknowledgeIntents]}
}
fn input() -> PrepareAccount {PrepareAccount{command_id:"account_original_http".into()}}
#[tokio::test]
async fn native_account_preparation_recovers_effect_without_reply_after_normal_restart() {
    let f=Fixture::new().await;
    let m=f.owner.management(account_package()).await.unwrap();
    let (context,session)=m.authorized().await.unwrap();
    session.prepare_intent(&context,Intent::new(IntentOperation::PrepareAccount,input().command_id,&input()).unwrap()).await.unwrap();
    let original=session.prepare_account(&context,input()).await.unwrap();
    assert_eq!(original.state,"active");
    assert!(m.saved_commands().await.unwrap()[0].reply.is_none());
    let Fixture{_root,owner,transport,registry,state:_,task,current}=f;
    let path=_root.path().join("host");let origin=owner.0.expected_origin.clone();
    owner.suspend().await.unwrap();drop(session);drop(m);drop(transport);drop(owner);
    let selection=crate::native_owner::CurrentProfileSelection::load_for_isolated_tests(&path,&origin,"@owner:test",registry.clone()).unwrap().unwrap();
    let owner=selection.open_with_matrix_guarded(&path,&origin,"@owner:test",Arc::new(Source(current)),|_|Ok(())).await.unwrap();
    let m=owner.management(account_package()).await.unwrap();
    let reply=m.retry_saved_command(input().command_id).await.unwrap();
    assert_eq!(reply["operation"],"prepare_account");assert_eq!(reply["data"]["accountId"],original.account_id);
    assert_eq!(m.prepare_account(input()).await.unwrap().account_id,original.account_id);
    let page=m.account_inventory(AccountInventoryQuery::default()).await.unwrap();
    let accounts=page.accounts.iter().filter(|account|account.id==original.account_id).collect::<Vec<_>>();
    assert_eq!(accounts.len(),1);assert_eq!(accounts[0].readiness,"unknown");
    m.acknowledge_command(input().command_id).await.unwrap();assert!(m.saved_commands().await.unwrap().is_empty());
    owner.suspend().await.unwrap();registry.shutdown().await.unwrap();task.abort();
}
#[tokio::test]
async fn native_account_preparation_saved_success_requires_current_exact_permission_and_sdk() {
    let f=Fixture::new().await;let m=f.owner.management(account_package()).await.unwrap();
    let original=m.prepare_account(input()).await.unwrap();
    let mut read_package=account_package();read_package.instance=996;read_package.permissions.retain(|p|*p!=Permission::PrepareAccounts);
    let reader=f.owner.management(read_package).await.unwrap();
    assert!(reader.prepare_account(input()).await.is_err());assert!(reader.retry_saved_command(input().command_id).await.is_err());
    f.current.store(false,Ordering::Release);
    assert!(m.prepare_account(input()).await.is_err(),"cached success cannot bypass current SDK scope");
    f.current.store(true,Ordering::Release);
    assert_eq!(m.prepare_account(input()).await.unwrap().account_id,original.account_id);
    assert_eq!(m.account_inventory(AccountInventoryQuery::default()).await.unwrap().accounts.iter().filter(|account|account.id==original.account_id).count(),1);
    f.owner.suspend().await.unwrap();f.stop().await;
}

#[tokio::test]
async fn native_account_preparation_recovery_future_has_bounded_stack_state() {
    let f=Fixture::new().await;let m=f.owner.management(account_package()).await.unwrap();
    let retry=m.retry_saved_command("size_only_never_polled".into());
    let bytes=std::mem::size_of_val(&retry);eprintln!("saved-command recovery future: {bytes} bytes");
    assert!(bytes <= 16384,"saved-command recovery must not grow the caller stack with every operation: {bytes}");
    drop(retry);f.owner.suspend().await.unwrap();f.stop().await;
}
