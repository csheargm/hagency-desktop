mod common;
use common::*;
use hagency_core::{authority::VerifiedRequest, canonical, project::EngagementState};
use hagency_store::{DomainRepository, Error, coordinator::*};
use palpo_hagency_contract as contract;
use serde_json::{Value, json};

fn authority() -> ServerEngagement {
    serde_json::from_value(json!({"id":registration().fleet_id,"server":"example.test","owner":"@provider:example.test","coordinator":"@coordinator:example.test","registrationGeneration":1,"delegationRevision":1,"delegationExpiresAtMs":1000000,"state":"verified","allowSelfApproval":false,"coordinatorApprovalV1":true})).unwrap()
}
fn context(id: &str) -> Value {
    json!({"version":1,"commandId":id,"serverEngagementId":registration().fleet_id,"registrationGeneration":1,"delegationRevision":1,"actor":"@coordinator:example.test","issuedAtMs":1000,"expiresAtMs":100000})
}
fn resource_grant(amount: u64, revision: u64) -> ResourceGrant {
    serde_json::from_value(json!({"id":"grant_one","serverEngagementId":registration().fleet_id,"resourceId":resource("pool","seat",1000).id(),"revision":revision,"allocatedTokens":amount,"eligibleManagers":["@owner:example.test"]})).unwrap()
}
fn setup() -> (tempfile::TempDir, DomainRepository) {
    let dir = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.register(&registration()).unwrap();
    db.put_resource(&resource("pool", "seat", 1000)).unwrap();
    db.configure_coordinator(&authority()).unwrap();
    db.put_coordinator_resource(&resource_grant(300, 1), 1000)
        .unwrap();
    let definition = json!({"name":"Project","roomId":"!project:example.test","ownerDmRoomId":"!private:example.test"});
    let command: ProjectApproval = serde_json::from_value(json!({"context":context("project_decision"),"request":{"id":"project_request","revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","owner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&definition).unwrap(),"resourceAllocations":["grant_one"]}})).unwrap();
    assert_eq!(
        db.approve_coordinator_project(&command, &definition, 1000)
            .unwrap()
            .state,
        contract::ProjectState::Approved
    );
    (dir, db)
}
fn prepared(
    db: &mut DomainRepository,
    id: &str,
    name: &str,
    tokens: u64,
) -> (AgentApproval, VerifiedRequest) {
    let proof = proof(&request(id, name, &resource("pool", "seat", 1000), tokens));
    db.verify_coordinator_project(&proof, 1000).unwrap();
    db.admit(&proof, 1000).unwrap();
    let command = serde_json::from_value(json!({"context":context(&format!("approve_{id}")),"request":{"id":id,"revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"grant_one","projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&value(proof.request())).unwrap(),"requestedTokens":tokens},"allocatedTokens":tokens})).unwrap();
    (command, proof)
}

#[test]
fn delivered_approval_is_visible_before_matrix_admission_and_terminal_refusal_survives_restart() {
    let (dir, mut db) = setup();
    let proof = proof(&request(
        "pending_agent",
        "VisiblePending",
        &resource("pool", "seat", 1000),
        200,
    ));
    db.verify_coordinator_project(&proof, 1000).unwrap();
    let command:AgentApproval=serde_json::from_value(json!({"context":context("pending_decision"),"request":{"id":"pending_agent","revision":1,
        "serverEngagementId":registration().fleet_id,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"grant_one",
        "projectOwner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&value(proof.request())).unwrap(),"requestedTokens":200},"allocatedTokens":200})).unwrap();
    let mut payload = value(proof.request());
    payload["coordinatorApproval"] = value(&command);
    let row = db
        .receive_coordinator_agent(&registration().fleet_id, &payload, 1000)
        .unwrap();
    assert_eq!(row["state"], "pending");
    assert_eq!(row["agentName"], "VisiblePending");
    assert!(row["agentAllocationId"].is_null());
    assert!(db.engagement_labels("", None, 16).unwrap().is_empty());
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.coordinator_deliveries(&registration().fleet_id, "", 50)
            .unwrap(),
        vec![row.clone()]
    );
    assert_eq!(
        db.receive_coordinator_agent(&registration().fleet_id, &payload, 1001)
            .unwrap(),
        row
    );
    let mut changed = payload.clone();
    changed["coordinatorApproval"]["allocatedTokens"] = json!(150);
    assert!(matches!(
        db.receive_coordinator_agent(&registration().fleet_id, &changed, 1001),
        Err(Error::Conflict)
    ));
    let mut authority = authority();
    authority.coordinator = "@replacement:example.test".to_owned().try_into().unwrap();
    authority.delegation_revision = 2.try_into().unwrap();
    db.configure_coordinator(&authority).unwrap();
    let refused = db
        .receive_coordinator_agent(&registration().fleet_id, &payload, 1002)
        .unwrap();
    assert_eq!(refused["state"], "refused");
    assert_eq!(refused["reason"], "authority_changed");
    assert_eq!(
        db.receive_coordinator_agent(&registration().fleet_id, &payload, 1003)
            .unwrap(),
        refused
    );
    db.admit(&proof, 1003).unwrap();
    assert!(matches!(
        db.approve_coordinated_agent(&command, &proof, 1003),
        Err(Error::State)
    ));
}

#[test]
fn capacity_refusal_has_a_durable_receipt_and_applied_delivery_replays_without_reserving_again() {
    let (_dir, mut db) = setup();
    let (command, proof) = prepared(&mut db, "agent_first", "First", 200);
    let mut payload = value(proof.request());
    payload["coordinatorApproval"] = value(&command);
    db.receive_coordinator_agent(&registration().fleet_id, &payload, 1000)
        .unwrap();
    let applied = db
        .approve_coordinated_agent(&command, &proof, 1000)
        .unwrap();
    let row = db
        .receive_coordinator_agent(&registration().fleet_id, &payload, 200000)
        .unwrap();
    assert_eq!(row["state"], "applied");
    assert_eq!(row["agentAllocationId"], applied.id);
    let (second, proof) = prepared(&mut db, "agent_second", "Second", 200);
    let mut payload = value(proof.request());
    payload["coordinatorApproval"] = value(&second);
    db.receive_coordinator_agent(&registration().fleet_id, &payload, 1000)
        .unwrap();
    let error = db
        .approve_coordinated_agent(&second, &proof, 1000)
        .unwrap_err();
    assert!(matches!(error, Error::InsufficientCapacity));
    let row = db
        .refuse_coordinator_agent(
            second.context.command_id.as_str(),
            coordinator_refusal_reason(&error).unwrap(),
            1000,
        )
        .unwrap();
    assert_eq!(row["reason"], "insufficient_capacity");
    db.put_coordinator_resource(&resource_grant(600, 2), 1000)
        .unwrap();
    assert_eq!(
        db.receive_coordinator_agent(&registration().fleet_id, &payload, 1001)
            .unwrap(),
        row
    );
    assert!(matches!(
        db.approve_coordinated_agent(&second, &proof, 1001),
        Err(Error::State)
    ));
}

#[test]
fn coordinator_approval_reserves_and_provisions_without_a_console_decision() {
    let (dir, mut db) = setup();
    let (command, proof) = prepared(&mut db, "agent_one", "Littlewhite", 200);
    assert!(matches!(
        db.approve("console_bypass", &proof, 1000),
        Err(Error::LocalAuthority)
    ));
    // A console rejection would be a second verdict on the coordinator's
    // request, exactly like a console approval.
    let pending = proof.request().engagement_id().unwrap();
    assert!(matches!(
        db.reject("console_reject_bypass", &pending),
        Err(Error::LocalAuthority)
    ));
    let approved = db
        .approve_coordinated_agent(&command, &proof, 1000)
        .unwrap();
    assert_eq!(approved.state, EngagementState::Reserved);
    assert_eq!(u64::from(approved.allocation()), 200);
    let effect = db.effect(&format!("provision_{}", approved.id)).unwrap();
    assert_eq!(serde_json::to_value(effect.state).unwrap(), "pending");
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.approve_coordinated_agent(&command, &proof, 1001)
            .unwrap()
            .id,
        approved.id
    );
    assert_eq!(db.effect(&effect.id).unwrap().fence, 0);
    let mut changed = command;
    changed.allocated_tokens = 100.try_into().unwrap();
    assert!(
        db.approve_coordinated_agent(&changed, &proof, 1001)
            .is_err()
    );
}

#[test]
fn project_purpose_is_part_of_the_frozen_coordinator_decision() {
    let (_dir, mut db) = setup();
    let definition = json!({"name":"Another project","reason":"Build a useful app","roomId":"!another:example.test","ownerDmRoomId":"!another_private:example.test"});
    let command:ProjectApproval=serde_json::from_value(json!({"context":context("purpose_decision"),"request":{"id":"purpose_request","revision":1,
        "serverEngagementId":registration().fleet_id,"projectId":"project_two","owner":"@owner:example.test","requester":"@owner:example.test",
        "definitionDigest":canonical::digest(&definition).unwrap(),"resourceAllocations":["grant_one"]}})).unwrap();
    let mut changed = definition.clone();
    changed["reason"] = json!("Different purpose");
    assert!(
        db.approve_coordinator_project(&command, &changed, 1000)
            .is_err()
    );
    assert_eq!(
        db.approve_coordinator_project(&command, &definition, 1000)
            .unwrap()
            .state,
        contract::ProjectState::Approved
    );
    assert!(
        db.approve_coordinator_project(&command, &changed, 1000)
            .is_err()
    );
}

#[test]
fn contribution_and_agent_reservations_cannot_exceed_parent_or_child() {
    let (_dir, mut db) = setup();
    let (one, p1) = prepared(&mut db, "one", "One", 200);
    db.approve_coordinated_agent(&one, &p1, 1000).unwrap();
    let (two, p2) = prepared(&mut db, "two", "Two", 101);
    assert!(matches!(
        db.approve_coordinated_agent(&two, &p2, 1000),
        Err(Error::InsufficientCapacity)
    ));
    // Top up the engagement within its owned resource, then retry the original
    // still-unexecuted decision. No new human agent approval is required.
    db.put_coordinator_resource(&resource_grant(400, 2), 1000)
        .unwrap();
    db.approve_coordinated_agent(&two, &p2, 1000).unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(1001, 3), 1000)
            .is_err()
    );
    assert!(
        db.put_coordinator_resource(&resource_grant(300, 3), 1000)
            .is_err()
    );
    // Parent resource reserves 400, not 400 + both child allocations.
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1000)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(600)
    );
    db.revoke("retire_one", &p1.request().engagement_id().unwrap())
        .unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(101, 3), 1000)
            .is_err(),
        "retiring cannot refund unknown consumption"
    );
}

