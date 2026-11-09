use super::*;
use crate::management::{
    HostContext, Permission, Session, WebConnectionChange, WebConnectionCommand,
};
fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "b".repeat(64),
        1,
        "connection_device".into(),
        1,
    )
    .unwrap()
}
fn session(store: &CurrentManagementStore, context: &HostContext, write: bool) -> Session {
    let mut permissions = vec![Permission::ReadConnections];
    if write {
        permissions.push(Permission::ContributeResources)
    }
    Session::new_current(
        store.clone(),
        context.clone(),
        permissions,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn command(id: &str, rev: u64, change: WebConnectionChange) -> WebConnectionCommand {
    WebConnectionCommand {
        command_id: id.into(),
        id: "matrix.test".into(),
        expected_revision: rev,
        change,
    }
}
fn create(id: &str) -> WebConnectionCommand {
    command(
        id,
        0,
        WebConnectionChange::Create {
            label: "Project Matrix".into(),
            api_base_url: "https://matrix.test".into(),
        },
    )
}
fn credential() -> serde_json::Value {
    json!({"kind":"appservice","asToken":"fixture-as-secret","hsToken":"fixture-hs-secret","namespace":"@ac_.*","senderLocalpart":"hagency"})
}

#[tokio::test]
async fn web_connection_commands_preserve_private_credentials_original_receipts_and_cas() {
    let f = fixture().await;
    let store = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(&store, &c, true);
    let original = s.apply_web_connection(&c, create("create")).await.unwrap();
    assert_eq!(original.connection.revision, 1);
    let applicability =
        serde_json::to_value(&original.connection).unwrap()["applicability"].clone();
    assert_eq!(
        applicability,
        json!({
            "allowanceScope":"legacy_fleet_requests", "allowanceEnforced":false,
            "resourcePoolAllocationsAffected":false, "managementAuthority":"signed_in_matrix",
            "importedTransportActive":false
        })
    );
    let updated = s
        .apply_web_connection(
            &c,
            command(
                "credential",
                1,
                WebConnectionChange::ReplaceCredential {
                    credential: credential(),
                },
            ),
        )
        .await
        .unwrap();
    assert!(updated.connection.has_credential);
    let public = serde_json::to_string(&s.web_connections(&c, None).await.unwrap()).unwrap();
    assert!(
        !public.contains("fixture-as-secret")
            && !public.contains("fixture-hs-secret")
            && !public.contains("asToken")
    );
    let private = s
        .web_connection_private(&c, "matrix.test".into())
        .await
        .unwrap();
    assert_eq!(
        private.credential.unwrap().as_token.as_deref(),
        Some("fixture-as-secret")
    );
    let zero = s
        .apply_web_connection(
            &c,
            command(
                "zero",
                2,
                WebConnectionChange::SetAllowance {
                    allocated_tokens: Some(0),
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(zero.connection.allocated_tokens, Some(0));
    assert!(!zero.connection.applicability.allowance_enforced);
    assert!(
        !zero
            .connection
            .applicability
            .resource_pool_allocations_affected
    );
    let snapshot = s.web_connection(&c, "matrix.test".into()).await.unwrap();
    assert_eq!(snapshot.applicability, original.connection.applicability);
    let cleared = s
        .apply_web_connection(
            &c,
            command(
                "clear",
                3,
                WebConnectionChange::SetAllowance {
                    allocated_tokens: None,
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(cleared.connection.allocated_tokens, None);
    assert_eq!(
        s.apply_web_connection(&c, create("create")).await.unwrap(),
        original
    );
    assert_eq!(
        s.web_connection_result(&c, "create".into()).await.unwrap(),
        Some(original)
    );
    assert!(matches!(
        s.apply_web_connection(
            &c,
            command("stale", 2, WebConnectionChange::WithdrawCredential)
        )
        .await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        s.apply_web_connection(
            &c,
            command("create", 4, WebConnectionChange::WithdrawCredential)
        )
        .await,
        Err(Error::Conflict)
    ));
    let reissued = s
        .apply_web_connection(
            &c,
            command(
                "issue",
                4,
                WebConnectionChange::IssueRegistration {
                    request: crate::IssueSideRegistrationRequest {
                        side: "matrix.test".into(),
                        url: "https://callback.test/as".into(),
                        registration_id: None,
                        sender_localpart: None,
                        user_namespace: Some("^@ac_.*:matrix\\.test$".into()),
                        exclusive: Some(true),
                    },
                },
            ),
        )
        .await
        .unwrap();
    assert!(reissued.connection.awaiting_install && reissued.connection.registration_available);
    let private = s
        .web_connection_private(&c, "matrix.test".into())
        .await
        .unwrap();
    assert_eq!(
        private.credential.as_ref().unwrap().as_token.as_deref(),
        Some("fixture-as-secret")
    );
    let yaml = private.registration.unwrap();
    assert!(yaml.contains("matrix\\\\.test"));
    let pending_digest =
        crate::management::web_connection_credential_digest(private.pending.as_ref().unwrap())
            .unwrap();
    s.apply_web_connection(
        &c,
        command(
            "verify",
            5,
            WebConnectionChange::Verification {
                state: "accepted".into(),
                credential_digest: pending_digest,
                staged: true,
            },
        ),
    )
    .await
    .unwrap();
    let promoted = s
        .web_connection_private(&c, "matrix.test".into())
        .await
        .unwrap();
    assert!(promoted.pending.is_none());
    assert_ne!(
        promoted.credential.as_ref().unwrap().as_token.as_deref(),
        Some("fixture-as-secret")
    );
    s.apply_web_connection(
        &c,
        command("withdraw", 6, WebConnectionChange::WithdrawCredential),
    )
    .await
    .unwrap();
    let private = s
        .web_connection_private(&c, "matrix.test".into())
        .await
        .unwrap();
    assert!(private.credential.is_none() && private.registration.is_none());
    assert_eq!(
        s.web_connection_result(&c, "credential".into())
            .await
            .unwrap()
            .unwrap(),
        updated
    );

}

#[tokio::test]
async fn web_connection_authority_and_sqlite_failure_rollback_are_closed() {
    let f = fixture().await;
    let store = resource_name_tests::active(&f).await;
    let c = context(&f);
    let read = session(&store, &c, false);
    assert!(matches!(
        read.apply_web_connection(&c, create("denied")).await,
        Err(Error::LocalAuthority)
    ));
    let s = session(&store, &c, true);
    s.apply_web_connection(&c, create("initial")).await.unwrap();
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER reject_web_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='web.connection' BEGIN SELECT RAISE(ABORT,'test receipt failure'); END;")?;Ok(())}).await.unwrap();
    assert!(
        s.apply_web_connection(
            &c,
            command(
                "rollback",
                1,
                WebConnectionChange::ReplaceCredential {
                    credential: credential()
                }
            )
        )
        .await
        .is_err()
    );
    let row = s.web_connection(&c, "matrix.test".into()).await.unwrap();
    assert_eq!(row.revision, 1);
    assert!(!row.has_credential);
    assert!(
        s.web_connection_result(&c, "rollback".into())
            .await
            .unwrap()
            .is_none()
    );
    let other = HostContext::new(
        "@other:a.test".into(),
        1,
        "b".repeat(64),
        1,
        "connection_device".into(),
        1,
    )
    .unwrap();
    assert!(s.web_connections(&other, None).await.is_err());
    s.authority().retire();
    assert!(s.web_connections(&c, None).await.is_err());
}

#[tokio::test]
async fn web_connection_profile_filter_and_private_kind_do_not_leak_into_pages() {
    let f = fixture().await;
    let store = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(&store, &c, true);
    s.apply_web_connection(&c, create("one")).await.unwrap();
    f.registry.0.store.call(1,|db|{db.db.execute("INSERT INTO owner_web_catalog_settings(profile,kind,entity,value) SELECT ?1,kind,entity,value FROM owner_web_catalog_settings WHERE kind='connection_private_v1'",["f".repeat(64)])?;Ok(())}).await.unwrap();
    assert_eq!(s.web_connections(&c, None).await.unwrap().items.len(), 1);
    assert!(
        s.web_connections(&c, Some("matrix.test".into()))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut oversize = credential();
    oversize["asToken"] = json!("s".repeat(4097));
    assert!(
        s.apply_web_connection(
            &c,
            command(
                "oversize",
                1,
                WebConnectionChange::ReplaceCredential {
                    credential: oversize
                }
            )
        )
        .await
        .is_err()
    );
    assert_eq!(
        s.web_connection(&c, "matrix.test".into())
            .await
            .unwrap()
            .revision,
        1
    );
}
