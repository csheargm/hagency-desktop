use super::*;
use crate::management::{
    CeilingAmendmentBlocker, CeilingAmendmentRefusal, EditLocalResource, HostContext, Permission,
    PublishResource, ResourceDetailQuery, ResourceEditBlocker, Session,
};
use crate::{CeilingAmendmentResult, CeilingChange, ProfileChange};

fn context(f: &Fixture) -> HostContext {
    HostContext::new(
        f.profile.account.clone(),
        1,
        "c".repeat(64),
        1,
        "ceiling_device".into(),
        1,
    )
    .unwrap()
}
fn session(backing: CurrentManagementStore, context: &HostContext) -> Session {
    Session::new_current(
        backing,
        context.clone(),
        [
            Permission::ReadResources,
            Permission::ConfigureResources,
            Permission::PublishResources,
        ],
        Instant::now() + Duration::from_secs(90),
    )
    .unwrap()
}
fn until() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn copy(input: &EditLocalResource) -> EditLocalResource {
    serde_json::from_value(serde_json::to_value(input).unwrap()).unwrap()
}
async fn amendment(
    s: &Session,
    c: &HostContext,
    id: &str,
    command: &str,
    sequence: u64,
    ceiling: CeilingChange,
) -> EditLocalResource {
    EditLocalResource {
        command_id: command.into(),
        resource_id: id.into(),
        expected_revision: s.resource(c, id.into()).await.unwrap().revision,
        profile: ProfileChange::Preserve {},
        ceiling,
        execution_policy: None,
        expected_ceiling_sequence: Some(sequence),
    }
}
fn monthly(tokens: u64) -> CeilingChange {
    CeilingChange::Monthly {
        tokens: tokens.try_into().unwrap(),
    }
}
fn refusal(result: Result<crate::ResourceConfigurationResult, Error>) -> CeilingAmendmentRefusal {
    match result {
        Err(Error::CeilingAmendmentRefused(reason)) => reason,
        other => panic!("expected a refused amendment, got {other:?}"),
    }
}
fn enrollment(f: &Fixture, revision: &str) -> VerifiedManagementEnrollment {
    VerifiedManagementEnrollment::new(
        f.profile.clone(),
        "ceiling_enrollment".into(),
        "ceiling_contribution".into(),
        f.resource.id(),
        revision.into(),
        f.profile.account.clone(),
        "ceiling_executor".into(),
        1,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
    .with_admission_request("ceiling_admission".into())
    .unwrap()
}
/// Offer the fixture's resource as a pool: one live grant of `allocated`
/// tokens this month. Provenance then records the resource as offered.
async fn offer(f: &Fixture, s: &Session, c: &HostContext, allocated: u64) -> NativeGrantReceipt {
    let revision = s.resource(c, f.resource.id()).await.unwrap().revision;
    f.registry
        .prepare_capacity_grant(
            VerifiedManagementEnrollment::new(
                f.profile.clone(),
                "ceiling_enrollment".into(),
                "ceiling_contribution".into(),
                f.resource.id(),
                revision,
                f.profile.account.clone(),
                "ceiling_executor".into(),
                1,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: None,
                command_id: "ceiling_consent".into(),
                grant_id: "ceiling_grant".into(),
                coordinator_mxid: f.profile.account.clone(),
                delegation_expires_at_ms: graphs::now_ms().unwrap() + 600_000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:a.test".into()],
                allocated_tokens: allocated,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .with_admission_request("ceiling_admission".into())
        .unwrap()
        .approve()
        .await
        .unwrap()
}
/// Every row an amendment must leave untouched: the definition, its
/// provenance and access, grants and their receipts, and pool Agents.
async fn untouched(f: &Fixture) -> serde_json::Value {
    f.registry
        .0
        .store
        .call(8192, |db| {
            let mut rows = Vec::new();
            for table in [
                "resources",
                "seats",
                "owner_resource_accounts",
                "owner_resource_grants",
                "owner_resource_receipts",
                "owner_resource_provenance",
                "owner_capacity_grants",
                "owner_capacity_receipts",
                "owner_managed_agents",
            ] {
                let mut q = db
                    .db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
                let count = q.column_count();
                let data = q
                    .query_map([], |r| {
                        Ok((0..count)
                            .map(|c| format!("{:?}", r.get_ref(c).unwrap()))
                            .collect::<Vec<_>>())
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.push(json!({"table":table,"rows":data}));
            }
            Ok(json!(rows))
        })
        .await
        .unwrap()
}
async fn amendments(f: &Fixture) -> Vec<(String, u64, String, u64, u64, String)> {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok(db
                .db
                .prepare("SELECT resource_id,sequence,command_id,previous_tokens,tokens,base_revision FROM owner_resource_ceiling_amendments ORDER BY resource_id,sequence")?
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}
async fn receipts(f: &Fixture) -> u64 {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT count(*) FROM management_commands WHERE operation='resource.edit_local'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}
fn tokens(detail: &crate::management::ResourceDetail) -> Option<u64> {
    detail
        .ceiling
        .as_ref()
        .and_then(|c| c.tokens)
        .map(u64::from)
}

#[tokio::test]
async fn offered_pool_maximum_is_amended_without_its_definition_revision_or_provenance() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f.resource.id();
    // An imported resource without proven history can be neither edited nor amended.
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert!(!detail.ceiling_amendment.allowed);
    assert_eq!(
        detail.ceiling_amendment.reason,
        Some(CeilingAmendmentBlocker::ContributionHistoryNotProven)
    );
    let early = amendment(&s, &c, &id, "amend_unproven", 0, monthly(1500)).await;
    assert_eq!(
        refusal(s.amend_resource_ceiling(&c, early, until()).await),
        CeilingAmendmentRefusal::NotAvailable
    );
    let grant = offer(&f, &s, &c, 500).await;
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(
        detail.editing.reason,
        Some(ResourceEditBlocker::PreviouslyOffered)
    );
    assert!(detail.ceiling_amendment.allowed);
    assert_eq!(detail.ceiling_amendment.reason, None);
    assert_eq!(detail.ceiling_amendment.sequence, 0);
    assert_eq!(detail.ceiling_amendment.minimum_tokens, Some(500));
    assert_eq!(
        detail.ceiling_amendment.per_request_tokens,
        Some(grant.allocated_tokens)
    );
    assert_eq!(tokens(&detail), Some(1000));
    let revision = detail.resource.revision.clone();
    let before = untouched(&f).await;
    let request = amendment(&s, &c, &id, "amend_raise", 0, monthly(1500)).await;
    let result = s
        .amend_resource_ceiling(&c, copy(&request), until())
        .await
        .unwrap();
    assert_eq!(result.resource_id, id);
    assert_eq!(
        result.revision, revision,
        "the definition revision is unchanged"
    );
    assert_eq!(
        result.ceiling_amendment,
        Some(CeilingAmendmentResult {
            sequence: 1,
            previous_tokens: 1000,
            tokens: 1500
        })
    );
    assert_eq!(
        untouched(&f).await,
        before,
        "no definition, provenance or grant row changed"
    );
    assert_eq!(
        amendments(&f).await,
        vec![(
            id.clone(),
            1,
            "amend_raise".to_owned(),
            1000,
            1500,
            revision.clone()
        )]
    );
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(1500));
    assert_eq!(detail.resource.revision, revision);
    assert_eq!(
        detail.resource.budget.pool.ceiling.map(u64::from),
        Some(1500)
    );
    assert_eq!(detail.resource.usage.ceiling_tokens, Some(1500));
    assert_eq!(
        detail.editing.reason,
        Some(ResourceEditBlocker::PreviouslyOffered)
    );
    assert_eq!(detail.ceiling_amendment.sequence, 1);
    assert_eq!(
        detail.ceiling_amendment.per_request_tokens,
        Some(500),
        "the live per-request limit is not changed by a max amendment"
    );
    assert_eq!(
        f.registry
            .resource(&f.profile, id.clone())
            .await
            .unwrap()
            .revision,
        revision
    );
    // Read-only sessions see the state but cannot amend.
    let read_only = Session::new_current(
        backing.clone(),
        c.clone(),
        [Permission::ReadResources],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let observed = read_only
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert!(!observed.ceiling_amendment.allowed);
    assert_eq!(
        observed.ceiling_amendment.reason,
        Some(CeilingAmendmentBlocker::ReadOnly)
    );
    assert_eq!(tokens(&observed), Some(1500));
    let denied = amendment(&s, &c, &id, "amend_read_only", 1, monthly(1600)).await;
    assert!(matches!(
        read_only.amend_resource_ceiling(&c, denied, until()).await,
        Err(Error::LocalAuthority)
    ));
    read_only.retire();
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn ceiling_amendment_replays_its_original_receipt_and_recovers_after_restart() {
    let mut f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f.resource.id();
    offer(&f, &s, &c, 500).await;
    let original = amendment(&s, &c, &id, "amend_original", 0, monthly(1200)).await;
    assert!(
        s.recover_local_resource_edit(&c, &original)
            .await
            .unwrap()
            .is_none()
    );
    let result = s
        .amend_resource_ceiling(&c, copy(&original), until())
        .await
        .unwrap();
    let later = amendment(&s, &c, &id, "amend_later", 1, monthly(1800)).await;
    s.amend_resource_ceiling(&c, later, until()).await.unwrap();
    // Replay returns the original result and writes nothing, even after a
    // later amendment; it never re-applies 1200 over 1800.
    let replay = s
        .amend_resource_ceiling(&c, copy(&original), until())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        serde_json::to_value(&result).unwrap()
    );
    let recovered = s
        .recover_local_resource_edit(&c, &original)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.ceiling_amendment, result.ceiling_amendment);
    assert_eq!(amendments(&f).await.len(), 2);
    assert_eq!(receipts(&f).await, 2);
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(1800));
    // The same command id with different content is a conflict, not a write.
    let mut changed = copy(&original);
    changed.ceiling = monthly(1300);
    assert!(matches!(
        s.recover_local_resource_edit(&c, &changed).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        s.amend_resource_ceiling(&c, changed, until()).await,
        Err(Error::Conflict)
    ));
    // A new command reviewed against the old sequence is stale.
    let mut stale = copy(&original);
    stale.command_id = "amend_stale".into();
    assert!(matches!(
        s.amend_resource_ceiling(&c, stale, until()).await,
        Err(Error::ResourceEditRevisionChanged)
    ));
    assert_eq!(amendments(&f).await.len(), 2);
    s.retire();
    drop(s);
    backing.shutdown().await.unwrap();
    drop(backing);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let reopened = current_backing(&f).await;
    let fresh = session(reopened.clone(), &c);
    let recovered = fresh
        .recover_local_resource_edit(&c, &original)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(recovered).unwrap(),
        serde_json::to_value(&result).unwrap()
    );
    let detail = fresh
        .resource_detail(&c, ResourceDetailQuery { id })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(1800));
    assert_eq!(detail.ceiling_amendment.sequence, 2);
    assert_eq!(receipts(&f).await, 2);
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn ceiling_amendment_refuses_clear_unchanged_stale_and_lowering_below_commitments() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f.resource.id();
    offer(&f, &s, &c, 500).await;
    let before = untouched(&f).await;
    for (command, ceiling, expected) in [
        (
            "amend_clear",
            CeilingChange::Clear {},
            CeilingAmendmentRefusal::ClearRefused,
        ),
        (
            "amend_same",
            monthly(1000),
            CeilingAmendmentRefusal::Unchanged,
        ),
        // 500 tokens are published for this month: the maximum cannot go below.
        (
            "amend_below",
            monthly(499),
            CeilingAmendmentRefusal::BelowCommitments,
        ),
        (
            "amend_far_below",
            monthly(1),
            CeilingAmendmentRefusal::BelowCommitments,
        ),
    ] {
        let request = amendment(&s, &c, &id, command, 0, ceiling).await;
        assert_eq!(
            refusal(s.amend_resource_ceiling(&c, request, until()).await),
            expected,
            "{command}"
        );
    }
    let mut stale = amendment(&s, &c, &id, "amend_stale_revision", 0, monthly(1500)).await;
    stale.expected_revision = "f".repeat(64);
    assert!(matches!(
        s.amend_resource_ceiling(&c, stale, until()).await,
        Err(Error::ResourceEditRevisionChanged)
    ));
    let future = amendment(&s, &c, &id, "amend_future_sequence", 1, monthly(1500)).await;
    assert!(matches!(
        s.amend_resource_ceiling(&c, future, until()).await,
        Err(Error::ResourceEditRevisionChanged)
    ));
    // Malformed amendment requests are refused before any read.
    let mut zero = amendment(&s, &c, &id, "amend_zero", 0, monthly(0)).await;
    assert!(matches!(
        s.amend_resource_ceiling(&c, copy(&zero), until()).await,
        Err(Error::Invalid(_))
    ));
    zero.ceiling = CeilingChange::Preserve {};
    assert!(matches!(
        s.amend_resource_ceiling(&c, copy(&zero), until()).await,
        Err(Error::Invalid(_))
    ));
    zero.ceiling = monthly(1500);
    zero.profile = ProfileChange::Select {
        model: "gpt-5.6-sol".into(),
        reasoning: Some("high".into()),
    };
    assert!(matches!(
        s.amend_resource_ceiling(&c, copy(&zero), until()).await,
        Err(Error::Invalid(_))
    ));
    zero.profile = ProfileChange::Preserve {};
    zero.execution_policy = Some(hagency_core::execution::ExecutionPolicy { yolo: true });
    assert!(matches!(
        s.amend_resource_ceiling(&c, copy(&zero), until()).await,
        Err(Error::Invalid(_))
    ));
    // An amendment request is never applied as an in-place edit, and an
    // offered resource's in-place edit stays refused.
    zero.execution_policy = None;
    let verified = f
        .registry
        .verify_provider_resource(f.profile.clone(), id.clone(), provider())
        .await
        .unwrap();
    let binding = ResourceEditBinding::new(verified, None).unwrap();
    assert!(matches!(
        s.edit_local_resource(&c, copy(&zero), binding).await,
        Err(Error::Invalid(_))
    ));
    let mut edit = copy(&zero);
    edit.expected_ceiling_sequence = None;
    let verified = f
        .registry
        .verify_provider_resource(f.profile.clone(), id.clone(), provider())
        .await
        .unwrap();
    assert!(matches!(
        s.edit_local_resource(&c, edit, ResourceEditBinding::new(verified, None).unwrap())
            .await,
        Err(Error::ResourceEditNotAvailable)
    ));
    // An expired confirmation records nothing.
    let late = amendment(&s, &c, &id, "amend_late", 0, monthly(1500)).await;
    assert!(
        s.amend_resource_ceiling(&c, late, Instant::now())
            .await
            .is_err()
    );
    assert!(amendments(&f).await.is_empty());
    assert_eq!(receipts(&f).await, 0);
    assert_eq!(untouched(&f).await, before);
    // Lowering to exactly this month's commitments is allowed.
    let floor = amendment(&s, &c, &id, "amend_floor", 0, monthly(500)).await;
    let lowered = s.amend_resource_ceiling(&c, floor, until()).await.unwrap();
    assert_eq!(
        lowered.ceiling_amendment,
        Some(CeilingAmendmentResult {
            sequence: 1,
            previous_tokens: 1000,
            tokens: 500
        })
    );
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(500));
    assert_eq!(
        detail.resource.budget.remaining_tokens.map(u64::from),
        Some(0)
    );
    assert_eq!(untouched(&f).await, before);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn never_offered_resources_are_not_amended_and_amended_ones_stay_amendment_only() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f
        .registry
        .prepare_enrollment(
            f.profile.clone(),
            "ceiling_fresh".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            700,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert!(!detail.ceiling_amendment.allowed);
    assert_eq!(
        detail.ceiling_amendment.reason,
        Some(CeilingAmendmentBlocker::NotOffered)
    );
    let request = amendment(&s, &c, &id, "amend_fresh", 0, monthly(900)).await;
    assert_eq!(
        refusal(s.amend_resource_ceiling(&c, request, until()).await),
        CeilingAmendmentRefusal::NotAvailable
    );
    // Published locally: in-place edits stop, amendments start.
    let revision = s.resource(&c, id.clone()).await.unwrap().revision;
    s.publish_resource(
        &c,
        PublishResource {
            command_id: "ceiling_publish".into(),
            resource_id: id.clone(),
            expected_revision: revision,
            published: true,
        },
    )
    .await
    .unwrap();
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(detail.editing.reason, Some(ResourceEditBlocker::Published));
    assert!(detail.ceiling_amendment.allowed);
    // Nothing is committed on it yet, so any whole maximum is acceptable.
    assert_eq!(detail.ceiling_amendment.minimum_tokens, Some(1));
    assert_eq!(detail.ceiling_amendment.per_request_tokens, None);
    let request = amendment(&s, &c, &id, "amend_published", 0, monthly(900)).await;
    s.amend_resource_ceiling(&c, request, until())
        .await
        .unwrap();
    // Withdrawn again, the amended resource keeps its amendment history: it is
    // never edited in place, so no definition write can diverge from the chain.
    let revision = s.resource(&c, id.clone()).await.unwrap().revision;
    s.publish_resource(
        &c,
        PublishResource {
            command_id: "ceiling_withdraw".into(),
            resource_id: id.clone(),
            expected_revision: revision,
            published: false,
        },
    )
    .await
    .unwrap();
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(
        detail.editing.reason,
        Some(ResourceEditBlocker::RetainedCommitments)
    );
    assert!(detail.ceiling_amendment.allowed);
    assert_eq!(detail.ceiling_amendment.sequence, 1);
    assert_eq!(tokens(&detail), Some(900));
    let request = amendment(&s, &c, &id, "amend_withdrawn", 1, monthly(800)).await;
    s.amend_resource_ceiling(&c, request, until())
        .await
        .unwrap();
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(800));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_create_copies_the_effective_maximum_of_an_amended_source() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f.resource.id();
    offer(&f, &s, &c, 500).await;
    let request = amendment(&s, &c, &id, "amend_source", 0, monthly(1300)).await;
    s.amend_resource_ceiling(&c, request, until())
        .await
        .unwrap();
    let revision = s.resource(&c, id.clone()).await.unwrap().revision;
    let create =
        |command: &str, ceiling: CeilingChange| crate::management::CreateResourceConfiguration {
            command_id: command.into(),
            source_resource_id: id.clone(),
            expected_revision: revision.clone(),
            profile: ProfileChange::Preserve {},
            ceiling,
            execution_policy: None,
        };
    let copied = s
        .create_resource_configuration(
            &c,
            create("copy_amended", CeilingChange::Preserve {}),
            until(),
        )
        .await
        .unwrap();
    let copied_id = copied.resource_id.clone();
    let definition = f
        .registry
        .0
        .store
        .call(1024, move |db| read_resource(&db.db, &copied_id))
        .await
        .unwrap();
    assert_eq!(
        definition.ceiling.and_then(|c| c.tokens).map(u64::from),
        Some(1300)
    );
    let detail = s
        .resource_detail(
            &c,
            ResourceDetailQuery {
                id: copied.resource_id.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(1300));
    assert_eq!(
        detail.ceiling_amendment.sequence, 0,
        "a copy starts its own definition"
    );
    // An explicit maximum still wins over the source's.
    let explicit = s
        .create_resource_configuration(&c, create("copy_explicit", monthly(200)), until())
        .await
        .unwrap();
    let detail = s
        .resource_detail(
            &c,
            ResourceDetailQuery {
                id: explicit.resource_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(200));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

fn pool_command(
    grant: &NativeGrantReceipt,
    f: &Fixture,
    id: &str,
    tokens: u64,
) -> ManagedAgentReserveCommand {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../capacity/managed_agents/pool-agent-v1.json"
    ))
    .unwrap();
    let mut command: ManagedAgentReserveCommand =
        serde_json::from_value(fixture["command"].clone()).unwrap();
    command.command_id = format!("ceiling_reserve_{id}");
    command.request_id = format!("ceiling_request_{id}");
    command.proposed_agent_id = format!("ceiling_agent_{id}");
    command.allocation_id = command.request_id.clone();
    command.manager_mxid = "@manager:a.test".into();
    command.decision_actor_mxid = f.profile.account.clone();
    command.pool_grant = Some(grant.clone());
    command.grant_receipt_digest =
        canonical::digest(&serde_json::to_value(grant).unwrap()).unwrap();
    command.enrollment_id = grant.enrollment_id.clone();
    command.executor_device_id = grant.executor_device_id.clone();
    command.executor_generation = grant.executor_generation;
    command.resource_id = grant.resource_id.clone();
    command.resource_revision = grant.resource_revision.clone();
    command.authority_revision = grant.grant_revision;
    command.requested_tokens = tokens;
    command.period_key = grant.period_key.clone();
    command.issued_at_ms = graphs::now_ms().unwrap();
    command.expires_at_ms = grant.delegation_expires_at_ms;
    command
}
async fn reserve(
    f: &Fixture,
    grant: &NativeGrantReceipt,
    command: ManagedAgentReserveCommand,
) -> ManagedAgentReservationReceipt {
    let digest = canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap();
    f.registry
        .reserve_managed_agent(
            VerifiedManagedAgentDelivery::for_pool(
                enrollment(f, &grant.resource_revision),
                command,
                digest,
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn pool_agent_custody_and_the_live_grant_are_unaffected_by_amendments() {
    let f = fixture().await;
    let backing = resource_name_tests::active(&f).await;
    let c = context(&f);
    let s = session(backing.clone(), &c);
    let id = f.resource.id();
    let grant = offer(&f, &s, &c, 500).await;
    let first = reserve(&f, &grant, pool_command(&grant, &f, "first", 200)).await;
    assert_eq!(first.state, "reserved");
    let child = first.agent_reservation_id.clone().unwrap();
    let before = untouched(&f).await;
    // Raise, then lower to just above this month's 500 published tokens.
    let raise = amendment(&s, &c, &id, "amend_pool_raise", 0, monthly(1600)).await;
    s.amend_resource_ceiling(&c, raise, until()).await.unwrap();
    let lower = amendment(&s, &c, &id, "amend_pool_lower", 1, monthly(600)).await;
    s.amend_resource_ceiling(&c, lower, until()).await.unwrap();
    assert_eq!(
        untouched(&f).await,
        before,
        "Agent bindings, grants and receipts unchanged"
    );
    // The pool stays bounded by its published grant, not by the new maximum:
    // the per-request limit changes only through a separate grant amendment.
    let over = reserve(&f, &grant, pool_command(&grant, &f, "over", 301)).await;
    assert_eq!(over.reason.as_deref(), Some("insufficient_capacity"));
    let rest = reserve(&f, &grant, pool_command(&grant, &f, "rest", 300)).await;
    assert_eq!(
        rest.state, "reserved",
        "new Agents still bind the same pinned revision"
    );
    // The reserved Agent's custody still verifies its pinned definition revision.
    let assignment = || {
        ManagedAssignmentAuthority::fixture(
            enrollment(&f, &grant.resource_revision),
            child.clone(),
            "ceiling_assignment".into(),
            1,
            Instant::now() + Duration::from_secs(20),
        )
    };
    assert!(matches!(
        f.registry
            .claim_managed_provision(assignment())
            .await
            .unwrap(),
        ManagedProvisionClaim::Issued(_)
    ));
    assert!(matches!(
        f.registry
            .claim_managed_provision(assignment())
            .await
            .unwrap(),
        ManagedProvisionClaim::History(_)
    ));
    // While that Agent's provisioning is unsettled, its usage is unknown: the
    // maximum can be raised but never lowered.
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(detail.ceiling_amendment.minimum_tokens, None);
    let unsettled = amendment(&s, &c, &id, "amend_pool_unsettled", 2, monthly(550)).await;
    assert_eq!(
        refusal(s.amend_resource_ceiling(&c, unsettled, until()).await),
        CeilingAmendmentRefusal::UsageUnsettled
    );
    let raise = amendment(&s, &c, &id, "amend_pool_raise_again", 2, monthly(700)).await;
    s.amend_resource_ceiling(&c, raise, until()).await.unwrap();
    let detail = s
        .resource_detail(&c, ResourceDetailQuery { id })
        .await
        .unwrap();
    assert_eq!(tokens(&detail), Some(700));
    assert_eq!(detail.ceiling_amendment.per_request_tokens, Some(500));
    assert_eq!(detail.ceiling_amendment.sequence, 3);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