#[test]
fn console_contribution_permission_is_owned_revocable_and_fenced_at_commit() {
    use hagency_store::{ResourceConfigurationAccess, ResourcePublicationRetirement};
    use std::time::{Duration, Instant};
    let (_dir, mut db) = setup();
    let until = Instant::now() + Duration::from_secs(30);
    let retirement = ResourcePublicationRetirement::default();
    let access = ResourceConfigurationAccess::new(until, retirement.clone());
    let command = access
        .prepare_contribution(resource_grant(400, 2), until)
        .unwrap();
    assert!(
        matches!(access.revoke(), Err(Error::Busy)),
        "an admitted command retains custody"
    );
    retirement.retire();
    assert!(matches!(
        db.contribute_resource(command, 1000),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["allocatedTokens"],
        300
    );
    access.revoke().unwrap();
    assert!(matches!(
        access.prepare_contribution(resource_grant(400, 2), until),
        Err(Error::LocalAuthority)
    ));

    let live = ResourceConfigurationAccess::new(until, Default::default());
    let command = live
        .prepare_contribution(resource_grant(400, 2), until)
        .unwrap();
    db.contribute_resource(command, 1000).unwrap();
    live.revoke().unwrap();
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"],
        400
    );
}

#[test]
fn fleet_status_pages_reach_agents_after_the_first_hundred() {
    let (_dir, mut db) = setup();
    for i in 0..101 {
        let proof = proof(&request(
            &format!("request_{i}"),
            &format!("Worker{i}"),
            &resource("pool", "seat", 1000),
            1,
        ));
        db.admit(&proof, 1000).unwrap();
    }
    let mut after = String::new();
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = db
            .fleet_engagements(&registration().fleet_id, &after, 20)
            .unwrap();
        let Some(last) = page.last() else {
            break;
        };
        after = last.id.clone();
        for row in page {
            assert!(ids.insert(row.id));
        }
    }
    assert_eq!(ids.len(), 101);
    assert!(
        db.fleet_engagements("other_engagement", "", 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn revoked_coordinator_and_wrong_project_binding_are_refused() {
    let (_dir, mut db) = setup();
    let (command, proof) = prepared(&mut db, "one", "One", 200);
    let mut authority = authority();
    authority.coordinator = "@replacement:example.test".to_owned().try_into().unwrap();
    assert!(db.configure_coordinator(&authority).is_err());
    authority.delegation_revision = 2.try_into().unwrap();
    db.configure_coordinator(&authority).unwrap();
    assert!(
        db.approve_coordinated_agent(&command, &proof, 1000)
            .is_err()
    );
    assert!(matches!(
        db.effect(&format!(
            "provision_{}",
            proof.request().engagement_id().unwrap()
        )),
        Err(Error::NotFound)
    ));
    let mut other = request("other", "Other", &resource("pool", "seat", 1000), 100);
    other.target_room_id = "!different:example.test".into();
    assert!(
        db.verify_coordinator_project(&common::proof(&other), 1000)
            .is_err()
    );
}

fn publication_identity() -> hagency_store::outbound::RegistrationIdentity {
    hagency_store::outbound::RegistrationIdentity {
        binding: "fixture".into(),
        registration_generation: 1,
        side_id: "example.test".into(),
        fleet_id: registration().fleet_id,
        registration_fingerprint: hagency_store::publication_fingerprint(&registration()).unwrap(),
    }
}

#[test]
fn project_readiness_requires_fresh_matrix_proof_and_old_publication_ack_keeps_new_state() {
    let (dir, mut db) = setup();
    let identity = publication_identity();
    let initial = db.coordinator_updates(&identity).unwrap();
    assert_eq!(initial.len(), 3);
    let observation = observation(&request(
        "unused",
        "Unused",
        &resource("pool", "seat", 1000),
        100,
    ));
    let mut readiness = ProjectReadiness {
        registration: registration(),
        project_id: "project_one".into(),
        observed_at_ms: 1000,
        project: observation.project,
        owner_room: observation.owner_room,
    };
    assert!(db.coordinator_project_ready(&readiness, 31001).is_err());
    readiness
        .owner_room
        .joined
        .insert("@intruder:example.test".into());
    assert!(db.coordinator_project_ready(&readiness, 1001).is_err());
    readiness.owner_room.joined.remove("@intruder:example.test");
    assert_eq!(
        db.coordinator_project_ready(&readiness, 1001)
            .unwrap()
            .state,
        contract::ProjectState::Ready
    );
    db.put_coordinator_resource(&resource_grant(400, 2), 1001)
        .unwrap();
    // Network acceptance of an older frozen body cannot erase the later ready
    // state or the top-up. Restart retains precisely those new projections.
    db.acknowledge_coordinator_updates(&identity, &initial)
        .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let current = db.coordinator_updates(&identity).unwrap();
    assert_eq!(current.len(), 2);
    assert!(
        current
            .iter()
            .any(|u| u["payload"]["project"]["state"] == "ready")
    );
    assert!(
        current
            .iter()
            .any(|u| u["payload"]["resource"]["allocatedTokens"] == 400)
    );
    db.acknowledge_coordinator_updates(&identity, &current)
        .unwrap();
    assert!(db.coordinator_updates(&identity).unwrap().is_empty());
}

fn top_up(agent: &str, id: &str, expected: u64, added: u64) -> TokenTopUpApproval {
    let definition = json!({"agentAllocationId":agent,"expectedAllocatedTokens":expected,"requestedAdditionalTokens":added});
    serde_json::from_value(json!({"context":context(id),"request":{
        "id":format!("request_{id}"),"revision":1,"serverEngagementId":registration().fleet_id,"projectId":"project_one","projectRevision":1,
        "resourceAllocationId":"grant_one","agentAllocationId":agent,"projectOwner":"@owner:example.test","requester":"@owner:example.test",
        "definitionDigest":canonical::digest(&definition).unwrap(),"expectedAllocatedTokens":expected,"requestedAdditionalTokens":added},
        "additionalTokens":added})).unwrap()
}

#[test]
fn coordinator_top_up_is_atomic_replayable_and_cannot_grow_the_engagement_pool() {
    let (dir, mut db) = setup();
    let (decision, proof) = prepared(&mut db, "agent_one", "Littlewhite", 200);
    let agent = db
        .approve_coordinated_agent(&decision, &proof, 1000)
        .unwrap();
    let command = top_up(&agent.id, "tokens_one", 200, 80);
    assert!(matches!(
        db.raise_allocation("console_topup", &agent.id, 80, 1000),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(&command, &proof, 1001)
                .unwrap()
                .allocation()
        ),
        280
    );
    // A competing request froze the previous amount. It cannot apply twice.
    assert!(
        db.approve_coordinator_top_up(
            &top_up(&agent.id, "tokens_competing", 200, 20),
            &proof,
            1001
        )
        .is_err()
    );
    assert!(matches!(
        db.approve_coordinator_top_up(&top_up(&agent.id, "tokens_over", 280, 21), &proof, 1001),
        Err(Error::InsufficientCapacity)
    ));
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(&command, &proof, 1002)
                .unwrap()
                .allocation()
        ),
        280
    );
    assert_eq!(u64::from(db.get(&agent.id).unwrap().allocation()), 280);
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1002)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(700)
    );
    db.put_coordinator_resource(&resource_grant(400, 2), 1002)
        .unwrap();
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(
                &top_up(&agent.id, "tokens_later", 280, 80),
                &proof,
                1002
            )
            .unwrap()
            .allocation()
        ),
        360
    );
    assert_eq!(
        db.resource_headroom(&resource("pool", "seat", 1000).id(), 1002)
            .unwrap()
            .0
            .pool
            .remaining
            .map(u64::from),
        Some(600)
    );
    assert!(
        db.coordinator_updates(&publication_identity())
            .unwrap()
            .iter()
            .any(|u| u["payload"]["commandId"] == "tokens_later")
    );
}

