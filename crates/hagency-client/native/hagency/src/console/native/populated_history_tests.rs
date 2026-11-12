//! Populated current account history, without a live provider.
use super::*;
use crate::console::native::web_overview::{WebAccountCapacityQuery,WebOverviewQuery};
use hagency_store::management::{AccountInventoryQuery,WebAlertsQuery,WebCatalogChange,WebCatalogCommand};

fn history_package()->ManagementPackage {
    let mut p=package();p.instance=1061;
    p.permissions.extend([Permission::ReadAccounts,Permission::ReadResources,Permission::ReadWebCatalog]);p
}
fn snapshots(path:&std::path::Path)->Vec<(String,Vec<Vec<String>>)> {
    let db=rusqlite::Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    ["resources","owner_web_catalog_settings","management_commands","ceiling_alerts","ceiling_alert_notes"].into_iter().map(|table|{
        let mut q=db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();let n=q.column_count();
        let rows=q.query_map([],|r|Ok((0..n).map(|c|format!("{:?}",r.get_ref(c).unwrap())).collect())).unwrap().collect::<Result<Vec<_>,_>>().unwrap();(table.into(),rows)
    }).collect()
}
#[tokio::test]
async fn native_populated_current_account_history_pagination_owner_guard_and_restart() {
    let (mut f,original)=fixture().await;
    let database=f._root.path().join("registry/domain.sqlite3");
    let scope=f.transport.scope(Permission::PublishGrants).await.unwrap();
    let id=original.clone();
    let source=f.registry.with_repository_for_tests(move |db|db.resource_configuration(&id)).await.unwrap();
    let mut setup=history_package();setup.instance+=100;setup.permissions.extend([Permission::ConfigureResources,Permission::ManageWebCatalog]);
    let manager=f.owner.management(setup).await.unwrap();
    let (context,session)=manager.authorized().await.unwrap();
    for i in 0..35 {
        if i<18 {
            session.create_resource_configuration(&context,hagency_store::management::CreateResourceConfiguration{
                command_id:format!("history-config-{i}"),source_resource_id:original.clone(),
                expected_revision:hagency_store::resource_publication_revision(&source).unwrap(),
                profile:hagency_store::ProfileChange::Preserve{},ceiling:hagency_store::CeilingChange::Preserve{},execution_policy:None,
            },Instant::now()+Duration::from_secs(30)).await.unwrap();
        } else {
            f.registry.prepare_enrollment(scope.profile.clone(),format!("history-account-{i}"),
                VerifiedProviderAccount::new("codex","https://provider.test",&format!("history-subject-{i}"),Instant::now()+Duration::from_secs(30)).unwrap(),
                source.model.clone(),source.reasoning.clone(),1000,Instant::now()+Duration::from_secs(30),
            ).unwrap().approve().await.unwrap();
        }
    }
    let offer_revision=session.web_offers(&context,original.clone()).await.unwrap().revision;
    // Persist the offer through the current profile writer and its original
    // receipt. The read-only view must never expose this private command ID.
    session.apply_web_catalog(&context,WebCatalogCommand{
        command_id:"private-current-writer".into(),resource_id:original.clone(),expected_revision:offer_revision,
        change:WebCatalogChange::Offer{role:"documentation".into(),count:Some(2),budget_cap_per_engagement:Some(500),rate_cap:Some(100),published:true},
    }).await.unwrap();
    drop(session);drop(manager);drop(scope);
    f.registry.with_repository_for_tests(|db|{
        for i in 0..19 {db.add_alert_note("fixture:original","older-owner",&format!("Retained note / 历史记录 {i}"),100+i)?;}
        Ok(())
    }).await.unwrap();
    {
        let db=rusqlite::Connection::open(&database).unwrap();
        for i in 0..20 {db.execute("INSERT INTO ceiling_alerts(dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms) VALUES(?1,?2,'Retained observation',?3,'Review','No permission change','Recheck',1,1,1)",rusqlite::params![format!("history:{i:02}"),original,r#"{"ceilingTokens":1000,"drawnTokens":1100,"credential":"history-private-sentinel"}"#]).unwrap();}
    }
    let before=snapshots(&database);f=reopen(f).await;let m=f.owner.management(history_package()).await.unwrap();
    let mut account_ids=std::collections::BTreeSet::new();let mut after=None;let mut shared=None;
    loop {
        let page=m.account_inventory(AccountInventoryQuery{id:None,after}).await.unwrap();assert!(page.accounts.len()<=16);
        for a in page.accounts {assert!(account_ids.insert(a.id.clone()));assert_eq!(a.kind,"resource_binding");assert_eq!(a.readiness,"unknown");if a.resource_count==19{shared=Some(a.id);}}
        after=page.next_after;if after.is_none(){break}
    }
    assert_eq!(account_ids.len(),18);let shared=shared.unwrap();
    let offers=m.web_offers(original.clone()).await.unwrap();assert!(offers.offers_editable);assert_eq!(offers.offer_editing_reason,None);
    let retained=offers.items.iter().find(|v|v.role=="documentation").unwrap();assert_eq!((retained.count,retained.budget_cap_per_engagement,retained.rate_cap,retained.published),(Some(2),Some(500),Some(100),true));
    // Current enrollment supplies published, qualified capacity. The owner's
    // explicit offer therefore remains advertised after a real registry reopen.
    assert!(retained.qualified);assert!(retained.catalog_published);
    assert!(!serde_json::to_string(&offers).unwrap().contains("private-current-writer"));
    let capabilities=m.web_capability(WebOverviewQuery::default()).await.unwrap();assert_eq!(capabilities["items"].as_array().unwrap().len(),16);assert!(capabilities["nextAfter"].is_string());
    for row in capabilities["items"].as_array().unwrap(){assert_eq!(row["offersEditable"],true);assert_eq!(row["offerEditingReason"],Value::Null);}
    let first=m.web_account_capacity(WebAccountCapacityQuery{id:Some(shared.clone()),..Default::default()}).await.unwrap();
    let row=&first["items"][0];assert_eq!(row["resources"].as_array().unwrap().len(),16);assert_eq!(row["aggregateComplete"],false);assert_eq!(row["oversubscribedTokens"],Value::Null);
    let next=row["nextResourceAfter"].as_str().unwrap().to_owned();
    let second=m.web_account_capacity(WebAccountCapacityQuery{id:Some(shared.clone()),resource_after:Some(next),after:None}).await.unwrap();assert_eq!(second["items"][0]["resources"].as_array().unwrap().len(),3);assert_eq!(second["items"][0]["aggregateComplete"],false);
    let usage=m.web_usage(WebOverviewQuery::default()).await.unwrap();assert_eq!(usage["totals"]["resourceCount"],36);assert_eq!(usage["totals"]["complete"],true);assert_eq!(usage["totals"]["knownFreshTokens"],Value::Null);
    let mut alert_ids=std::collections::BTreeSet::new();let mut after=None;
    loop {let page=m.web_alerts(WebAlertsQuery{after,..Default::default()}).await.unwrap();assert_eq!(page.counts.total,21);assert!(page.alerts.len()<=16);assert!(!serde_json::to_string(&page).unwrap().contains("history-private-sentinel"));for alert in page.alerts{assert!(alert_ids.insert(alert.id));}after=page.next_after;if after.is_none(){break}}
    assert_eq!(alert_ids.len(),21);
    let notes=m.resource_alert(query(&original)).await.unwrap();assert_eq!(notes.notes.len(),16);
    let tail=m.resource_alert(ResourceAlertDetailQuery{resource_id:original.clone(),id:"fixture:original".into(),after_note:notes.next_note.unwrap()}).await.unwrap();assert_eq!(tail.notes.len(),3);
    let mut denied=history_package();denied.instance+=1;denied.permissions.retain(|p|*p!=Permission::ReadAccounts);
    assert!(f.owner.management(denied).await.unwrap().web_account_capacity(Default::default()).await.is_err());
    f.current.store(false,Ordering::Release);assert!(m.web_alerts(Default::default()).await.is_err());assert!(m.web_account_capacity(Default::default()).await.is_err());f.current.store(true,Ordering::Release);
    drop(m);f=reopen(f).await;let m=f.owner.management(history_package()).await.unwrap();
    assert_eq!(m.account_inventory(AccountInventoryQuery{id:Some(shared),after:None}).await.unwrap().account.unwrap().resource_count,19);
    assert_eq!(m.web_alerts(Default::default()).await.unwrap().counts.total,21);
    assert_eq!(m.resource_alert(query(&original)).await.unwrap().notes.len(),16);
    assert_eq!(serde_json::to_value(m.web_offers(original.clone()).await.unwrap()).unwrap(),serde_json::to_value(offers).unwrap());
    assert_eq!(snapshots(&database),before,"reads/restart preserve populated original history");
    f.owner.suspend().await.unwrap();f.stop().await;
}
