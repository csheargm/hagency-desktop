//! Actual local HTTP identity and SQLite, with synthetic alerts and no provider.
#[path = "populated_history_tests.rs"]
mod populated_history_tests;
use super::*;
use hagency_store::management::{ResourceAlertDetailQuery,ResourceAlertQuery,TriageResourceAlert,Intent,IntentOperation};
fn package()->ManagementPackage {ManagementPackage{digest:"f".repeat(64),instance:1040,account_epoch:1,permissions:vec![Permission::ReadAlerts,Permission::TriageAlerts,Permission::ReadIntents,Permission::AcknowledgeIntents]}}
async fn reopen(f:Fixture)->Fixture { f.reopen().await }
async fn fixture()->(Fixture,String){
    let f=Fixture::new().await;
    let resource=f.state.lock().unwrap().grant.as_ref().unwrap().resource_id.clone();
    let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES('fixture:original',?1,'Synthetic observation','{}','Review commitments','No permission change','Recheck observed usage',1,1,1)",[&resource]).unwrap();drop(db);
    (reopen(f).await,resource)
}
fn query(id:&str)->ResourceAlertDetailQuery {ResourceAlertDetailQuery{resource_id:id.into(),id:"fixture:original".into(),after_note:0}}
#[tokio::test]
async fn native_resource_alert_original_effect_missing_reply_and_normal_restart(){
    let (mut f,id)=fixture().await;let m=f.owner.management(package()).await.unwrap();
    let row=m.resource_alert(query(&id)).await.unwrap().alert;
    let input=TriageResourceAlert{command_id:"original_alert_triage".into(),resource_id:id.clone(),id:row.id,expected_revision:row.revision,to:"acknowledged".into(),note:Some("Observed / 已观测".into())};
    let (context,session)=m.authorized().await.unwrap();session.prepare_intent(&context,Intent::new(IntentOperation::TriageResourceAlert,input.command_id.clone(),&input).unwrap()).await.unwrap();
    let original=session.triage_resource_alert(&context,input.clone()).await.unwrap();assert!(m.saved_commands().await.unwrap().iter().find(|r|r.command_id==input.command_id).unwrap().reply.is_none());
    drop(session);drop(m);f=reopen(f).await;let m=f.owner.management(package()).await.unwrap();
    let reply=m.retry_saved_command(input.command_id.clone()).await.unwrap();assert_eq!(reply["operation"],"triage_resource_alert");assert_eq!(reply["data"],serde_json::to_value(&original).unwrap());
    assert_eq!(m.triage_resource_alert(input.clone()).await.unwrap(),original);let current=m.resource_alert(query(&id)).await.unwrap();assert_eq!(current.notes.len(),1);assert_eq!(current.notes[0].author,"@owner:test");
    let mut reader=package();reader.instance+=1;reader.permissions.retain(|p|*p!=Permission::TriageAlerts);let reader=f.owner.management(reader).await.unwrap();assert!(reader.retry_saved_command(input.command_id.clone()).await.is_err());
    f.current.store(false,Ordering::Release);assert!(m.triage_resource_alert(input.clone()).await.is_err());assert!(m.resource_alert(query(&id)).await.is_err());f.current.store(true,Ordering::Release);assert_eq!(m.triage_resource_alert(input).await.unwrap(),original);
    f.owner.suspend().await.unwrap();f.stop().await;
}
#[tokio::test]
async fn native_resource_alert_conflict_is_known_only_before_original_effect(){
    let (f,id)=fixture().await;let m=f.owner.management(package()).await.unwrap();
    let row=m.resource_alerts(ResourceAlertQuery{resource_id:id.clone(),after:String::new(),status:None}).await.unwrap().alerts.remove(0);
    let mut input=TriageResourceAlert{command_id:"alert_ack".into(),resource_id:id.clone(),id:row.id,expected_revision:row.revision,to:"acknowledged".into(),note:None};m.triage_resource_alert(input.clone()).await.unwrap();
    input.command_id="stale_review".into();assert_eq!(m.triage_resource_alert(input.clone()).await.unwrap_err().code,"management_revision_conflict");let saved=m.saved_commands().await.unwrap();assert!(saved.iter().any(|r|r.command_id==input.command_id && r.reply.is_some()));
    input.command_id="unconfirmed_original".into();let (ctx,s)=m.authorized().await.unwrap();s.prepare_intent(&ctx,Intent::new(IntentOperation::TriageResourceAlert,input.command_id.clone(),&input).unwrap()).await.unwrap();assert_eq!(m.triage_resource_alert(input.clone()).await.unwrap_err().code,"management_outcome_unknown");assert!(m.saved_commands().await.unwrap().iter().find(|r|r.command_id==input.command_id).unwrap().reply.is_none());
    f.owner.suspend().await.unwrap();f.stop().await;
}

#[tokio::test]
async fn native_resource_alert_observer_reopens_without_miniapp_and_drains_on_suspend() {
    use std::time::Duration;
    let (f,resource)=fixture().await;
    let Fixture{_root,owner,transport,registry,state,task,current}=f;
    let origin=owner.0.expected_origin.clone();
    let database=_root.path().join("registry/domain.sqlite3");
    owner.suspend().await.unwrap();drop(transport);drop(owner);
    {
        let db=rusqlite::Connection::open(&database).unwrap();
        db.execute("UPDATE ceiling_alerts SET status='resolved',resolved_at_ms=1,resolved_by='system' WHERE resource_id=?1",[&resource]).unwrap();
    }
    let path=_root.path().join("host");
    let selection=crate::native_owner::CurrentProfileSelection::load_for_isolated_tests(&path,&origin,"@owner:test",registry.clone()).unwrap().unwrap();
    let owner=selection.open_with_matrix_guarded(&path,&origin,"@owner:test",Arc::new(Source(current.clone())),|_|Ok(())).await.unwrap();
    assert!(owner.0.management.has_store().await,"Current profile reopens its canonical backing before any UI management package");
    assert_eq!(owner.0.management.live_scope_count(),0,"No UI management package was opened after restart");
    crate::console::native::alert_observer::start_with_timing(&owner,Duration::from_millis(10),Duration::from_secs(60));
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let count=rusqlite::Connection::open(&database).unwrap().query_row("SELECT count(*) FROM ceiling_alerts",[],|r|r.get::<_,u64>(0)).unwrap();
            if count==0 {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(owner.0.management.has_store().await);
    owner.suspend().await.unwrap();
    // Normal closure joins the original producer and writer. New durable data
    // added only after that close cannot be touched by a detached old worker.
    {
        let db=rusqlite::Connection::open(&database).unwrap();
        db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms,status,resolved_at_ms,resolved_by) VALUES('after_suspend',?1,'old','{}','r','i','c',1,1,1,'resolved',1,'system')",[resource]).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(rusqlite::Connection::open(&database).unwrap().query_row("SELECT count(*) FROM ceiling_alerts",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    registry.shutdown().await.unwrap();task.abort();drop(state);
}