#[test]
fn profile_import_is_atomic_and_native_probe_is_required_before_contribution() {
    let dir = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let mut reg = registration();
    reg.reception_room_id.clear();
    let mut policy = authority();
    policy.state = contract::EngagementState::Configuring;
    db.import_coordinator_registration(&reg, Some(&policy))
        .unwrap();
    db.put_resource(&resource("pool", "seat", 1000)).unwrap();
    assert!(
        db.put_coordinator_resource(&resource_grant(100, 1), 1000)
            .is_err()
    );
    db.bind_reception(&reg.fleet_id, reg.generation, "!reception:example.test")
        .unwrap();
    db.put_coordinator_resource(&resource_grant(100, 1), 1000)
        .unwrap();
    // A rejected delegate revision must not rotate the registration underneath
    // the still-valid profile.
    reg.generation = 2;
    policy.registration_generation = 2.try_into().unwrap();
    policy.coordinator = "@replacement:example.test".to_owned().try_into().unwrap();
    assert!(
        db.import_coordinator_registration(&reg, Some(&policy))
            .is_err()
    );
    assert_eq!(
        db.provisioning_registration(&reg.fleet_id)
            .unwrap()
            .generation,
        1
    );
    db.put_coordinator_resource(&resource_grant(200, 2), 1000)
        .unwrap();
}

