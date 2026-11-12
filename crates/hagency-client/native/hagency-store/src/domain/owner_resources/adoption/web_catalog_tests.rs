use super::*;
use crate::management::{HostContext,Permission,Session,WebCatalogQuery,WebCatalogCommand,WebCatalogChange};
fn context(f:&Fixture)->HostContext{HostContext::new(f.profile.account.clone(),1,"b".repeat(64),1,"web_device".into(),1).unwrap()}
fn session(b:CurrentManagementStore,c:&HostContext)->Session{Session::new_current(b,c.clone(),[Permission::ReadWebCatalog,Permission::ManageWebCatalog,Permission::ReadResources],Instant::now()+Duration::from_secs(60)).unwrap()}
fn query(f:&Fixture)->WebCatalogQuery{WebCatalogQuery{resource_id:f.resource.id(),after:None}}
async fn input(s:&Session,c:&HostContext,f:&Fixture,id:&str,change:WebCatalogChange)->WebCatalogCommand{WebCatalogCommand{command_id:id.into(),resource_id:f.resource.id(),expected_revision:s.web_catalog(c,query(f)).await.unwrap().revision,change}}
#[tokio::test]
async fn web_catalog_declared_rate_cas_replay_and_scope_isolation(){
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let before=s.resource(&c,f.resource.id()).await.unwrap();
    let original=input(&s,&c,&f,"rate_original",WebCatalogChange::RateCap{rate_cap_per_day:Some(750)}).await;
    let saved=s.apply_web_catalog(&c,original.clone()).await.unwrap();
    assert!(!saved.retired);assert!(!saved.server_auto_approval);
    let now=s.web_catalog(&c,query(&f)).await.unwrap();assert_eq!(now.rate_cap_per_day,Some(750));assert!(!now.rate_enforced);
    assert_eq!(s.resource(&c,f.resource.id()).await.unwrap().revision,before.revision);
    let mut stale=original.clone();stale.command_id="stale".into();assert!(matches!(s.apply_web_catalog(&c,stale).await,Err(Error::Conflict)));
    let clear=input(&s,&c,&f,"clear",WebCatalogChange::RateCap{rate_cap_per_day:None}).await;s.apply_web_catalog(&c,clear).await.unwrap();
    assert_eq!(s.apply_web_catalog(&c,original.clone()).await.unwrap(),saved);
    let mut changed=original;changed.change=WebCatalogChange::RateCap{rate_cap_per_day:Some(751)};assert!(matches!(s.apply_web_catalog(&c,changed).await,Err(Error::Conflict)));
    let denied=Session::new_current(b.clone(),c.clone(),[Permission::ReadWebCatalog],Instant::now()+Duration::from_secs(60)).unwrap();
    let cmd=input(&s,&c,&f,"denied",WebCatalogChange::RateCap{rate_cap_per_day:Some(1)}).await;assert!(matches!(denied.apply_web_catalog(&c,cmd).await,Err(Error::LocalAuthority)));
    let hidden="c".repeat(64);let id=f.resource.id();f.registry.0.store.call(1,move|db|{db.db.execute("INSERT INTO owner_web_catalog_settings VALUES(?1,'rate_cap',?2,'999')",params![hidden,id])?;Ok(())}).await.unwrap();
    assert_eq!(s.web_catalog(&c,query(&f)).await.unwrap().rate_cap_per_day,None);
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn web_catalog_offer_trust_paging_no_global_or_server_approval(){
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let offers=s.web_offers(&c,f.resource.id()).await.unwrap();let role=offers.items.iter().find(|r|r.qualified).unwrap().role.clone();
    let command=input(&s,&c,&f,"offer",WebCatalogChange::Offer{role:role.clone(),count:Some(2),budget_cap_per_engagement:Some(80),rate_cap:Some(40),published:true}).await;s.apply_web_catalog(&c,command).await.unwrap();
    let result=s.web_offers(&c,f.resource.id()).await.unwrap();let row=result.items.iter().find(|r|r.role==role).unwrap();assert_eq!(row.count,Some(2));assert!(row.catalog_published);
    for i in 0..18 {let command=input(&s,&c,&f,&format!("trust_{i}"),WebCatalogChange::TrustAdd{project_room_id:format!("!room{i:02}:example.org"),display_name:Some("项目".into())}).await;s.apply_web_catalog(&c,command).await.unwrap();}
    let first=s.web_trust(&c,query(&f)).await.unwrap();assert_eq!(first.items.len(),16);assert!(!first.server_auto_approval);
    let next=s.web_trust(&c,WebCatalogQuery{resource_id:f.resource.id(),after:first.next_after}).await.unwrap();assert_eq!(next.items.len(),2);assert!(next.next_after.is_none());
    let rm=input(&s,&c,&f,"remove",WebCatalogChange::TrustRemove{project_room_id:first.items[0].project_room_id.clone()}).await;let saved=s.apply_web_catalog(&c,rm.clone()).await.unwrap();assert_eq!(s.apply_web_catalog(&c,rm).await.unwrap(),saved);
    f.registry.0.store.call(1,|db|{for table in ["role_offers","room_whitelist","owner_managed_agents","engagements"]{assert_eq!(db.db.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get::<_,u64>(0))?,0)}Ok(())}).await.unwrap();
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn web_catalog_retirement_retains_history_and_original_receipt(){
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let cmd=input(&s,&c,&f,"retire",WebCatalogChange::Retire{}).await;let saved=s.apply_web_catalog(&c,cmd.clone()).await.unwrap();assert!(saved.retired);
    assert!(s.web_catalog(&c,query(&f)).await.is_err());assert_eq!(s.apply_web_catalog(&c,cmd).await.unwrap(),saved);
    let id=f.resource.id();f.registry.0.store.call(1,move|db|{assert!(read_resource(&db.db,&id).is_ok());assert_eq!(db.db.query_row("SELECT active FROM owner_resource_grants WHERE resource_id=?1",[id],|r|r.get::<_,u8>(0))?,0);Ok(())}).await.unwrap();
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn web_catalog_future_defaults_do_not_change_existing_resource_snapshot(){
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);let before=s.resource(&c,f.resource.id()).await.unwrap().revision;
    let command=input(&s,&c,&f,"default",WebCatalogChange::DefaultPolicy{yolo:true,web_search:None}).await;s.apply_web_catalog(&c,command).await.unwrap();
    assert!(s.web_catalog(&c,query(&f)).await.unwrap().default_yolo);assert_eq!(s.resource(&c,f.resource.id()).await.unwrap().revision,before);
    let id=f.resource.id();f.registry.0.store.call(1,move|db|{assert!(crate::domain::web_catalog::future_policy(&db.db,&id)?.unwrap().yolo);Ok(())}).await.unwrap();
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_catalog_network_default_is_explicit_and_legacy_policy_edits_preserve_it() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let before=s.resource(&c,f.resource.id()).await.unwrap().revision;
    let change=input(&s,&c,&f,"network_off",WebCatalogChange::DefaultPolicy{yolo:true,web_search:Some(false)}).await;
    s.apply_web_catalog(&c,change.clone()).await.unwrap();
    let view=s.web_catalog(&c,query(&f)).await.unwrap();assert!(view.default_yolo);assert!(!view.default_web_search);
    let old=input(&s,&c,&f,"legacy_policy",WebCatalogChange::DefaultPolicy{yolo:false,web_search:None}).await;
    s.apply_web_catalog(&c,old).await.unwrap();
    s.apply_web_catalog(&c,change).await.unwrap();
    let view=s.web_catalog(&c,query(&f)).await.unwrap();assert!(!view.default_yolo);assert!(!view.default_web_search);
    let on=input(&s,&c,&f,"network_on",WebCatalogChange::DefaultPolicy{yolo:false,web_search:Some(true)}).await;
    s.apply_web_catalog(&c,on).await.unwrap();
    let view=s.web_catalog(&c,query(&f)).await.unwrap();assert!(!view.default_yolo);assert!(view.default_web_search);
    assert_eq!(s.resource(&c,f.resource.id()).await.unwrap().revision,before);
    let legacy:WebCatalogChange=serde_json::from_value(serde_json::json!({"operation":"default_policy","yolo":true})).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(),serde_json::json!({"operation":"default_policy","yolo":true}));
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_catalog_assigns_only_original_unconfigured_pending_agent_and_refuses_retire_in_use(){
    use crate::domain_worker::clock_fixtures as legacy;
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let resource=f.resource.clone();let engagement=f.registry.0.store.call(4096,move|db|{
        db.register(&legacy::registration())?;
        let proof=legacy::proof(&legacy::request("bare_request","bare_agent",&resource,20));
        let engagement=db.admit(&proof,1000)?;
        db.db.execute("UPDATE engagements SET preset_id=NULL WHERE id=?1",[&engagement.id])?;
        Ok(engagement.id)
    }).await.unwrap();
    let view=s.web_catalog(&c,query(&f)).await.unwrap();assert!(!view.can_retire);assert_eq!(view.agents.len(),1);assert_eq!(view.agents[0].id,engagement);
    let retire=input(&s,&c,&f,"in_use",WebCatalogChange::Retire{}).await;assert!(matches!(s.apply_web_catalog(&c,retire).await,Err(Error::State)));
    let bind=input(&s,&c,&f,"assign_original",WebCatalogChange::AssignPreset{engagement_id:engagement.clone()}).await;
    let result=s.apply_web_catalog(&c,bind.clone()).await.unwrap();assert_eq!(result.operation,"assign_preset");assert!(s.web_catalog(&c,query(&f)).await.unwrap().agents.is_empty());assert_eq!(s.apply_web_catalog(&c,bind).await.unwrap(),result);
    let again=input(&s,&c,&f,"cannot_rebind_configured",WebCatalogChange::AssignPreset{engagement_id:engagement.clone()}).await;assert!(matches!(s.apply_web_catalog(&c,again).await,Err(Error::State)));
    let expected=f.resource.preset_id.clone();f.registry.0.store.call(1,move|db|{assert_eq!(db.db.query_row("SELECT preset_id FROM engagements WHERE id=?1",[engagement],|r|r.get::<_,String>(0))?,expected);assert_eq!(db.db.query_row("SELECT COUNT(*) FROM effects",[],|r|r.get::<_,u64>(0))?,0);Ok(())}).await.unwrap();
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_catalog_withdrawal_filters_scoped_prospective_resources_without_changing_revision() {
    use crate::management::{ResourceQuery,ResourceDetailQuery};
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let before=s.resource(&c,f.resource.id()).await.unwrap();let role=before.capabilities[0].clone();
    let withdrawal=input(&s,&c,&f,"withdraw_role",WebCatalogChange::Offer{role:role.clone(),count:None,budget_cap_per_engagement:None,rate_cap:None,published:false}).await;
    s.apply_web_catalog(&c,withdrawal).await.unwrap();
    let after=s.resource(&c,f.resource.id()).await.unwrap();assert!(!after.capabilities.contains(&role));assert_eq!(after.revision,before.revision);
    let detail=s.resource_detail(&c,ResourceDetailQuery{id:f.resource.id()}).await.unwrap();assert!(!detail.resource.capabilities.contains(&role));
    let page=s.resources(&c,ResourceQuery{after:String::new(),limit:16,framework:None,capability:Some(role.clone()),published:None}).await.unwrap();assert!(page.resources.is_empty());
    assert!(f.registry.resource(&f.profile,f.resource.id()).await.unwrap().capabilities.contains(&role),"original canonical resource and existing grants are unchanged");
    let publish=input(&s,&c,&f,"restore_role",WebCatalogChange::Offer{role:role.clone(),count:Some(1),budget_cap_per_engagement:Some(80),rate_cap:None,published:true}).await;
    s.apply_web_catalog(&c,publish).await.unwrap();
    assert!(s.resource(&c,f.resource.id()).await.unwrap().capabilities.contains(&role));
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

async fn provision_default(f:&Fixture,name:&str)->(String,String,u64) {
    use crate::domain_worker::clock_fixtures as legacy;
    let resource=f.resource.clone();let name=name.to_owned();
    f.registry.0.store.call(4096,move|db|{
        db.register(&legacy::registration())?;
        let proof=legacy::proof(&legacy::request(&format!("request_{name}"),&name,&resource,20));
        let engagement=db.admit(&proof,1000)?;
        db.approve(&format!("approve_{name}"),&proof,1000)?;
        let effect=db.claim_effect_for(&format!("provision_{}",engagement.id))?.unwrap();
        db.observe_effect(&effect.id,effect.fence,&crate::EffectOutcome::Applied{receipt:"web default provision".into()})?;
        Ok((engagement.id,effect.id,effect.fence))
    }).await.unwrap()
}
#[tokio::test]
async fn web_catalog_future_default_reaches_first_provision_and_preserves_existing_agent_policy() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let change=input(&s,&c,&f,"future_unrestricted",WebCatalogChange::DefaultPolicy{yolo:true,web_search:None}).await;s.apply_web_catalog(&c,change).await.unwrap();
    let resource=f.resource.id();
    f.registry.0.store.call(1,move|db|{db.db.execute("INSERT INTO owner_web_catalog_settings VALUES(?1,'resource_default',?2,?3)",params!["f".repeat(64),resource,"{\"yolo\":false}"])?;Ok(())}).await.unwrap();
    let first=provision_default(&f,"future_one").await;
    let change=input(&s,&c,&f,"future_standard",WebCatalogChange::DefaultPolicy{yolo:false,web_search:None}).await;s.apply_web_catalog(&c,change).await.unwrap();
    let second=provision_default(&f,"future_two").await;
    f.registry.0.store.call(4096,move|db|{
        assert_eq!(db.execution_policy(&first.0)?["executionPolicy"]["yolo"],true);
        assert_eq!(db.execution_policy(&second.0)?["executionPolicy"]["yolo"],false);
        db.observe_effect(&first.1,first.2,&crate::EffectOutcome::Applied{receipt:"web default provision".into()})?;
        assert_eq!(db.execution_policy(&first.0)?["executionPolicy"]["yolo"],true,"provision replay must not apply a newer default");
        Ok(())
    }).await.unwrap();
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_catalog_cancel_races_on_original_receipt_without_replaying_or_mutating_the_change(){
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let original=input(&s,&c,&f,"cancel_before_apply",WebCatalogChange::RateCap{rate_cap_per_day:Some(50)}).await;
    assert!(s.cancel_web_catalog(&c,original.clone()).await.unwrap().is_none());
    assert!(matches!(s.apply_web_catalog(&c,original.clone()).await,Err(Error::ManagementChangeCancelled)));
    assert!(s.cancel_web_catalog(&c,original.clone()).await.unwrap().is_none());
    let mut conflicting=original.clone();conflicting.change=WebCatalogChange::RateCap{rate_cap_per_day:Some(51)};
    assert!(matches!(s.cancel_web_catalog(&c,conflicting).await,Err(Error::Conflict)));
    assert_eq!(s.web_catalog(&c,query(&f)).await.unwrap().rate_cap_per_day,None);
    let committed=input(&s,&c,&f,"cancel_after_effect",WebCatalogChange::RateCap{rate_cap_per_day:Some(60)}).await;
    let receipt=s.apply_web_catalog(&c,committed.clone()).await.unwrap();
    assert_eq!(s.cancel_web_catalog(&c,committed.clone()).await.unwrap(),Some(receipt.clone()));
    assert_eq!(s.apply_web_catalog(&c,committed).await.unwrap(),receipt);
    // Independent sessions still arbitrate through the canonical SQLite receipt.
    let other=session(b.clone(),&c);
    let racing=input(&s,&c,&f,"race_cancel_apply",WebCatalogChange::RateCap{rate_cap_per_day:Some(70)}).await;
    let(cancelled,applied)=tokio::join!(s.cancel_web_catalog(&c,racing.clone()),other.apply_web_catalog(&c,racing.clone()));
    match(cancelled.unwrap(),applied){
        (None,Err(Error::ManagementChangeCancelled))=>assert_eq!(s.web_catalog(&c,query(&f)).await.unwrap().rate_cap_per_day,Some(60)),
        (Some(result),Ok(applied))=>{assert_eq!(result,applied);assert_eq!(s.web_catalog(&c,query(&f)).await.unwrap().rate_cap_per_day,Some(70));},
        unexpected=>panic!("inconsistent cancellation race: {unexpected:?}"),
    }
    let denied=Session::new_current(b.clone(),c.clone(),[Permission::ReadWebCatalog],Instant::now()+Duration::from_secs(30)).unwrap();
    assert!(matches!(denied.cancel_web_catalog(&c,original).await,Err(Error::LocalAuthority)));
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_catalog_model_qualification_is_independent_of_publication_and_per_resource() {
    let f=fixture().await;let b=resource_name_tests::active(&f).await;let c=context(&f);let s=session(b.clone(),&c);
    let mut low=f.resource.clone();low.model="gpt-6.1-sol".into();low.reasoning=Some("low".into());low.published=false;low.ceiling=None;
    let mut strong=low.clone();strong.preset_id="strong_model".into();strong.model="gpt-5.6-sol".into();strong.reasoning=Some("high".into());
    let low_config=low.clone();let strong_config=strong.clone();let profile=f.profile.digest.clone();
    f.registry.0.store.call(4096,move|db|{
        db.put_resource(&low_config)?;db.put_resource(&strong_config)?;
        db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params![profile,strong_config.id()])?;Ok(())
    }).await.unwrap();
    let low_page=s.web_offers(&c,low.id()).await.unwrap();
    assert_eq!(low_page.resource_qualifications,vec!["documentation"]);
    assert!(low_page.items.iter().find(|r|r.role=="coding").unwrap().qualified,"owner aggregate includes the stronger owned model");
    assert!(!low_page.items.iter().find(|r|r.role=="review").unwrap().qualified,"two GPT models do not meet cross-family requirements");
    assert!(low_page.items.iter().all(|r|!r.catalog_published),"unpublished resources are not advertised");
    let strong_page=s.web_offers(&c,strong.id()).await.unwrap();
    assert!(strong_page.resource_qualifications.contains(&"architect".into()));
    assert!(strong_page.resource_qualifications.contains(&"review".into()),"model qualification is distinct from an aggregate cross-family offer");
    let publish=input(&s,&c,&f,"offer_before_resource_publication",WebCatalogChange::Offer{role:"documentation".into(),count:None,budget_cap_per_engagement:None,rate_cap:None,published:true}).await;
    s.apply_web_catalog(&c,publish).await.unwrap();
    let offer=s.web_offers(&c,low.id()).await.unwrap().items.into_iter().find(|r|r.role=="documentation").unwrap();
    assert!(offer.qualified&&offer.published);assert!(!offer.catalog_published);
    low.published=true;let published_without_capacity=low.clone();
    f.registry.0.store.call(4096,move|db|db.put_resource(&published_without_capacity)).await.unwrap();
    assert!(!s.web_offers(&c,low.id()).await.unwrap().items.into_iter().find(|r|r.role=="documentation").unwrap().catalog_published,"qualification cannot manufacture declared capacity");
    low.ceiling=f.resource.ceiling.clone();let available=low.clone();
    f.registry.0.store.call(4096,move|db|db.put_resource(&available)).await.unwrap();
    assert!(s.web_offers(&c,low.id()).await.unwrap().items.into_iter().find(|r|r.role=="documentation").unwrap().catalog_published);
    b.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
