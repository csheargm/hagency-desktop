use serde_json::Value;
#[derive(Default)]
struct ConnectionFixtureState {
    commands: std::collections::BTreeMap<String, (String, Value)>,
    resume_posts: usize,
    verify_posts: usize,
    lose_resume_reply: bool,
    provenance_path: Option<PathBuf>,
    contribution_posts: usize,
    /// Ledger declarations the fixture server has answered.
    ledger_declarations: usize,
}
fn connection_fixture_reply(
    path: &str,
    body: &Value,
    admission: &AdmissionState,
    publications: &PublicationFixture,
) -> Option<Value> {
    let mut s = publications.lock().unwrap();
    let mut value = admission.lock().unwrap();
    if path.contains("/management/commands/") {
        let id = path.split('/').rev().nth(1).unwrap();
        return Some(match s.connections.commands.get(id) {
            Some((operation, _)) if body["operation"] == *operation => {
                json!({"recorded":true,"admission":value["admission"],"observedAtMs":now_ms().unwrap()})
            }
            _ => json!({"recorded":false,"observedAtMs":now_ms().unwrap()}),
        });
    }
    let generation = s.device_generation.unwrap_or(1);
    let c = &value["admission"]["contribution"];
    let operation = if path.ends_with("/resume") {
        "contribution.resume"
    } else {
        "contribution.verify"
    };
    let id = body["commandId"].as_str().unwrap().to_owned();
    if let Some((old_operation, old)) = s.connections.commands.get(&id) {
        assert_eq!(old_operation, operation);
        assert_eq!(old, body);
        return Some(value.clone());
    }
    if body["expectedRevision"] != c["revision"] {
        return Some(json!({"code":"management_revision_changed"}));
    }
    assert_eq!(body["resourceId"], c["resourceId"]);
    assert_eq!(body["resourceRevision"], c["resourceRevision"]);
    assert_eq!(c["id"], "fixture_contribution");
    assert_eq!(c["requestId"], "original_admission_request");
    assert_eq!(c["state"], "admitted");
    if operation == "contribution.resume" {
        if generation <= c["executorGeneration"].as_u64().unwrap() {
            return Some(json!({"code":"contribution_executor_current"}));
        }
        s.connections.resume_posts += 1;
        value["admission"]["contribution"]["executorGeneration"] = json!(generation);
    } else {
        if c["executorGeneration"] != generation {
            return Some(json!({"code":"contribution_executor_required"}));
        }
        s.connections.verify_posts += 1;
        let at = now_ms().unwrap();
        value["admission"]["contribution"]["verification"] =
            json!({"status":"verified","verifiedAtMs":at,"freshUntilMs":at+30000});
    }
    let revision = value["admission"]["contribution"]["revision"]
        .as_u64()
        .unwrap()
        + 1;
    value["admission"]["contribution"]["revision"] = json!(revision);
    value["admission"]["request"]["revision"] = json!(revision);
    s.connections
        .commands
        .insert(id, (operation.into(), body.clone()));
    if operation == "contribution.resume" && std::mem::take(&mut s.connections.lose_resume_reply) {
        return None;
    }
    Some(value.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_connection_requires_explicit_reconnect_and_keeps_approval_and_custody() {
    run_current_owner_contract("connection").await;
}

async fn verify_current_connection_contract(
    manager: &NativeManagement,
    owner: &NativeOwner,
    registry: &OwnerResourceRegistry,
    access: &super::super::OwnerGrantAccess,
    resource: &hagency_core::project::Resource,
) {
    use super::super::ManagementCommand;
    let service = access.service();
    let denied = service
        .verified_enrollment_with(registry, "fixture_contribution".into(), |_| async {
            panic!("stale generation must be refused before provider observation");
            #[allow(unreachable_code)]
            Ok(provider_fixture("synthetic-account"))
        })
        .await
        .err()
        .unwrap();
    assert_eq!(denied.code, "management_enrollment_changed");
    let revision = hagency_store::resource_publication_revision(resource).unwrap();
    let resume = |id: &str, revision: String| ManagementCommand::Resume {
        command_id: id.into(),
        contribution_id: "fixture_contribution".into(),
        expected_revision: 2,
        resource_id: resource.id(),
        resource_revision: revision,
    };
    let wrong = manager
        .central_with_connection_fixture(
            resume("wrong_provider", revision.clone()),
            provider_fixture("another-account"),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(wrong.code, "management_executor_changed");
    let stale = manager
        .central_with_connection_fixture(
            resume("wrong_revision", "a".repeat(64)),
            provider_fixture("synthetic-account"),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(stale.code, "management_revision_conflict");
    let mut limited = current_owner_package(93);
    limited
        .permissions
        .retain(|p| *p != Permission::ResumeConnections);
    let limited = owner.management(limited).await.unwrap();
    assert_eq!(
        limited
            .central_with_connection_fixture(
                resume("no_permission", revision.clone()),
                provider_fixture("synthetic-account")
            )
            .await
            .err()
            .unwrap()
            .code,
        "management_permission_required"
    );
    limited.retire();
    // Provider observation can outlive the first finite package session. A
    // fresh same-context session must do the actual canonical proof check;
    // the retired captured session itself remains unusable.
    let (context, before) = manager.authorized().await.unwrap();
    let snapshot = before.resource(&context, resource.id()).await.unwrap();
    before.retire();
    assert!(!before.is_live());
    let renewed = manager
        .verify_canonical_connection(
            &context,
            &snapshot,
            true,
            provider_fixture("synthetic-account"),
        )
        .await;
    assert!(
        renewed.is_ok(),
        "fresh same-context provider check must not reuse retired session: {:?}",
        renewed.err()
    );
    assert!(!before.is_live(), "renewal never revives an old capability");
    // The native provider observation is synthetic; the current profile SQLite
    // journal, finite owner authority and HTTP write/recovery are real.
    let lost = manager
        .central_with_connection_fixture(
            resume("explicit_reconnect", revision.clone()),
            provider_fixture("synthetic-account"),
        )
        .await;
    assert!(lost.is_err(), "fixture loses only the post-commit response");
    let recovered = manager
        .central(ManagementCommand::Recover {
            command_id: "explicit_reconnect".into(),
            original_operation: hagency_store::management::IntentOperation::Resume,
        })
        .await
        .unwrap();
    assert_eq!(recovered["recorded"], true);
    let c = &recovered["admission"]["contribution"];
    assert_eq!(c["id"], "fixture_contribution");
    assert_eq!(c["resourceId"], resource.id());
    assert_eq!(c["requestId"], "original_admission_request");
    assert_eq!(c["state"], "admitted");
    assert_eq!(c["executorGeneration"], 2);
    assert_eq!(c["revision"], 3);
    let verified = manager
        .central_with_connection_fixture(
            ManagementCommand::Verify {
                command_id: "explicit_verify".into(),
                contribution_id: "fixture_contribution".into(),
                expected_revision: 3,
                resource_id: resource.id(),
                resource_revision: revision,
            },
            provider_fixture("synthetic-account"),
        )
        .await
        .unwrap();
    assert_eq!(
        verified["admission"]["contribution"]["verification"]["status"],
        "verified"
    );
    assert_eq!(
        verified["admission"]["request"]["id"],
        "original_admission_request"
    );
    assert!(
        service
            .verified_enrollment_with(registry, "fixture_contribution".into(), |_| async {
                Ok(provider_fixture("synthetic-account"))
            })
            .await
            .is_ok()
    );
    assert!(
        access.list_existing().await.unwrap().is_empty(),
        "reconnect is not capacity consent"
    );
    let page = manager
        .resources(hagency_store::management::ResourceQuery::default())
        .await
        .unwrap();
    assert_eq!(page.resources.len(), 2);
    for row in page.resources {
        assert_eq!(row.budget.seat.remaining.map(u64::from), Some(700));
    }
}