#[test]
fn owner_delegation_changes_fence_old_commands_and_publish_before_resources() {
    use std::time::{Duration, Instant};
    let (dir, mut db) = setup();
    let access = hagency_store::ResourceConfigurationAccess::new(
        Instant::now() + Duration::from_secs(60),
        Default::default(),
    );
    let change: DelegationChange = serde_json::from_value(json!({
        "serverEngagementId":registration().fleet_id,"expectedRevision":1,"coordinatorMxid":"@replacement:example.test",
        "delegationExpiresAtMs":1000000,"allowSelfApproval":false,"state":"active","exportMxids":["@replacement:example.test"]
    })).unwrap();
    let prepare = |change: DelegationChange| {
        access
            .prepare_delegation(change, Instant::now() + Duration::from_secs(10))
            .unwrap()
    };
    let result = db
        .change_coordinator(prepare(change.clone()), 1001)
        .unwrap();
    assert_eq!(u64::from(result.delegation_revision), 2);
    assert_eq!(result.coordinator.as_str(), "@replacement:example.test");
    assert_eq!(
        db.change_coordinator(prepare(change.clone()), 1002)
            .unwrap(),
        result
    );
    let mut conflict = change.clone();
    conflict.allow_self_approval = true;
    assert!(matches!(
        db.change_coordinator(prepare(conflict), 1002),
        Err(Error::Conflict)
    ));
    let identity = publication_identity();
    let updates = db.coordinator_updates(&identity).unwrap();
    assert_eq!(updates[0]["id"], "engagement_authority");
    assert_eq!(
        updates[0]["payload"]["exportMxids"],
        json!(["@replacement:example.test"])
    );
    let (old, proof) = prepared(&mut db, "stale", "Stale", 100);
    assert!(db.approve_coordinated_agent(&old, &proof, 1002).is_err());
    let mut suspended = change.clone();
    suspended.expected_revision = 2.try_into().unwrap();
    suspended.state = DelegationState::Suspended;
    let paused = db
        .change_coordinator(prepare(suspended.clone()), 1003)
        .unwrap();
    assert_eq!(paused.state, contract::EngagementState::Suspended);
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.coordinator_authority(&registration().fleet_id)
            .unwrap()
            .unwrap(),
        paused
    );
    assert_eq!(
        db.change_coordinator(prepare(change), 1004).unwrap(),
        result
    );
    assert_eq!(
        db.coordinator_authority(&registration().fleet_id)
            .unwrap()
            .unwrap(),
        paused,
        "old replay cannot restore authority"
    );
    suspended.expected_revision = 3.try_into().unwrap();
    suspended.state = DelegationState::Revoked;
    let revoked = db
        .change_coordinator(prepare(suspended.clone()), 1005)
        .unwrap();
    suspended.expected_revision = 4.try_into().unwrap();
    suspended.state = DelegationState::Active;
    assert!(matches!(
        db.change_coordinator(prepare(suspended), 1006),
        Err(Error::Generation)
    ));
    assert_eq!(revoked.state, contract::EngagementState::Revoked);
    access.revoke().unwrap();
    assert!(access.prepare_delegation(serde_json::from_value(json!({"serverEngagementId":registration().fleet_id,"expectedRevision":4,"coordinatorMxid":"@replacement:example.test","delegationExpiresAtMs":1000000,"allowSelfApproval":false,"state":"revoked","exportMxids":[]})).unwrap(),Instant::now()+Duration::from_secs(10)).is_err());
}

