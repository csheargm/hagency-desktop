//! Current canonical management fixtures. No source ledger, archive or importer.
use super::*;
struct Fixture {
    _root:tempfile::TempDir,
    registry:OwnerResourceRegistry,
    journal:PathBuf,
    profile:ProfileAuthority,
    resource:Resource,
}
fn authority(f:&Fixture)->ProfileInitializationAuthority {
    ProfileInitializationAuthority::new(f.registry.clone(),f.profile.clone(),Instant::now()+Duration::from_secs(30),||true).unwrap()
}
fn provider()->VerifiedProviderAccount {
    VerifiedProviderAccount::new("codex","https://provider.test","synthetic_stable_account",Instant::now()+Duration::from_secs(30)).unwrap()
}
async fn fixture()->Fixture {
    let root=tempfile::tempdir().unwrap();
    let registry=OwnerResourceRegistry::open_root(&root.path().join("owner")).unwrap();
    let journal=root.path().join("journal");
    let profile=ProfileAuthority::new("a".repeat(64),"@owner:a.test".into(),Instant::now()+Duration::from_secs(120)).unwrap();
    let resource:Resource=serde_json::from_value(json!({"presetId":"original","seatId":"seat_current_test","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","published":true,"ceiling":{"tokens":1000,"period":"monthly"}})).unwrap();
    let f=Fixture{_root:root,registry,journal,profile,resource};
    let journal=crate::management::ServerManagementJournal::open(&f.journal,&f.profile.digest,&f.profile.account).unwrap();
    f.registry.initialize_current_profile(authority(&f),"current_test".into(),journal.clone()).await.unwrap();
    journal.shutdown().await.unwrap();
    // Seed current canonical resources through the real repository worker. This
    // is test data, never an executable source-import authority or protocol.
    let resource=f.resource.clone();let profile=f.profile.digest.clone();let provider=provider().digest;
    f.registry.0.store.call(4096,move|db|{
        let seat:Seat=serde_json::from_value(json!({"id":resource.seat_id,"declaration":{"quotaTokens":700,"period":"monthly"}}))?;
        db.put_seat(&seat)?;db.put_resource(&resource)?;
        db.db.execute("INSERT INTO owner_resource_accounts VALUES(?1,?2,'verified_provider')",params![provider,resource.seat_id])?;
        db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",params![profile,resource.id()])?;
        Ok(())
    }).await.unwrap();
    f
}
async fn current_backing(f:&Fixture)->CurrentManagementStore {
    let journal=crate::management::ServerManagementJournal::open(&f.journal,&f.profile.digest,&f.profile.account).unwrap();
    f.registry.current_management(authority(f),"current_test".into(),journal).await.unwrap()
}
#[path="workspace_tests.rs"]
mod workspace_tests;
#[path="resource_name_tests.rs"]
mod resource_name_tests;
#[path="resource_edit_tests.rs"]
mod resource_edit_tests;
#[path="resource_create_tests.rs"]
mod resource_create_tests;
#[path="resource_ceiling_tests.rs"]
mod resource_ceiling_tests;
#[path="account_inventory_tests.rs"]
mod account_inventory_tests;
#[path="resource_alert_tests.rs"]
mod resource_alert_tests;
#[path="approval_binding_tests.rs"]
mod approval_binding_tests;
#[path="tool_approval_tests.rs"]
mod tool_approval_tests;
#[path="execution_policy_tests.rs"]
mod execution_policy_tests;
#[path="operator_graph_workflow_tests.rs"]
mod operator_graph_workflow_tests;
#[path="resource_template_tests.rs"]
mod resource_template_tests;
#[path="web_catalog_tests.rs"]
mod web_catalog_tests;
#[path="retirement_tests.rs"]
mod retirement_tests;
#[path="resource_publication_tests.rs"]
mod resource_publication_tests;
#[path="web_connection_tests.rs"]
mod web_connection_tests;
