//! Real SDK loopback and SQLite recovery; no provider child or external login.
use super::*;
use hagency_store::management::{PrepareAccount, RetireAccount, AccountInventoryQuery, Intent, IntentOperation};
fn package() -> ManagementPackage {
    ManagementPackage { digest:"c".repeat(64), instance:998, account_epoch:1, permissions:vec![Permission::PrepareAccounts,Permission::RetireAccounts,Permission::ReadAccounts,Permission::ReadIntents,Permission::AcknowledgeIntents] }
}
fn until() -> Instant { Instant::now()+Duration::from_secs(30) }
async fn prepare(m: &NativeManagement) -> RetireAccount {
    let a=m.prepare_account(PrepareAccount{command_id:"prepare-retire-http".into()}).await.unwrap();
    let page=m.account_inventory(AccountInventoryQuery{id:Some(a.account_id.clone()),after:None}).await.unwrap();
    RetireAccount { command_id:"retire-http-original".into(), account_id:a.account_id, expected_revision:page.account.unwrap().revision.unwrap() }
}
#[tokio::test]
async fn native_account_retirement_recovers_original_effect_after_normal_restart() {
    let f=Fixture::new().await;let m=f.owner.management(package()).await.unwrap();let input=prepare(&m).await;
    let (context,session)=m.authorized().await.unwrap();
    session.prepare_intent(&context,Intent::new(IntentOperation::RetireAccount,input.command_id.clone(),&input).unwrap()).await.unwrap();
    let original=session.retire_account(&context,input.clone(),until()).await.unwrap();
    assert!(m.saved_commands().await.unwrap().iter().find(|r|r.command_id==input.command_id).unwrap().reply.is_none());
    drop(session);drop(m);
    let f=f.reopen().await;
    let m=f.owner.management(package()).await.unwrap();
    let reply=m.retry_saved_command(input.command_id.clone()).await.unwrap();
    assert_eq!(reply["operation"],"retire_account");assert_eq!(reply["data"],serde_json::to_value(&original).unwrap());
    assert_eq!(m.retire_account(input.clone(),until()).await.unwrap(),original);
    let row=m.account_inventory(AccountInventoryQuery{id:Some(input.account_id),after:None}).await.unwrap().account.unwrap();
    assert_eq!(row.state,"retired");assert_eq!(row.readiness,"unknown");assert_eq!(row.resource_count,0);
    f.owner.suspend().await.unwrap();f.stop().await;
}
#[tokio::test]
async fn native_account_retirement_native_deadline_current_grant_and_sdk_guard_saved_success() {
    let f=Fixture::new().await;let m=f.owner.management(package()).await.unwrap();let input=prepare(&m).await;
    assert_eq!(m.retire_account(input.clone(),Instant::now()).await.unwrap_err().code,"provider_confirmation_expired");
    assert!(!m.saved_commands().await.unwrap().iter().any(|r|r.command_id==input.command_id));
    let mut reader=package();reader.instance=999;reader.permissions.retain(|p|*p!=Permission::RetireAccounts);
    let reader=f.owner.management(reader).await.unwrap();assert!(reader.retire_account(input.clone(),until()).await.is_err());
    let original=m.retire_account(input.clone(),until()).await.unwrap();assert_eq!(original.provider_logout,"unobserved");
    assert!(reader.retry_saved_command(input.command_id.clone()).await.is_err());
    f.current.store(false,Ordering::Release);assert!(m.retire_account(input.clone(),until()).await.is_err());
    f.current.store(true,Ordering::Release);assert_eq!(m.retire_account(input,until()).await.unwrap(),original);
    f.owner.suspend().await.unwrap();f.stop().await;
}