#[test]
fn project_and_top_up_refusals_are_terminal_and_applied_commands_replay_after_expiry() {
    let (dir, mut db) = setup();
    let fleet = registration().fleet_id;
    let (command, proof) = prepared(&mut db, "receipt_agent", "ReceiptAgent", 200);
    let agent = db
        .approve_coordinated_agent(&command, &proof, 1000)
        .unwrap();
    let increase = top_up(&agent.id, "increase_once", 200, 50);
    let payload = json!({"operation":"coordinator_token_top_up","command":increase});
    db.approve_coordinator_top_up(&increase, &proof, 1001)
        .unwrap();
    assert_eq!(
        db.coordinator_command_outcome(&fleet, &payload)
            .unwrap()
            .unwrap()["state"],
        "applied"
    );
    assert_eq!(
        u64::from(
            db.approve_coordinator_top_up(&increase, &proof, 100001)
                .unwrap()
                .allocation()
        ),
        250
    );
    let too_much = top_up(&agent.id, "increase_refused", 250, 100);
    let failed_payload = json!({"operation":"coordinator_token_top_up","command":too_much});
    assert!(matches!(
        db.approve_coordinator_top_up(&too_much, &proof, 1001),
        Err(Error::InsufficientCapacity)
    ));
    let receipt = db
        .refuse_coordinator_command(&fleet, &failed_payload, "insufficient_capacity", 1001)
        .unwrap();
    assert_eq!(receipt["agentId"], agent.id);
    assert_eq!(receipt["state"], "refused");
    db.put_coordinator_resource(&resource_grant(500, 2), 1002)
        .unwrap();
    assert!(matches!(
        db.approve_coordinator_top_up(&too_much, &proof, 1002),
        Err(Error::State)
    ));
    let definition = json!({"name":"Blocked project","roomId":"!blocked:example.test","ownerDmRoomId":"!blocked_private:example.test"});
    let project:ProjectApproval=serde_json::from_value(json!({"context":context("project_refused"),"request":{"id":"blocked_project","revision":1,"serverEngagementId":fleet,"projectId":"blocked","owner":"@owner:example.test","requester":"@owner:example.test","definitionDigest":canonical::digest(&definition).unwrap(),"resourceAllocations":["missing_grant"]}})).unwrap();
    let project_payload = json!({"operation":"coordinator_project_approval","command":project,"definition":definition});
    assert!(matches!(
        db.approve_coordinator_project(&project, &definition, 1002),
        Err(Error::NotFound)
    ));
    let project_receipt = db
        .refuse_coordinator_command(&fleet, &project_payload, "resource_unavailable", 1002)
        .unwrap();
    assert_eq!(project_receipt["projectId"], "blocked");
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.refuse_coordinator_command(&fleet, &failed_payload, "authority_changed", 1003)
            .unwrap(),
        receipt
    );
    assert_eq!(
        db.coordinator_command_outcome(&fleet, &project_payload)
            .unwrap()
            .unwrap(),
        project_receipt
    );
    assert!(matches!(
        db.approve_coordinator_project(&project, &definition, 1003),
        Err(Error::State)
    ));
    let mut changed = failed_payload;
    changed["command"]["additionalTokens"] = json!(99);
    assert!(matches!(
        db.coordinator_command_outcome(&fleet, &changed),
        Err(Error::Conflict)
    ));
    assert_eq!(u64::from(db.get(&agent.id).unwrap().allocation()), 250);
    let updates = db.coordinator_updates(&publication_identity()).unwrap();
    assert_eq!(
        updates
            .iter()
            .filter(|u| u["payload"]["state"] == "refused")
            .count(),
        2
    );
}

#[test]
fn uncertain_provision_is_visible_after_restart_without_releasing_its_reservation() {
    let (dir, mut db) = setup();
    let (approval, proof) = prepared(&mut db, "uncertain_agent", "Uncertain", 200);
    let agent = db
        .approve_coordinated_agent(&approval, &proof, 1000)
        .unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["provisionEffect"],
        "pending"
    );
    let effect = db
        .claim_effect_for(&format!("provision_{}", agent.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["provisionEffect"],
        "started"
    );
    db.observe_effect(
        &effect.id,
        effect.fence,
        &hagency_store::EffectOutcome::Unknown,
    )
    .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let lifecycle = db.coordinator_agent_lifecycle(&agent.id).unwrap();
    assert_eq!(lifecycle["provisionEffect"], "uncertain");
    assert_eq!(lifecycle["runtimeState"], "reserved");
    assert!(db.claim_effect_for(&effect.id).unwrap().is_none());
    let (next, next_proof) = prepared(&mut db, "next_agent", "Next", 101);
    assert!(matches!(
        db.approve_coordinated_agent(&next, &next_proof, 1001),
        Err(Error::InsufficientCapacity)
    ));
}

