use super::*;
use crate::management::{HostContext, Permission, Session, WebCatalogChange, WebCatalogCommand, WebCatalogQuery};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

#[tokio::test]
async fn pool_retirement_preserves_expired_grants_and_unsettled_usage_after_confirmed_stop() {
    let f=fixture().await;
    let backing=resource_name_tests::active(&f).await;
    let context=HostContext::new(f.profile.account.clone(),1,"b".repeat(64),1,"retirement_device".into(),1).unwrap();
    let session=Session::new_current(backing.clone(),context.clone(),[Permission::ReadWebCatalog,Permission::ManageWebCatalog,Permission::ReadResources],Instant::now()+Duration::from_secs(120)).unwrap();
    let resource=f.resource.id();
    let revision=session.resource(&context,resource.clone()).await.unwrap().revision;
    let enrollment=||VerifiedManagementEnrollment::new(f.profile.clone(),"retirement_enrollment".into(),"retirement_contribution".into(),resource.clone(),revision.clone(),f.profile.account.clone(),"device_a".into(),1,Instant::now()+Duration::from_secs(60)).unwrap().with_admission_request("retirement_admission".into()).unwrap();
    let now=graphs::now_ms().unwrap();
    let clock=Arc::new(AtomicU64::new(now));
    let worker_clock=clock.clone();
    f.registry.0.store.call(1024,move |_|{graphs::install_test_clock(&worker_clock);Ok(())}).await.unwrap();
    let grant=f.registry.prepare_capacity_grant(enrollment(),OwnerGrantSelection{ renewal_mode: None, per_request_tokens: None,calendar:None,command_id:"retirement_consent".into(),grant_id:"retirement_grant".into(),coordinator_mxid:f.profile.account.clone(),delegation_expires_at_ms:now+60_000,allow_self_approval:false,eligible_managers:vec!["@manager:a.test".into()],allocated_tokens:500},Instant::now()+Duration::from_secs(60)).unwrap().with_admission_request("retirement_admission".into()).unwrap().approve().await.unwrap();
    let fixture:Value=serde_json::from_str(include_str!("../capacity/managed_agents/pool-agent-v1.json")).unwrap();
    let mut command:ManagedAgentReserveCommand=serde_json::from_value(fixture["command"].clone()).unwrap();
    command.command_id="retirement_reserve".into();command.request_id="retirement_request".into();command.proposed_agent_id="retirement_agent".into();command.allocation_id=command.request_id.clone();
    command.manager_mxid="@manager:a.test".into();command.decision_actor_mxid=f.profile.account.clone();command.pool_grant=Some(grant.clone());
    command.grant_receipt_digest=canonical::digest(&serde_json::to_value(&grant).unwrap()).unwrap();
    command.enrollment_id=grant.enrollment_id.clone();command.executor_device_id=grant.executor_device_id.clone();command.executor_generation=grant.executor_generation;
    command.resource_id=resource.clone();command.resource_revision=revision.clone();command.authority_revision=grant.grant_revision;command.requested_tokens=100;command.period_key=grant.period_key.clone();command.issued_at_ms=now;command.expires_at_ms=grant.delegation_expires_at_ms;
    command.definition.capability=session.resource(&context,resource.clone()).await.unwrap().capabilities[0].clone();
    command.definition_digest=canonical::digest(&serde_json::to_value(&command.definition).unwrap()).unwrap();
    let digest=canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap();
    let child=f.registry.reserve_managed_agent(VerifiedManagedAgentDelivery::for_pool(enrollment(),command,digest,true,Instant::now()+Duration::from_secs(60)).unwrap()).await.unwrap().agent_reservation_id.unwrap();
    let query=||WebCatalogQuery{resource_id:resource.clone(),after:None};
    assert_eq!(session.web_catalog(&context,query()).await.unwrap().retire_blocked_reason.as_deref(),Some("active_grant"));
    // Expiry ends new authority, but a child must still be permanently fenced.
    clock.store(grant.period_end_ms+1,Ordering::SeqCst);
    assert_eq!(session.web_catalog(&context,query()).await.unwrap().retire_blocked_reason.as_deref(),Some("active_agents"));
    let id=child.clone();
    f.registry.0.store.call(4096,move |db|{
        db.db.execute("UPDATE owner_managed_agents SET permanent_fence=1,state='fenced' WHERE id=?1",[&id])?;
        db.db.execute("UPDATE owner_managed_effects SET state='qualified' WHERE child_id=?1",[&id])?;
        db.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES('retirement_call',?1,'retirement_execution',?2,1,'unknown',100,17,0)",params![id,"a".repeat(64)])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(session.web_catalog(&context,query()).await.unwrap().retire_blocked_reason.as_deref(),Some("runtime_not_stopped"),"missing custody is not proof of exit");
    let id=child.clone();let profile=f.profile.digest.clone();
    f.registry.0.store.call(4096,move |db|{
        db.db.execute("INSERT INTO owner_shared_runtime_lifetimes SELECT ?1,?2,'retirement_assignment','retirement_runtime','{}',?3,?3,provider_digest,10000,'retirement_process' FROM owner_resource_accounts LIMIT 1",params![id,profile,"a".repeat(64)])?;Ok(())
    }).await.unwrap();
    assert_eq!(session.web_catalog(&context,query()).await.unwrap().retire_blocked_reason.as_deref(),Some("runtime_not_stopped"));
    let id=child.clone();let rid=resource.clone();
    f.registry.0.store.call(4096,move |db|{
        db.db.execute("INSERT INTO owner_shared_process_observations VALUES('retirement_stop',?1,'{\"state\":\"stopped\"}',?2,10001)",params![id,"b".repeat(64)])?;
        assert!(unsettled_exact_resource(&db.db,&rid)?);Ok(())
    }).await.unwrap();
    let before_successor=session.web_catalog(&context,query()).await.unwrap();assert!(before_successor.can_retire);
    let id=child.clone();let profile=f.profile.digest.clone();
    f.registry.0.store.call(4096,move |db|{
        db.db.execute("INSERT INTO owner_shared_successor_attempts SELECT 'retirement_successor',?1,?2,2,'retirement_runtime',?3,?3,'retirement_start','{}',?3,?3,?3,provider_digest,10001,10002,'retirement_successor_process' FROM owner_resource_accounts LIMIT 1",params![id,profile,"c".repeat(64)])?;Ok(())
    }).await.unwrap();
    assert_eq!(session.web_catalog(&context,query()).await.unwrap().retire_blocked_reason.as_deref(),Some("runtime_not_stopped"),"a stopped original does not prove its successor stopped");
    let stale=WebCatalogCommand{command_id:"retirement_stale".into(),resource_id:resource.clone(),expected_revision:before_successor.revision,change:WebCatalogChange::Retire{}};
    assert!(matches!(session.apply_web_catalog(&context,stale).await,Err(Error::State)),"mutation rechecks process custody after the page was read");
    f.registry.0.store.call(4096,move |db|{
        db.db.execute("INSERT INTO owner_shared_successor_observations VALUES('retirement_successor_stop','retirement_successor','{\"state\":\"stopped\"}',?1,10003)",["d".repeat(64)])?;Ok(())
    }).await.unwrap();
    let view=session.web_catalog(&context,query()).await.unwrap();assert!(view.can_retire);
    let command=WebCatalogCommand{command_id:"retirement_confirm".into(),resource_id:resource.clone(),expected_revision:view.revision,change:WebCatalogChange::Retire{}};
    let result=session.apply_web_catalog(&context,command.clone()).await.unwrap();assert!(result.retired);
    assert_eq!(session.apply_web_catalog(&context,command).await.unwrap(),result,"retry preserves the original result");
    assert!(session.resource(&context,resource.clone()).await.is_err(),"retired pool cannot accept new work through its owner listing");
    let receipt=serde_json::to_string(&grant).unwrap();
    f.registry.0.store.call(4096,move |db|{
        let retained:(i64,String)=db.db.query_row("SELECT active,receipt FROM owner_capacity_grants WHERE id='retirement_grant'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(retained,(1,receipt),"retirement is not grant settlement");
        let usage:(String,u64,u64,bool)=db.db.query_row("SELECT state,reserved_tokens,known_consumed_tokens,usage_complete FROM owner_managed_calls WHERE id='retirement_call'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(usage,("unknown".into(),100,17,false));
        assert!(unsettled_exact_resource(&db.db,&resource)?);
        assert!(read_resource(&db.db,&resource).is_ok(),"resource history remains available to accounting");Ok(())
    }).await.unwrap();
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn pool_retirement_keeps_recurring_authorization_after_its_initial_period() {
    let f=fixture().await;
    let backing=resource_name_tests::active(&f).await;
    let context=HostContext::new(f.profile.account.clone(),1,"b".repeat(64),1,"retirement_calendar_device".into(),1).unwrap();
    let session=Session::new_current(backing.clone(),context.clone(),[Permission::ReadWebCatalog,Permission::ManageWebCatalog,Permission::ReadResources],Instant::now()+Duration::from_secs(120)).unwrap();
    let resource=f.resource.id();
    let revision=session.resource(&context,resource.clone()).await.unwrap().revision;
    let enrollment=VerifiedManagementEnrollment::new(f.profile.clone(),"retirement_enrollment".into(),"retirement_contribution".into(),resource.clone(),revision,f.profile.account.clone(),"device_a".into(),1,Instant::now()+Duration::from_secs(60)).unwrap().with_admission_request("retirement_admission".into()).unwrap();
    let now=graphs::now_ms().unwrap();
    let clock=Arc::new(AtomicU64::new(now));let worker_clock=clock.clone();
    f.registry.0.store.call(1024,move |_|{graphs::install_test_clock(&worker_clock);Ok(())}).await.unwrap();
    let calendar=serde_json::from_value(json!({"cadence":"monthly","timeZone":"UTC"})).unwrap();
    let grant=f.registry.prepare_capacity_grant(enrollment,OwnerGrantSelection{ renewal_mode: None, per_request_tokens: None,calendar:Some(calendar),command_id:"retirement_calendar_consent".into(),grant_id:"retirement_calendar_grant".into(),coordinator_mxid:f.profile.account.clone(),delegation_expires_at_ms:now+60_000,allow_self_approval:false,eligible_managers:vec!["@manager:a.test".into()],allocated_tokens:500},Instant::now()+Duration::from_secs(60)).unwrap().approve().await.unwrap();
    clock.store(grant.period_end_ms+1,Ordering::SeqCst);
    let view=session.web_catalog(&context,WebCatalogQuery{resource_id:resource.clone(),after:None}).await.unwrap();
    assert!(!view.can_retire);
    assert_eq!(view.retire_blocked_reason.as_deref(),Some("active_grant"));
    assert!(matches!(session.apply_web_catalog(&context,WebCatalogCommand{command_id:"retirement_calendar_delete".into(),resource_id:resource,expected_revision:view.revision,change:WebCatalogChange::Retire{}}).await,Err(Error::State)));
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
