use super::*;
use crate::console::native::reconciliation_sources::tests::Fixture;
use crate::console::native::{ManagementPackage};
use hagency_store::management::Permission;
use hagency_store::owner_resources::{OwnerResourceRegistry};
use std::time::{Duration,Instant};

fn package(instance:u64)->ManagementPackage{ManagementPackage{digest:"a".repeat(64),instance,account_epoch:1,permissions:vec![Permission::ReadResources,Permission::PublishResources,Permission::PrepareSourceQuiescence,Permission::ReadWebCatalog,Permission::ManageWebCatalog,Permission::ReadIntents,Permission::AcknowledgeIntents]}}
#[test]
fn web_catalog_cancellation_preserves_original_effects_and_prevents_later_application(){
    let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["console::native::web_catalog::tests::web_catalog_cancellation_process_child","--exact","--nocapture"]).env("HAGENCY_WEB_CANCEL_FIXTURE","1").output().unwrap();
    assert!(result.status.success(),"{} {}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
}
#[test]
fn web_publication_native_adopted_owner_uses_canonical_writer_and_original_command_recovery(){
    let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["console::native::web_catalog::tests::web_catalog_cancellation_process_child","--exact","--nocapture"]).env("HAGENCY_WEB_CANCEL_FIXTURE","publication").output().unwrap();
    assert!(result.status.success(),"{} {}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
}
#[tokio::test]
async fn web_catalog_cancellation_process_child(){
    let mode=std::env::var("HAGENCY_WEB_CANCEL_FIXTURE").unwrap_or_default();
    if mode!="1"&&mode!="publication"{return}
    let f=Fixture::new().await;
    let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
    let f=f.initialize_current(registry.clone()).await;
    let resource=f.current_resource(&registry).await;
    let manager=f.owner.management(package(2)).await.unwrap();
    if mode=="publication" {
        use hagency_store::management::PublishResource;
        assert!(f.owner.0.current_profile.get().is_some());
        let before=manager.resources(hagency_store::management::ResourceQuery{after:String::new(),limit:16,framework:None,capability:None,published:None}).await.unwrap().resources.into_iter().find(|r|r.id==resource.id()).unwrap();
        let original=PublishResource{command_id:"native_publish_original".into(),resource_id:resource.id(),expected_revision:before.revision.clone(),published:!before.published};
        let copy=||serde_json::from_value::<PublishResource>(serde_json::to_value(&original).unwrap()).unwrap();
        let result=manager.publish_resource(copy()).await.unwrap();
        assert_eq!(result.published,!before.published);
        assert_eq!(serde_json::to_value(manager.publish_resource(copy()).await.unwrap()).unwrap(),serde_json::to_value(&result).unwrap());
        let restored=manager.publish_resource(PublishResource{command_id:"native_publish_restore".into(),resource_id:resource.id(),expected_revision:result.revision.clone(),published:before.published}).await.unwrap();
        assert_eq!(restored.revision,before.revision);
        manager.acknowledge_command(original.command_id).await.unwrap();
        manager.acknowledge_command("native_publish_restore".into()).await.unwrap();
        let command=PublishResource{command_id:"native_publish_lost_reply".into(),resource_id:resource.id(),expected_revision:before.revision,published:!before.published};
        let(context,session)=manager.authorized().await.unwrap();
        let intent=Intent::new(IntentOperation::PublishResource,command.command_id.clone(),&command).unwrap();
        session.prepare_intent(&context,intent).await.unwrap();
        let direct=session.publish_resource(&context,serde_json::from_value(serde_json::to_value(&command).unwrap()).unwrap()).await.unwrap();
        assert_eq!(serde_json::to_value(manager.publish_resource(command).await.unwrap()).unwrap(),serde_json::to_value(direct).unwrap());
        manager.retire();f.owner.shutdown().await;registry.shutdown().await.unwrap();return
    }
    let query=||WebCatalogQuery{resource_id:resource.id(),after:None};
    let original=manager.prepare_web_catalog(WebCatalogPrepare{resource_id:resource.id(),expected_revision:manager.web_catalog(query()).await.unwrap().revision,change:WebCatalogChange::RateCap{rate_cap_per_day:Some(10)}}).await.unwrap();
    let cancelled=manager.cancel_saved_web_catalog(original.command_id.clone()).await.unwrap();
    assert_eq!(cancelled.command_id,original.command_id);assert_eq!(cancelled.error.as_deref(),Some("management_change_cancelled"));assert!(cancelled.result.is_none());
    assert_eq!(manager.apply_saved_web_catalog(original.command_id.clone()).await.unwrap_err().code,"management_change_cancelled");
    assert_eq!(manager.cancel_saved_web_catalog(original.command_id.clone()).await.unwrap().error,cancelled.error);
    assert_eq!(manager.web_catalog(query()).await.unwrap().rate_cap_per_day,None);
    manager.acknowledge_command(original.command_id).await.unwrap();

    let applied=manager.prepare_web_catalog(WebCatalogPrepare{resource_id:resource.id(),expected_revision:manager.web_catalog(query()).await.unwrap().revision,change:WebCatalogChange::RateCap{rate_cap_per_day:Some(20)}}).await.unwrap();
    let saved=manager.saved_commands().await.unwrap();let command=decode(saved.iter().find(|s|s.command_id==applied.command_id).unwrap()).unwrap();
    let(context,session)=manager.authorized().await.unwrap();
    let effect=session.apply_web_catalog(&context,command).await.unwrap(); // Canonical effect, lost journal reply.
    assert!(manager.review_web_catalog_command(applied.command_id.clone()).await.unwrap().result.is_none());
    let recovered=manager.cancel_saved_web_catalog(applied.command_id.clone()).await.unwrap();
    assert_eq!(recovered.result,Some(effect.clone()));assert!(recovered.error.is_none());
    assert_eq!(manager.apply_saved_web_catalog(applied.command_id.clone()).await.unwrap(),effect);
    assert_eq!(manager.cancel_saved_web_catalog(applied.command_id.clone()).await.unwrap().result,recovered.result);
    assert_eq!(manager.web_catalog(query()).await.unwrap().rate_cap_per_day,Some(20));
    manager.acknowledge_command(applied.command_id).await.unwrap();

    let lost=manager.prepare_web_catalog(WebCatalogPrepare{resource_id:resource.id(),expected_revision:manager.web_catalog(query()).await.unwrap().revision,change:WebCatalogChange::RateCap{rate_cap_per_day:Some(30)}}).await.unwrap();
    let saved=manager.saved_commands().await.unwrap();let command=decode(saved.iter().find(|s|s.command_id==lost.command_id).unwrap()).unwrap();
    assert!(session.cancel_web_catalog(&context,command).await.unwrap().is_none()); // Cancellation committed, journal reply lost.
    assert_eq!(manager.apply_saved_web_catalog(lost.command_id.clone()).await.unwrap_err().code,"management_change_cancelled");
    assert_eq!(manager.cancel_saved_web_catalog(lost.command_id).await.unwrap().error.as_deref(),Some("management_change_cancelled"));
    assert_eq!(manager.web_catalog(query()).await.unwrap().rate_cap_per_day,Some(20));
    manager.retire();f.owner.shutdown().await;registry.shutdown().await.unwrap();
}
