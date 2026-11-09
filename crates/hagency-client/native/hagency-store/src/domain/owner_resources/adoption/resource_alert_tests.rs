use super::*;
use crate::management::{HostContext, Permission, Session, ResourceAlert, ResourceAlertQuery, ResourceAlertDetailQuery, TriageResourceAlert};
fn context(f:&Fixture)->HostContext {HostContext::new(f.profile.account.clone(),1,"a".repeat(64),1,"alert_device".into(),1).unwrap()}
fn session(store:CurrentManagementStore,ctx:&HostContext)->Session {Session::new_current(store,ctx.clone(),[Permission::ReadAlerts,Permission::TriageAlerts],Instant::now()+Duration::from_secs(60)).unwrap()}
fn list(f:&Fixture)->ResourceAlertQuery {ResourceAlertQuery{resource_id:f.resource.id(),after:String::new(),status:None}}
fn detail(f:&Fixture,id:&str)->ResourceAlertDetailQuery {ResourceAlertDetailQuery{resource_id:f.resource.id(),id:id.into(),after_note:0}}
fn change(row:&ResourceAlert,command:&str,to:&str)->TriageResourceAlert {TriageResourceAlert{command_id:command.into(),resource_id:row.resource_id.clone(),id:row.id.clone(),expected_revision:row.revision.clone(),to:to.into(),note:Some("Checked resource usage / 已核对资源用量".into())}}
async fn seed(f:&Fixture,n:usize){let resource=f.resource.id();f.registry.0.store.call(8192,move|db|{
    for i in 0..n {db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES(?1,?2,'Synthetic quota observation',?3,'Review resource commitments','Existing work remains authorized','Drawn usage falls below the declared ceiling',1,100,100)",params![format!("test:{i:02}"),resource,r#"{"ceilingTokens":1000,"drawnTokens":1100,"overByTokens":100,"credential":"private-sentinel","home":"/private/provider"}"#])?;}
    Ok(())}).await.unwrap();}

#[tokio::test]
async fn current_web_alerts_share_profile_scope_for_rows_counts_and_revocation() {
    let f=fixture().await;
    let backing=resource_name_tests::active(&f).await;
    let ctx=context(&f);
    let s=session(backing.clone(),&ctx);
    seed(&f,2).await;
    let mut foreign=f.resource.clone();
    foreign.preset_id="foreign_alert".into();
    f.registry.0.store.call(4096,move|db|{
        db.put_resource(&foreign)?;
        db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params!["b".repeat(64),foreign.id()])?;
        db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES('foreign_alert',?1,'Private profile','{}','r','i','c',1,100,100)",[foreign.id()])?;
        Ok(())
    }).await.unwrap();
    let visible=s.web_alerts(&ctx,crate::management::WebAlertsQuery::default()).await.unwrap();
    assert_eq!(visible.counts.total,2);
    assert_eq!(visible.filtered_count,2);
    assert_eq!(visible.alerts.len(),2);
    assert!(visible.alerts.iter().all(|alert|alert.resource_id==f.resource.id()));
    let profile=f.profile.digest.clone();
    f.registry.0.store.call(1,move|db|{
        assert_eq!(db.db.execute("UPDATE owner_resource_grants SET active=0 WHERE profile=?1",[profile])?,1);
        Ok(())
    }).await.unwrap();
    let revoked=s.web_alerts(&ctx,crate::management::WebAlertsQuery::default()).await.unwrap();
    assert_eq!(revoked.counts.total,0);
    assert_eq!(revoked.filtered_count,0);
    assert!(revoked.alerts.is_empty());
    s.retire();
    assert!(s.web_alerts(&ctx,crate::management::WebAlertsQuery::default()).await.is_err());
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn resource_alert_pagination_projection_notes_and_resource_authority(){
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);let s=session(backing.clone(),&ctx);seed(&f,18).await;
    let first=s.resource_alerts(&ctx,list(&f)).await.unwrap();assert_eq!(first.alerts.len(),16);assert!(first.next_after.is_some());
    let serialized=serde_json::to_string(&first).unwrap();assert!(!serialized.contains("private-sentinel"));assert!(!serialized.contains("/private/provider"));assert_eq!(first.alerts[0].budget["overByTokens"],100);
    let mut query=list(&f);query.after=first.next_after.unwrap();let second=s.resource_alerts(&ctx,query).await.unwrap();assert_eq!(second.alerts.len(),2);assert!(second.next_after.is_none());
    let mut hidden=detail(&f,"test:00");hidden.resource_id="hidden_resource".into();assert!(s.resource_alert(&ctx,hidden).await.is_err());
    let reader=Session::new_current(backing.clone(),ctx.clone(),[Permission::ReadAlerts],Instant::now()+Duration::from_secs(60)).unwrap();assert!(matches!(reader.triage_resource_alert(&ctx,change(&first.alerts[0],"denied","acknowledged")).await,Err(Error::LocalAuthority)));
    f.registry.0.store.call(1,|db|{for i in 0..18{db.add_alert_note("test:00","operator",&format!("note-{i}"),200+i)?;}Ok(())}).await.unwrap();
    let d=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap();assert_eq!(d.notes.len(),16);assert_eq!(d.next_note,Some(16));assert_ne!(d.alert.revision,first.alerts[0].revision);
    let mut next=detail(&f,"test:00");next.after_note=16;assert_eq!(s.resource_alert(&ctx,next).await.unwrap().notes.len(),2);
    let original=s.triage_resource_alert(&ctx,change(&d.alert,"before_revoke","acknowledged")).await.unwrap();
    let profile=f.profile.digest.clone();f.registry.0.store.call(1,move|db|{db.db.execute("UPDATE owner_resource_grants SET active=0 WHERE profile=?1",[profile])?;Ok(())}).await.unwrap();
    assert!(s.resource_alerts(&ctx,list(&f)).await.is_err());assert!(s.resource_alert(&ctx,detail(&f,"test:00")).await.is_err());assert!(s.triage_resource_alert(&ctx,change(&d.alert,"before_revoke","acknowledged")).await.is_err());assert_eq!(original.status,"acknowledged");
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn resource_alert_kind_and_params_localize_without_parsing_english_or_guessing(){
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);let s=session(backing.clone(),&ctx);
    let resource=f.resource.id();
    // 2026-10-29 08:12 UTC, inside the fixture's monthly ceiling period.
    let seen:u64=1_793_261_520_000;
    f.registry.0.store.call(8192,move|db|{
        let native=serde_json::json!({"agent":resource,"presetId":"original","ceilingTokens":1000,"committedTokens":1200,"measuredTokens":null,"drawnTokens":1200,"overByTokens":200}).to_string();
        let insert="INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms,alert_type) VALUES(?1,?2,'English summary',?3,'English runbook','English impact','English recovery',1,?4,?4,?5)";
        db.db.execute(insert,params!["a_native",resource,native,seen,"agent_ceiling_overrun"])?;
        db.db.execute(insert,params!["b_transferred",resource,r#"{"ceilingTokens":1000,"drawnTokens":1200}"#,seen,"provider_rate_limit"])?;
        db.db.execute(insert,params!["c_truncated",resource,r#"{"agent":"cut","ceilingTok"#,seen,"agent_ceiling_overrun"])?;
        Ok(())
    }).await.unwrap();
    let page=s.resource_alerts(&ctx,list(&f)).await.unwrap();
    let row=|id:&str|page.alerts.iter().find(|a|a.id==id).unwrap().clone();
    let native=row("a_native");
    assert_eq!(native.kind,crate::ALERT_KIND_CEILING_OVERRUN);
    assert_eq!(serde_json::Value::Object(native.params.clone()),serde_json::json!({
        "ceilingTokens":1000,"drawnTokens":1200,"overByTokens":200,"committedTokens":1200,"measuredTokens":null,
        "percent":120,"period":"month","periodStartMs":1_790_812_800_000u64,"periodEndMs":1_793_491_200_000u64,
    }));
    // The English text stays for older clients; the kind never depends on it.
    assert_eq!((native.summary.as_str(),native.runbook.as_str()),("English summary","English runbook"));
    for id in ["b_transferred","c_truncated"] {
        let other=row(id);assert_eq!(other.kind,crate::ALERT_KIND_OTHER,"{id}");assert!(other.params.is_empty(),"{id}");
    }
    assert!(page.alerts.iter().all(|a|crate::ALERT_KINDS.contains(&a.kind.as_str())));
    // Detail and the triage result carry the same identity.
    let detail_row=s.resource_alert(&ctx,detail(&f,"a_native")).await.unwrap().alert;
    assert_eq!((detail_row.kind.as_str(),&detail_row.params),(native.kind.as_str(),&native.params));
    let triaged=s.triage_resource_alert(&ctx,change(&detail_row,"kind_triage","acknowledged")).await.unwrap();
    assert_eq!((triaged.kind.as_str(),&triaged.params),(native.kind.as_str(),&native.params));
    // A daily ceiling reports the UTC day that holds the last observation.
    let mut daily=f.resource.clone();daily.preset_id="daily_ceiling".into();
    daily.ceiling=Some(serde_json::from_value(serde_json::json!({"tokens":1000,"period":"daily"})).unwrap());
    let daily_id=daily.id();
    f.registry.0.store.call(8192,move|db|{
        db.db.execute("INSERT INTO resources(id,preset_id,config) VALUES(?1,?2,?3)",params![daily.id(),daily.preset_id,serde_json::to_string(&daily)?])?;
        db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES('daily',?1,'s',?2,'r','i','c',1,?3,?3)",params![daily.id(),r#"{"ceilingTokens":1000,"drawnTokens":1001}"#,seen])?;
        let page=db.management_resource_alerts(&ResourceAlertQuery{resource_id:daily.id(),after:String::new(),status:None},|_|Ok(()))?;
        let params=&page.alerts[0].params;
        assert_eq!((params["period"].as_str(),params["percent"].as_u64(),params["overByTokens"].as_u64()),(Some("day"),Some(100),Some(1)));
        assert_eq!((params["periodStartMs"].as_u64(),params["periodEndMs"].as_u64()),(Some(1_793_232_000_000),Some(1_793_318_400_000)));
        assert!(params["committedTokens"].is_null() && params["measuredTokens"].is_null());
        Ok(())
    }).await.unwrap();
    assert!(s.resource_alerts(&ctx,ResourceAlertQuery{resource_id:daily_id,after:String::new(),status:None}).await.is_err(),"unowned resources stay hidden");
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn resource_alert_atomic_triage_cas_suppression_original_receipt_and_restart(){
    let mut f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);let s=session(backing.clone(),&ctx);seed(&f,1).await;
    let resource_before=serde_json::to_value(f.registry.resource(&f.profile,f.resource.id()).await.unwrap()).unwrap();
    let open=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().alert;
    let suppress=change(&open,"suppress_original","suppressed");let first=s.triage_resource_alert(&ctx,suppress.clone()).await.unwrap();assert!(first.suppress_until_ms.is_some());assert_eq!(first.status,"suppressed");
    assert!(matches!(s.triage_resource_alert(&ctx,change(&open,"stale","resolved")).await,Err(Error::Conflict)));
    let assigned=s.triage_resource_alert(&ctx,change(&first,"assign_me","assigned")).await.unwrap();assert_eq!(assigned.assignee.as_deref(),Some(f.profile.account.as_str()));assert_eq!(assigned.suppress_until_ms,first.suppress_until_ms);
    let resolved=s.triage_resource_alert(&ctx,change(&assigned,"resolve","resolved")).await.unwrap();assert!(resolved.next.is_empty());assert!(resolved.resolved);
    assert_eq!(s.triage_resource_alert(&ctx,suppress.clone()).await.unwrap(),first);
    let mut forged=suppress.clone();forged.note=Some("changed".into());assert!(matches!(s.triage_resource_alert(&ctx,forged).await,Err(Error::Conflict)));
    assert!(s.triage_resource_alert(&ctx,change(&resolved,"illegal_reopen","open")).await.is_err());
    assert_eq!(s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().notes.len(),3);
    assert_eq!(serde_json::to_value(f.registry.resource(&f.profile,f.resource.id()).await.unwrap()).unwrap(),resource_before);
    s.retire();drop(s);backing.shutdown().await.unwrap();drop(backing);f.registry.shutdown().await.unwrap();
    f.registry=OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();let reopened = current_backing(&f).await;let s=session(reopened.clone(),&ctx);
    assert_eq!(s.triage_resource_alert(&ctx,suppress).await.unwrap(),first);let d=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap();assert_eq!(d.alert,resolved);assert_eq!(d.notes.len(),3);
    reopened.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn resource_alert_receipt_failure_rolls_back_and_new_observation_invalidates_review(){
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);let s=session(backing.clone(),&ctx);seed(&f,1).await;
    let open=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().alert;let input=change(&open,"rollback","acknowledged");
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER fail_alert_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='alert.triage' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;")?;Ok(())}).await.unwrap();
    assert!(s.triage_resource_alert(&ctx,input.clone()).await.is_err());let unchanged=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap();assert_eq!(unchanged.alert,open);assert!(unchanged.notes.is_empty());
    f.registry.0.store.call(1,|db|{db.db.execute_batch("DROP TRIGGER fail_alert_receipt; UPDATE ceiling_alerts SET occurrences=occurrences+1,last_seen_ms=101;")?;Ok(())}).await.unwrap();
    assert!(matches!(s.triage_resource_alert(&ctx,input).await,Err(Error::Conflict)));
    let fresh=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().alert;assert_eq!(fresh.status,"open");assert_eq!(fresh.occurrences,2);
    let original=s.triage_resource_alert(&ctx,change(&fresh,"fresh_review","acknowledged")).await.unwrap();assert_eq!(original.status,"acknowledged");
    // Original receipt survives retention of the source alert; no repeated note.
    f.registry.0.store.call(1,|db|{db.delete_alert("test:00")?;Ok(())}).await.unwrap();assert_eq!(s.triage_resource_alert(&ctx,change(&fresh,"fresh_review","acknowledged")).await.unwrap(),original);
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn resource_alert_queued_write_rechecks_retired_authority(){
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);let s=session(backing.clone(),&ctx);seed(&f,1).await;
    let open=s.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().alert;
    let (entered_tx,entered_rx)=tokio::sync::oneshot::channel();let (release_tx,release_rx)=std::sync::mpsc::channel();let store=f.registry.0.store.clone();
    let blocker=tokio::spawn(async move {store.call(1,move|_|{entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(10)).unwrap();Ok(())}).await.unwrap()});entered_rx.await.unwrap();
    let input=change(&open,"queued","acknowledged");let pending=s.triage_resource_alert(&ctx,input);tokio::pin!(pending);
    tokio::select!{ result=&mut pending=>panic!("writer must wait: {result:?}"), _=tokio::time::sleep(Duration::from_millis(20))=>{} }
    s.retire();release_tx.send(()).unwrap();assert!(pending.await.is_err());blocker.await.unwrap();
    let fresh=session(backing.clone(),&ctx);assert_eq!(fresh.resource_alert(&ctx,detail(&f,"test:00")).await.unwrap().alert,open);
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_alert_observer_is_native_only_and_prunes_only_authorized_resource() {
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;let ctx=context(&f);
    let observer=Session::new_current(backing.clone(),ctx.clone(),[Permission::ObserveResourceAlerts],Instant::now()+Duration::from_secs(60)).unwrap();
    let reader=session(backing.clone(),&ctx);
    assert!(matches!(reader.observe_resource_alert(&ctx,f.resource.id(),1_000_000_000).await,Err(Error::LocalAuthority)));
    let mut other=f.resource.clone();other.preset_id="other_owner".into();let other_id=other.id();let own=f.resource.id();
    f.registry.0.store.call(4096,move|db|{
        db.db.execute("INSERT INTO resources(id,preset_id,config) VALUES(?1,?2,?3)",params![other.id(),other.preset_id,serde_json::to_string(&other)?])?;
        for (key,id) in [("own_old",own.as_str()),("other_old",other.id().as_str())] {
            db.db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms,status,resolved_at_ms,resolved_by) VALUES(?1,?2,'old','{}','r','i','c',1,1,1,'resolved',1,'system')",params![key,id])?;
        }
        Ok(())
    }).await.unwrap();
    assert!(matches!(observer.observe_resource_alert(&ctx,other_id,1_000_000_000).await,Err(Error::LocalAuthority)));
    let result=observer.observe_resource_alert(&ctx,f.resource.id(),1_000_000_000).await.unwrap();assert_eq!(result.pruned,1);
    f.registry.0.store.call(1,|db|{
        assert_eq!(db.db.query_row("SELECT dedupe_key FROM ceiling_alerts",[],|r|r.get::<_,String>(0))?,"other_old");Ok(())
    }).await.unwrap();
    observer.retire();assert!(observer.observe_resource_alert(&ctx,f.resource.id(),1_000_000_000).await.is_err());
    backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_alert_observer_rechecks_authority_at_commit_and_rolls_back() {
    let f=fixture().await;let backing=resource_name_tests::active(&f).await;seed(&f,1).await;
    f.registry.0.store.call(1,|db|{
        db.db.execute("UPDATE ceiling_alerts SET status='resolved',resolved_at_ms=1,resolved_by='system'",[])?;
        let checks=std::cell::Cell::new(0);
        let id=db.db.query_row("SELECT resource_id FROM ceiling_alerts",[],|r|r.get::<_,String>(0))?;
        let result=db.observe_resource_alert(&id,1_000_000_000,|_|{
            checks.set(checks.get()+1);
            if checks.get()==1 {Ok(())} else {Err(Error::LocalAuthority)}
        });
        assert!(matches!(result,Err(Error::LocalAuthority)));
        assert_eq!(db.db.query_row("SELECT count(*) FROM ceiling_alerts",[],|r|r.get::<_,u64>(0))?,1);
        Ok(())
    }).await.unwrap();backing.shutdown().await.unwrap();f.registry.shutdown().await.unwrap();
}