#[test]
fn scoped_agent_controls_recheck_authority_and_preserve_cleanup_and_capacity_on_replay() {
    let (dir, mut db) = setup();
    let (approval, proof) = prepared(&mut db, "managed_agent", "Managed", 200);
    let agent = db
        .approve_coordinated_agent(&approval, &proof, 1000)
        .unwrap();
    let effect = db
        .claim_effect_for(&format!("provision_{}", agent.id))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &hagency_store::EffectOutcome::Applied {
            receipt: "native-fixture".into(),
        },
    )
    .unwrap();
    let control = |id: &str, op: &str, actor: &str| {
        let mut c = context(id);
        c["actor"] = json!(actor);
        serde_json::from_value::<AgentControl>(json!({"context":c,"agentAllocationId":agent.id,
            "projectId":"project_one","projectRevision":1,"resourceAllocationId":"grant_one","operation":op})).unwrap()
    };
    let fleet = registration().fleet_id;
    assert!(matches!(
        db.control_coordinator_agent(
            &fleet,
            &control("unauthorized", "stop", "@stranger:example.test"),
            1000
        ),
        Err(Error::LocalAuthority)
    ));
    let pause = control("pause_agent", "stop", "@owner:example.test");
    let receipt = db.control_coordinator_agent(&fleet, &pause, 1000).unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["paused"],
        true
    );
    let resume = control("resume_agent", "start", "@coordinator:example.test");
    db.control_coordinator_agent(&fleet, &resume, 1001).unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["paused"],
        false
    );
    // Replaying the old pause returns its receipt, it cannot undo the resume.
    assert_eq!(
        db.control_coordinator_agent(&fleet, &pause, 100001)
            .unwrap(),
        receipt
    );
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["paused"],
        false
    );
    let retire = control("retire_agent", "retire", "@owner:example.test");
    db.control_coordinator_agent(&fleet, &retire, 1002).unwrap();
    let cleanup = db
        .claim_effect_for(&format!("retire_{}", agent.id))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &cleanup.id,
        cleanup.fence,
        &hagency_store::EffectOutcome::Unknown,
    )
    .unwrap();
    let retry = control("retry_agent", "retry_cleanup", "@provider:example.test");
    assert!(matches!(
        db.control_coordinator_agent(&fleet, &retry, 1003),
        Err(Error::State)
    ));
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["cleanup"],
        "uncertain"
    );
    db.observe_effect(
        &cleanup.id,
        cleanup.fence,
        &hagency_store::EffectOutcome::NotApplied {
            receipt: "refused".into(),
        },
    )
    .unwrap();
    db.control_coordinator_agent(&fleet, &retry, 1004).unwrap();
    let retried = db.claim_effect_for(&cleanup.id).unwrap().unwrap();
    assert!(retried.fence > cleanup.fence);
    db.observe_effect(
        &retried.id,
        retried.fence,
        &hagency_store::EffectOutcome::Applied {
            receipt: "cleanup-complete".into(),
        },
    )
    .unwrap();
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    db.control_coordinator_agent(&fleet, &retire, 100001)
        .unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["cleanup"],
        "complete"
    );
    assert_eq!(
        db.server_engagement_resources(&fleet, "", 50).unwrap()[0]["retainedTokens"],
        200
    );
    let mut conflict = pause;
    conflict.operation = AgentOperation::Retire;
    assert!(matches!(
        db.control_coordinator_agent(&fleet, &conflict, 100001),
        Err(Error::Conflict)
    ));
}

#[test]
fn final_account_settlement_refunds_only_unused_capacity_and_late_usage_remains_charged() {
    use hagency_core::tasks::*;
    use hagency_store::{EffectOutcome, ResourceConfigurationAccess};
    let (dir, mut db) = setup();
    let (approval, proof) = prepared(&mut db, "settled_agent", "Settle", 200);
    let agent = db
        .approve_coordinated_agent(&approval, &proof, 1000)
        .unwrap();
    let effect = db
        .claim_effect_for(&format!("provision_{}", agent.id))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &EffectOutcome::Applied {
            receipt: "fixture".into(),
        },
    )
    .unwrap();
    db.register_session(&SessionBinding {
        id: "settlement_session".into(),
        engagement_id: agent.id.clone(),
        room_id: "!project:example.test".into(),
        thread_root: Some("$settle".into()),
    })
    .unwrap();
    db.create_canonical_task(
        "settlement_task",
        "settlement_session",
        "Measure settlement",
        1001,
    )
    .unwrap();
    db.register_workspace("settlement_workspace").unwrap();
    db.enqueue_dispatch(&DispatchInput {
        id: "settlement_dispatch".into(),
        session_id: "settlement_session".into(),
        task_id: Some("settlement_task".into()),
        resources: vec![ResourceLease {
            id: "settlement_workspace".into(),
            exclusive: true,
        }],
        payload: json!({}),
    })
    .unwrap();
    let cap = db
        .claim_dispatch("settlement_runner", 1002, 60000, 120000, 128)
        .unwrap()
        .unwrap();
    let scope = db.owned_dispatch_scope(&cap, 1003).unwrap();
    let started = db
        .start_owned_dispatch(&cap, scope.fingerprint(), 1004)
        .unwrap();
    let source = db.bind_usage_source(&cap, &started, 1005).unwrap();
    let observation = |n| {
        hagency_metering::observation::UsageObservation::parse(hagency_metering::Framework::Codex,&json!({"payload":{"info":{"total_token_usage":{"input_tokens":n,"output_tokens":0,"cached_input_tokens":0,"reasoning_output_tokens":0,"total_tokens":n}}}}).to_string()).unwrap()
    };
    db.record_usage_observation(&source, "first_usage", &observation(50), 1010)
        .unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let access = ResourceConfigurationAccess::new(until, Default::default());
    let mut usage:FinalUsage=serde_json::from_value(json!({"commandId":"settle_final","agentAllocationId":agent.id,"resourceAllocationId":"grant_one",
        "expectedAllocatedTokens":200,"consumedTokens":50,"period":"monthly","periodKey":"1970-01","evidenceReference":"invoice-fixture-50"})).unwrap();
    assert!(matches!(
        db.settle_coordinator_agent(
            access.prepare_settlement(usage.clone(), until).unwrap(),
            1011
        ),
        Err(Error::State)
    ));
    db.complete_dispatch(&cap, &json!({"done":true}), 1012)
        .unwrap();
    db.revoke("retire_before_settle", &agent.id).unwrap();
    assert!(matches!(
        db.settle_coordinator_agent(
            access.prepare_settlement(usage.clone(), until).unwrap(),
            1013
        ),
        Err(Error::State)
    ));
    let cleanup = db
        .claim_effect_for(&format!("retire_{}", agent.id))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &cleanup.id,
        cleanup.fence,
        &EffectOutcome::Applied {
            receipt: "cleanup-proven".into(),
        },
    )
    .unwrap();
    usage.consumed_tokens = 49.try_into().unwrap();
    assert!(matches!(
        db.settle_coordinator_agent(
            access.prepare_settlement(usage.clone(), until).unwrap(),
            1014
        ),
        Err(Error::Conflict)
    ));
    usage.consumed_tokens = 50.try_into().unwrap();
    let receipt = db
        .settle_coordinator_agent(
            access.prepare_settlement(usage.clone(), until).unwrap(),
            1015,
        )
        .unwrap();
    assert_eq!(receipt["releasedTokens"], 150);
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"],
        250
    );
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.settle_coordinator_agent(
            access.prepare_settlement(usage.clone(), until).unwrap(),
            1016
        )
        .unwrap(),
        receipt
    );
    db.record_usage_observation(&source, "late_usage", &observation(75), 1017)
        .unwrap();
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"],
        225
    );
    assert_eq!(
        db.coordinator_settlement(&agent.id).unwrap()["state"],
        "late_usage_charged"
    );
    assert_eq!(
        db.coordinator_settlement(&agent.id).unwrap()["lateUsageTokens"],
        25
    );
    assert!(
        db.coordinator_settlement(&agent.id)
            .unwrap()
            .get("evidenceReference")
            .is_none()
    );
    assert_eq!(
        db.settle_coordinator_agent(access.prepare_settlement(usage, until).unwrap(), 1018)
            .unwrap(),
        receipt
    );
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"],
        225
    );
    access.revoke().unwrap();
}

