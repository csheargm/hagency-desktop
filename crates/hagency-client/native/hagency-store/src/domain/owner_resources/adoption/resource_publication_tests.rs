use super::*;
use crate::management::{HostContext, Permission, PublishResource, Session};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(f.profile.account.clone(),1,"b".repeat(64),1,"publication_device".into(),1).unwrap()
}
fn session(b: CurrentManagementStore, c: &HostContext) -> Session {
    Session::new_current(b,c.clone(),[Permission::ReadResources,Permission::PublishResources,Permission::ConfigureResources,Permission::ReadWebCatalog,Permission::ManageWebCatalog],Instant::now()+Duration::from_secs(60)).unwrap()
}
async fn input(s: &Session,c: &HostContext,f: &Fixture,id: &str,published: bool) -> PublishResource {
    PublishResource{command_id:id.into(),resource_id:f.resource.id(),expected_revision:s.resource(c,f.resource.id()).await.unwrap().revision,published}
}
fn copy(c: &PublishResource) -> PublishResource {serde_json::from_value(serde_json::to_value(c).unwrap()).unwrap()}

#[tokio::test]
async fn web_publication_adopted_roundtrip_replays_original_receipt_and_rolls_back_stale_revision() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let original=input(&s,&c,&f,"withdraw",false).await;
    let first=s.publish_resource(&c,copy(&original)).await.unwrap();assert!(!first.published);
    assert!(!s.resource(&c,f.resource.id()).await.unwrap().published);
    let mut stale=copy(&original);stale.command_id="stale".into();stale.published=true;
    assert!(matches!(s.publish_resource(&c,stale).await,Err(Error::Conflict)));
    let restore=input(&s,&c,&f,"restore",true).await;s.publish_resource(&c,restore).await.unwrap();
    let replay=s.publish_resource(&c,copy(&original)).await.unwrap();
    assert_eq!(serde_json::to_value(replay).unwrap(),serde_json::to_value(first).unwrap());
    assert!(s.resource(&c,f.resource.id()).await.unwrap().published,"old receipt never flips newer state");
    let mut altered=copy(&original);altered.published=true;
    assert!(matches!(s.publish_resource(&c,altered).await,Err(Error::Conflict)));
    let denied=Session::new_current(b.clone(),c.clone(),[Permission::ReadResources],Instant::now()+Duration::from_secs(60)).unwrap();
    assert!(matches!(denied.publish_resource(&c,original).await,Err(Error::LocalAuthority)));
    let failed=input(&s,&c,&f,"rollback",false).await;
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER reject_publication_receipt BEFORE INSERT ON management_commands WHEN NEW.command_id='rollback' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).await.unwrap();
    assert!(s.publish_resource(&c,failed).await.is_err());assert!(s.resource(&c,f.resource.id()).await.unwrap().published);
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_publication_shared_resource_cannot_change_another_owner_or_cross_scope() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let original=input(&s,&c,&f,"shared_withdraw",false).await;let resource=f.resource.id();
    f.registry.0.store.call(1,move|db|{db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params!["c".repeat(64),resource])?;Ok(())}).await.unwrap();
    assert!(matches!(s.publish_resource(&c,copy(&original)).await,Err(Error::State)));
    assert_eq!(s.resource(&c,f.resource.id()).await.unwrap().revision,original.expected_revision);
    let other=ProfileAuthority::new("c".repeat(64),"@other:a.test".into(),Instant::now()+Duration::from_secs(60)).unwrap();
    let foreign=f.registry.prepare_enrollment(other,"foreign_enroll".into(),provider(),"gpt-5.6-sol".into(),Some("medium".into()),700,Instant::now()+Duration::from_secs(30)).unwrap().approve().await.unwrap();
    let mut attack=copy(&original);attack.resource_id=foreign.resource_id;
    assert!(matches!(s.publish_resource(&c,attack).await,Err(Error::LocalAuthority)));
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_publication_refuses_active_grant_without_invalidating_managed_child_authority() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let resource=f.resource.id();let revision=s.resource(&c,resource.clone()).await.unwrap().revision;
    let enrollment=||VerifiedManagementEnrollment::new(f.profile.clone(),"pub_enrollment".into(),"pub_contribution".into(),resource.clone(),revision.clone(),"@coordinator:a.test".into(),"device_a".into(),1,Instant::now()+Duration::from_secs(30)).unwrap();
    let now=graphs::now_ms().unwrap();
    let grant=f.registry.prepare_capacity_grant(enrollment(),OwnerGrantSelection{ renewal_mode: None, per_request_tokens: None,calendar:None,command_id:"pub_consent".into(),grant_id:"pub_grant".into(),coordinator_mxid:"@coordinator:a.test".into(),delegation_expires_at_ms:now+60_000,allow_self_approval:false,eligible_managers:vec!["@manager:a.test".into()],allocated_tokens:500},Instant::now()+Duration::from_secs(30)).unwrap().approve().await.unwrap();
    let project=AllocationCommand{version:1,command_id:"pub_project_reserve".into(),operation:"project.reserve".into(),allocation_id:"pub_allocation".into(),project_id:"pub_project".into(),project_revision:1,project_definition_digest:"a".repeat(64),request_id:"pub_request".into(),request_revision:1,decision_actor:grant.coordinator_mxid.clone(),manager_mxid:grant.eligible_managers[0].clone(),grant_receipt:grant.clone(),requested_tokens:300,reserve_command_id:None,issued_at_ms:now.saturating_sub(1),expires_at_ms:now+60_000};
    let digest=|v:&serde_json::Value|canonical::digest(v).unwrap();
    let project_result=f.registry.apply_project_allocation(VerifiedAllocationDelivery::new(enrollment(),project.clone(),digest(&serde_json::to_value(&project).unwrap()),true,Instant::now()+Duration::from_secs(30)).unwrap()).await.unwrap();
    assert_eq!(project_result.state,"reserved");
    let role=s.resource(&c,resource.clone()).await.unwrap().capabilities[0].clone();
    let definition=ManagedAgentDefinition{name:"Publication test".into(),capability:role,description:"Synthetic child".into(),tool_mode:"none".into()};
    let command=ManagedAgentReserveCommand{renewal_mode:None,allocation_generation:None,version:1,pool_grant:None,capability:AGENT_CAPABILITY.into(),operation:"agent.reserve".into(),command_id:"pub_agent_reserve".into(),request_id:"pub_agent_request".into(),request_revision:2,proposed_agent_id:"pub_agent".into(),manager_mxid:project.manager_mxid.clone(),project_id:project.project_id.clone(),project_revision:1,setup_id:"setup".into(),setup_binding_revision:1,domain_project_id:"domain_project".into(),room_id:"!discussion:a.test".into(),authority_revision:1,decision_actor_mxid:"@admin:a.test".into(),decision_digest:"d".repeat(64),allocation_id:project.allocation_id.clone(),allocation_receipt_digest:digest(&serde_json::to_value(&project_result).unwrap()),grant_receipt_digest:digest(&serde_json::to_value(&grant).unwrap()),enrollment_id:grant.enrollment_id.clone(),executor_device_id:grant.executor_device_id.clone(),executor_generation:1,resource_id:resource.clone(),resource_revision:revision.clone(),definition_digest:digest(&serde_json::to_value(&definition).unwrap()),definition,requested_tokens:100,period_key:grant.period_key.clone(),issued_at_ms:now.saturating_sub(1),expires_at_ms:now+60_000};
    let child=f.registry.reserve_managed_agent(VerifiedManagedAgentDelivery::new(enrollment(),command.clone(),digest(&serde_json::to_value(&command).unwrap()),project,true,Instant::now()+Duration::from_secs(30)).unwrap()).await.unwrap().agent_reservation_id.unwrap();
    let assignment=||ManagedAssignmentAuthority::fixture(enrollment(),child.clone(),"pub_assignment".into(),1,Instant::now()+Duration::from_secs(30));
    assert!(matches!(f.registry.claim_managed_provision(assignment()).await.unwrap(),ManagedProvisionClaim::Issued(_)));
    let original=input(&s,&c,&f,"active_withdraw",false).await;
    assert!(matches!(s.publish_resource(&c,original).await,Err(Error::State)));
    assert_eq!(s.resource(&c,resource.clone()).await.unwrap().revision,revision);
    // This runs the real child assignment/custody checks, including full pinned
    // resource revision; unchanged rows alone would not prove usable authority.
    assert!(matches!(f.registry.claim_managed_provision(assignment()).await.unwrap(),ManagedProvisionClaim::History(_)));
    // The same provider account/seat may also contain unused configurations.
    // Real configuration creation preserves the shared seat and adds no quota.
    let sibling=s.create_resource_configuration(&c,crate::management::CreateResourceConfiguration{
        command_id:"unused_sibling".into(),source_resource_id:resource.clone(),expected_revision:revision.clone(),
        profile:crate::ProfileChange::Preserve{},ceiling:crate::CeilingChange::Monthly{tokens:0u64.try_into().unwrap()},execution_policy:None,
    },Instant::now()+Duration::from_secs(30)).await.unwrap();
    let sibling_id=sibling.resource_id.clone();let original_id=resource.clone();
    f.registry.0.store.call(1,move|db|{
        let a=read_resource(&db.db,&original_id)?;let b=read_resource(&db.db,&sibling_id)?;
        assert_eq!(a.seat_id,b.seat_id);assert_ne!(a.preset_id,b.preset_id);
        assert!(unsettled_resource(&db.db,&b)?,"the former account-wide gate reproduces the bug");
        assert!(!unsettled_exact_resource(&db.db,&sibling_id)?);
        Ok(())
    }).await.unwrap();
    let withdrawn=s.publish_resource(&c,PublishResource{command_id:"sibling_withdraw".into(),resource_id:sibling.resource_id.clone(),expected_revision:sibling.revision,published:false}).await.unwrap();
    assert!(!withdrawn.published);
    let published=s.publish_resource(&c,PublishResource{command_id:"sibling_republish".into(),resource_id:sibling.resource_id.clone(),expected_revision:withdrawn.revision,published:true}).await.unwrap();
    assert!(published.published);
    let view=s.web_catalog(&c,crate::management::WebCatalogQuery{resource_id:sibling.resource_id.clone(),after:None}).await.unwrap();
    assert!(view.can_retire);
    let retired=s.apply_web_catalog(&c,crate::management::WebCatalogCommand{command_id:"sibling_retire".into(),resource_id:sibling.resource_id.clone(),expected_revision:view.revision,change:crate::management::WebCatalogChange::Retire{}}).await.unwrap();
    assert!(retired.retired);
    assert_eq!(s.resource(&c,resource.clone()).await.unwrap().revision,revision);
    assert!(matches!(f.registry.claim_managed_provision(assignment()).await.unwrap(),ManagedProvisionClaim::History(_)),"sibling publication and retirement retain actual active-child authority");
    // Even an inactive parent cannot make its own unresolved issued child safe
    // to reinterpret; only the unrelated sibling is exempted.
    f.registry.0.store.call(1,|db|{db.db.execute("UPDATE owner_capacity_grants SET active=0 WHERE id='pub_grant'",[])?;Ok(())}).await.unwrap();
    assert!(matches!(s.publish_resource(&c,input(&s,&c,&f,"inactive_unsettled",false).await).await,Err(Error::State)));
    assert!(!s.web_catalog(&c,crate::management::WebCatalogQuery{resource_id:resource.clone(),after:None}).await.unwrap().can_retire);
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
