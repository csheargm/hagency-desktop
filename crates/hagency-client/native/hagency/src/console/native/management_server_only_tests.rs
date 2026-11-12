use super::*;
use crate::console::native::{ManagementCommand, reconciliation_sources::tests::Fixture};
use serde_json::json;

#[tokio::test]
async fn server_management_after_another_account_initializes_resources(){
    let f=Fixture::new().await;
    let registry=hagency_store::owner_resources::OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("other_canonical")).unwrap();
    let until=Instant::now()+Duration::from_secs(30);
    let authority=hagency_store::owner_resources::adoption::ProfileInitializationAuthority::new(registry.clone(),
        hagency_store::owner_resources::ProfileAuthority::new("f".repeat(64),"@other:test".into(),until).unwrap(),until,||true).unwrap();
    let journal=service::ServerManagementJournal::open(&f.temp.path().join("other_journal"),&"f".repeat(64),"@other:test").unwrap();
    registry.initialize_current_profile(authority,"other_current".into(),journal.clone()).await.unwrap();
    f.recipient_routes.lock().unwrap().extend([
        ("/hagency/api/v1/management/session".into(), json!({"protocol":"hagency-management-v1","account":"@owner:test","device":"fixture-device","deviceGeneration":1,"canAdmitResources":true})),
        ("/hagency/api/v1/management/inbox".into(), json!({"protocol":"hagency-management-v1","items":[],"nextAfter":null})),
        ("/hagency/api/v1/management/requests/req_1/decide".into(), json!({"admission":{"contribution":{"id":"con_1","state":"rejected"}}})),
    ]);
    let package = ManagementPackage {
        digest: "c".repeat(64),
        instance: 1,
        account_epoch: 1,
        permissions: vec![
            service::Permission::ReadConnections,
            service::Permission::ReadResources,
            service::Permission::ReadInbox,
            service::Permission::DecideInbox,
            service::Permission::ReadIntents,
        ],
    };
    let result = f.owner.management(package).await;
    {
        let manager = result.unwrap();
        assert!(matches!(manager.store, ManagementStore::Server(_)));
        let session = manager
            .central(ManagementCommand::Session {})
            .await
            .unwrap();
        assert_eq!(session["account"], "@owner:test");
        assert_eq!(session["canAdmitResources"], true);
        manager
            .central(ManagementCommand::Inbox {
                after: None,
                needs_my_action: false,
                include_projects: false,
                include_allocations: false,
            })
            .await
            .unwrap();
        assert_eq!(
            manager
                .resources(Default::default())
                .await
                .unwrap_err()
                .code,
            "device_setup_required"
        );
        manager
            .central(ManagementCommand::Decide {
                command_id: "admin_decision".into(),
                request_id: "req_1".into(),
                expected_revision: 1,
                approve: false,
            })
            .await
            .unwrap();
        let (context, access) = manager.authorized().await.unwrap();
        let saved = access.pending_intents(&context).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].command_id, "admin_decision");
        assert!(saved[0].reply.is_some());
        // Revocation still fences a server-only current account manager.
        f.live.store(false,Ordering::Release);
        assert!(manager.central(ManagementCommand::Session{}).await.is_err());
        f.live.store(true,Ordering::Release);
        manager.retire();
        assert_eq!(
            manager
                .central(ManagementCommand::Session {})
                .await
                .unwrap_err()
                .code,
            "management_access_required"
        );
    }
    journal.shutdown().await.unwrap();
    registry.shutdown().await.unwrap();
    f.stop().await;
}