#[test]
fn scoped_agent_rename_preserves_identity_budget_and_newer_name_across_restart() {
    let (dir, mut db) = setup();
    let (approval, proof) = prepared(&mut db, "renamed_agent", "Original", 200);
    let agent = db
        .approve_coordinated_agent(&approval, &proof, 1000)
        .unwrap();
    let effect = db
        .claim_effect_for(&format!("provision_{}", agent.id))
        .unwrap()
        .unwrap();
    db.observe_effect(
        &effect.id,
        effect.fence,
        &hagency_store::EffectOutcome::Applied {
            receipt: "fixture".into(),
        },
    )
    .unwrap();
    let fleet = registration().fleet_id;
    // Upgrade a copied schema-65 database without changing its existing grant.
    drop(db);
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    common::remove_owner_resource_schema(&sql);
    sql.execute_batch("DROP TABLE management_intents; DROP TABLE management_commands; DROP TABLE management_identity; DROP TABLE coordinator_migrations; DROP TABLE coordinator_project_setup; DROP TABLE coordinator_project_setup_attempts; DROP TABLE coordinator_agent_profiles;")
        .unwrap();
    common::restore_empty_historical_tables(
        &sql,
        &[include_str!("../src/migrations/017-usage-ledger.sql")],
        &["usage_sources"],
    );
    sql.pragma_update(None, "user_version", 65).unwrap();
    drop(sql);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let rename = |id: &str, name: &str| {
        serde_json::from_value::<AgentControl>(json!({"context":context(id),"agentAllocationId":agent.id,"projectId":"project_one","projectRevision":1,"resourceAllocationId":"grant_one","operation":"rename","displayName":name})).unwrap()
    };
    let first = rename("name_one", "First friendly name");
    let receipt = db.control_coordinator_agent(&fleet, &first, 1000).unwrap();
    assert_eq!(
        db.matrix_agent_profile(&agent.id).unwrap()["state"],
        "pending"
    );
    db.observe_matrix_agent_profile(&agent.id, "First friendly name", true, 1001)
        .unwrap();
    assert_eq!(
        db.matrix_agent_profile(&agent.id).unwrap()["observedName"],
        "First friendly name"
    );
    let second = rename("name_two", "Second friendly name");
    db.control_coordinator_agent(&fleet, &second, 1002).unwrap();
    db.observe_matrix_agent_profile(&agent.id, "First friendly name", true, 1003)
        .unwrap();
    assert_eq!(
        db.matrix_agent_profile(&agent.id).unwrap()["state"],
        "pending"
    );
    db.observe_matrix_agent_profile(&agent.id, "Second friendly name", false, 1004)
        .unwrap();
    assert_eq!(
        db.matrix_agent_profile(&agent.id).unwrap()["state"],
        "failed"
    );
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.control_coordinator_agent(&fleet, &first, 1005).unwrap(),
        receipt
    );
    assert_eq!(
        db.matrix_agent_profile(&agent.id).unwrap()["desiredName"],
        "Second friendly name"
    );
    db.observe_matrix_agent_profile(&agent.id, "Second friendly name", true, 1006)
        .unwrap();
    assert_eq!(
        db.coordinator_agent_lifecycle(&agent.id).unwrap()["matrixProfile"]["state"],
        "verified"
    );
    let current = db.get(&agent.id).unwrap();
    assert_eq!(current.agent_name.as_str(), "Original");
    assert_eq!(u64::from(current.allocation()), 200);
    assert_eq!(
        db.server_engagement_resources(&fleet, "", 50).unwrap()[0]["retainedTokens"],
        200
    );
    let mut forbidden = rename("name_forbidden", "No");
    forbidden.context.actor = "@stranger:example.test".to_owned().try_into().unwrap();
    assert!(matches!(
        db.control_coordinator_agent(&fleet, &forbidden, 1007),
        Err(Error::LocalAuthority)
    ));
    let mut invalid = rename("name_invalid", "Bad\nName");
    assert!(
        db.control_coordinator_agent(&fleet, &invalid, 1007)
            .is_err()
    );
    invalid.operation = AgentOperation::Stop;
    invalid.display_name = Some("Good".into());
    assert!(
        db.control_coordinator_agent(&fleet, &invalid, 1007)
            .is_err()
    );
    assert!(matches!(
        db.control_coordinator_agent(&fleet, &rename("expired", "Expired"), 100001),
        Err(Error::Generation)
    ));
}

