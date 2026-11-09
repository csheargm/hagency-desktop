// Current-profile setup and real canonical account/resource enrollment for
// owner connection tests. No source database or migration descriptor is read.
fn current_owner_package(instance: u64) -> ManagementPackage {
    let mut p = package(instance, false);
    p.permissions.extend([
        Permission::PrepareSourceQuiescence,
        Permission::ContributeResources,
        Permission::ReadIntents,
        Permission::ReadConnections,
        Permission::ResumeConnections,
        Permission::VerifyConnections,
        Permission::ReadAgentBudget,
        Permission::PrepareAccounts,
        Permission::ReadAccounts,
    ]);
    p
}

async fn current_connection_resource(
    manager: &NativeManagement,
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    second: bool,
) -> hagency_core::project::Resource {
    use hagency_store::management::{AccountInventoryQuery, AccountModelQuery, PrepareAccount};
    use hagency_store::owner_resources::NamespaceEnrollment;
    let account = manager
        .prepare_account(PrepareAccount {
            command_id: "current_connection_account".into(),
        })
        .await
        .unwrap();
    let detail = manager
        .account_inventory(AccountInventoryQuery {
            id: Some(account.account_id.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let account = AccountModelQuery {
        account_id: account.account_id,
        expected_revision: detail.account.unwrap().revision.unwrap(),
    };
    let selected = registry
        .select_resource_account(profile.clone(), account.clone())
        .await
        .unwrap();
    let receipt = registry
        .prepare_namespace_enrollment(
            profile.clone(),
            NamespaceEnrollment {
                command_id: if second {
                    "current_connection_secondary"
                } else {
                    "current_connection_primary"
                }
                .into(),
                account,
                model: "gpt-5.6-sol".into(),
                reasoning: Some(if second { "high" } else { "medium" }.into()),
                monthly_tokens: 1000,
            },
            selected,
            provider_fixture("synthetic-account"),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    registry
        .with_repository_for_tests(move |db| db.resource_configuration(&receipt.resource_id))
        .await
        .unwrap()
}

async fn run_current_owner_contract(case: &str) {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("host");
    let (origin, server, admission, publications) = server().await;
    if case == "connection" {
        let mut remote = publications.lock().unwrap();
        remote.device_generation = Some(2);
        remote.connections.lose_resume_reply = true;
    }
    let source = Arc::new(Source(Arc::new(AtomicBool::new(true))));
    let owner = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source.clone())
        .await
        .unwrap();
    let registry =
        OwnerResourceRegistry::isolated_for_tests(&root.path().join("canonical")).unwrap();
    let device = owner.0.host.console.authorized_device().await.unwrap();
    let profile = crate::console::owned_agents::profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .unwrap();
    let manager = owner.management(current_owner_package(380)).await.unwrap();
    let journal = manager.current_setup_journal().await.unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    let authority = hagency_store::owner_resources::adoption::ProfileInitializationAuthority::new(
        registry.clone(),
        ProfileAuthority::new(profile.clone(), "@owner:test".into(), until).unwrap(),
        until,
        || true,
    )
    .unwrap();
    registry
        .initialize_current_profile(authority, "current_owner_fixture".into(), journal)
        .await
        .unwrap();
    let selection = super::super::CurrentProfileSelection::completed(
        &owner,
        registry.clone(),
        "current_owner_fixture".into(),
        profile.clone(),
        device.device_id().into(),
    );
    selection.persist(&owner).unwrap();
    manager.retire();
    drop(manager);
    owner.suspend().await.unwrap();
    drop(owner);
    let owner = selection
        .open_with_matrix_guarded(&state, &origin, "@owner:test", source, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(
        selection.device_setup_phase(&owner).await.unwrap(),
        "active"
    );
    assert!(
        !state
            .join("owned-agent-owners")
            .join(format!("management_{profile}"))
            .join("domain.sqlite3")
            .exists()
    );

    #[cfg(unix)]
    if matches!(case, "namespace" | "quota") {
        let probe_owner = owner.clone();
        let probe_registry = registry.clone();
        let quota_only = case == "quota";
        tokio::spawn(async move {
            verify_namespace_execution_contract(
                &probe_owner,
                &probe_registry,
                &profile,
                &state,
                quota_only,
            )
            .await;
        })
        .await
        .unwrap();
    } else {
        run_current_connection_contract(
            case,
            &owner,
            &registry,
            &selection,
            &profile,
            root.path(),
            &admission,
            &publications,
        )
        .await;
    }
    #[cfg(not(unix))]
    run_current_connection_contract(
        case,
        &owner,
        &registry,
        &selection,
        &profile,
        root.path(),
        &admission,
        &publications,
    )
    .await;

    let remote = publications.lock().unwrap();
    assert_eq!(
        remote.device_posts, 1,
        "current restart reuses the native device"
    );
    assert_eq!(remote.posts, 0, "these operations never allocate capacity");
    if case == "connection" {
        assert_eq!(remote.connections.resume_posts, 1);
        assert_eq!(remote.connections.verify_posts, 1);
    } else if case == "provenance" {
        assert_eq!(remote.connections.contribution_posts, 4);
    }
    drop(remote);
    drop(selection);
    owner.suspend().await.unwrap();
    drop(owner);
    registry.shutdown().await.unwrap();
    server.abort();
}

async fn run_current_connection_contract(
    case: &str,
    owner: &NativeOwner,
    registry: &OwnerResourceRegistry,
    selection: &super::super::CurrentProfileSelection,
    profile: &str,
    root: &std::path::Path,
    admission: &AdmissionState,
    publications: &PublicationFixture,
) {
    assert!(matches!(case, "connection" | "provenance"));
    let manager = owner.management(current_owner_package(381)).await.unwrap();
    let authority = ProfileAuthority::new(
        profile.into(),
        "@owner:test".into(),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
    let resource = current_connection_resource(&manager, registry, &authority, false).await;
    if case == "connection" {
        let second = current_connection_resource(&manager, registry, &authority, true).await;
        assert_ne!(second.id(), resource.id());
        assert_eq!(second.seat_id, resource.seat_id);
        // A separate explicit account declaration, never inferred from a pool.
        let seat_id = resource.seat_id.clone();
        registry
            .with_repository_for_tests(move |db| {
                db.put_seat(&serde_json::from_value(json!({
                    "id":seat_id,"declaration":{"quotaTokens":700,"period":"monthly"}
                }))?)
            })
            .await
            .unwrap();
    }
    let revision = hagency_store::resource_publication_revision(&resource).unwrap();
    let at = now_ms().unwrap();
    *admission.lock().unwrap() = json!({"admission":{"contribution":{"id":"fixture_contribution","ownerMxid":"@owner:test","executorDeviceId":"device","executorGeneration":1,"resourceId":resource.id(),"resourceRevision":revision,"label":"Current resource","runtime":"codex","capabilities":["coding"],"coordinatorMxid":"@owner:test","state":"admitted","revision":2,"requestId":"original_admission_request","createdAtMs":at,"updatedAtMs":at,"verification":null},"request":{"id":"original_admission_request","kind":"resource_admission","contributionId":"fixture_contribution","projectId":null,"allocationId":null,"projectName":null,"resourceLabel":null,"requesterMxid":"@owner:test","approverMxid":"@admin:test","state":"approved","revision":2,"createdAtMs":at,"expiresAtMs":at+60000,"decidedAtMs":at,"canDecide":false}},"observedAtMs":at});
    let access = selection
        .grant_access(owner, package(382, false))
        .await
        .unwrap();
    let probe_owner = owner.clone();
    let probe_manager = manager.clone();
    let probe_registry = registry.clone();
    let probe_access = access.clone();
    if case == "connection" {
        tokio::spawn(async move {
            verify_current_connection_contract(
                &probe_manager,
                &probe_owner,
                &probe_registry,
                &probe_access,
                &resource,
            )
            .await;
        })
        .await
        .unwrap();
    } else {
        let path = root.join("canonical/domain.sqlite3");
        publications.lock().unwrap().connections.provenance_path = Some(path.clone());
        tokio::spawn(async move {
            verify_current_provenance_contract(
                &probe_manager,
                &probe_owner,
                &probe_access,
                &path,
                &resource,
            )
            .await;
        })
        .await
        .unwrap();
    }
    drop(access);
    manager.retire();
}