#[test]
fn approved_project_setup_recovers_without_reapproval_and_fences_stale_attempts() {
    let (dir, mut db) = setup();
    let setup = |id: &str| {
        serde_json::from_value::<ProjectSetupCommand>(json!({"context":context(id),"projectId":"project_one","projectRevision":1,"approvalCommandId":"project_decision"})).unwrap()
    };
    let initial = setup("project_decision");
    let work = db.begin_project_setup(&initial, 1000).unwrap();
    assert_eq!(work.definition.room_id, "!project:example.test");
    assert!(work.completed.is_none());
    assert!(
        db.begin_project_setup(&initial, 1001)
            .unwrap()
            .completed
            .is_none()
    );
    let failed = db
        .finish_project_setup(&initial, Some("private_membership_pending"), 1002)
        .unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        db.begin_project_setup(&initial, 100001)
            .unwrap()
            .completed
            .unwrap(),
        failed
    );
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    let mut retry = setup("setup_retry");
    retry.context.actor = "@owner:example.test".to_owned().try_into().unwrap();
    let resumed = db.begin_project_setup(&retry, 1003).unwrap();
    assert_eq!(resumed.definition.room_id, work.definition.room_id);
    assert_eq!(
        resumed.definition.owner_dm_room_id,
        work.definition.owner_dm_room_id
    );
    assert!(db.validate_project_setup(&initial, 1004).is_err());
    assert_eq!(
        db.begin_project_setup(&initial, 1004)
            .unwrap()
            .completed
            .unwrap(),
        failed
    );
    let mut denied = setup("admin_bypass");
    denied.context.actor = "@admin:example.test".to_owned().try_into().unwrap();
    assert!(matches!(
        db.begin_project_setup(&denied, 1004),
        Err(Error::LocalAuthority)
    ));
    let mut wrong = setup("wrong_source");
    wrong.approval_command_id = "nonexistent_decision".to_owned().try_into().unwrap();
    assert!(matches!(
        db.begin_project_setup(&wrong, 1004),
        Err(Error::NotFound)
    ));
    let observation = observation(&request(
        "unused",
        "Unused",
        &resource("pool", "seat", 1000),
        100,
    ));
    db.coordinator_project_ready(
        &ProjectReadiness {
            registration: registration(),
            project_id: "project_one".into(),
            observed_at_ms: 1004,
            project: observation.project,
            owner_room: observation.owner_room,
        },
        1004,
    )
    .unwrap();
    let ready = db.finish_project_setup(&retry, None, 1005).unwrap();
    assert_eq!(ready["state"], "ready");
    assert!(ready["revision"].as_u64() > failed["revision"].as_u64());
    drop(db);
    let mut db = DomainRepository::open(&dir.path().join("state")).unwrap();
    assert_eq!(
        db.begin_project_setup(&retry, 100001)
            .unwrap()
            .completed
            .unwrap(),
        ready
    );
    assert_eq!(
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["retainedTokens"],
        0
    );
    let sql = rusqlite::Connection::open(dir.path().join("state/domain.sqlite3")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM coordinator_commands", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        1,
        "recovery cannot introduce another approval"
    );
}

#[test]
fn catalog_capacity_tracks_agent_holds_without_duplicating_capability_budgets() {
    let (_dir, mut db) = setup();
    let read = |db: &DomainRepository| {
        db.published_catalog(&publication_identity())
            .unwrap()
            .into_update()
    };
    let first = read(&db);
    assert_eq!(
        first["capabilities"]["resourceBudgets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        first["capabilities"]["resourceBudgets"][0]["remainingTokens"],
        300
    );
    let (command, proof) = prepared(&mut db, "budget_agent", "BudgetAgent", 100);
    db.approve_coordinated_agent(&command, &proof, 1000)
        .unwrap();
    let next = read(&db);
    let row = &next["capabilities"]["resourceBudgets"][0];
    assert_eq!(row["allocatedTokens"], 300);
    assert_eq!(row["retainedTokens"], 100);
    assert_eq!(row["remainingTokens"], 200);
    assert!(row.get("eligibleManagers").is_none());
    assert!(row.get("owner").is_none());
    assert_eq!(
        row["remainingTokens"],
        db.server_engagement_resources(&registration().fleet_id, "", 50)
            .unwrap()[0]["remainingTokens"]
    );
    db.put_coordinator_resource(&resource_grant(400, 2), 1001)
        .unwrap();
    let increased = read(&db);
    assert_eq!(
        increased["capabilities"]["resourceBudgets"][0]["revision"],
        2
    );
    assert_eq!(
        increased["capabilities"]["resourceBudgets"][0]["remainingTokens"],
        300
    );
}
