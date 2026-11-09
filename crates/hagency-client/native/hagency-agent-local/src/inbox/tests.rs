use super::*;
const OWNER: &str = "@alice:test";
fn event() -> Dispatch {
    Dispatch {
        id: "dispatch-a".into(),
        binding_id: "binding-a".into(),
        agent_id: "agent-a".into(),
        event_id: "$event-a:test".into(),
        room_id: "!room-a:test".into(),
        requester_mxid: "@bob:test".into(),
        thread_root: "$root:test".into(),
        body: "request".into(),
        state: "offered".into(),
        binding_generation: 1,
        dispatch_epoch: Some(1),
        dispatch_device_id: Some("device-a".into()),
        execution_id: None,
        outcome: None,
    }
}
fn setup(path: &std::path::Path) -> Ledger {
    let mut l = Ledger::open(path, OWNER).unwrap();
    l.register_binding(OWNER, &event().scope()).unwrap();
    l
}
fn received(l: &mut Ledger) {
    l.receive_dispatch(OWNER, &event(), Limits::default(), 10)
        .unwrap();
    l.acknowledge_dispatch(OWNER, &event().id).unwrap();
}
fn response(prepared: &Prepared, newly_started: bool) -> ServerStart {
    let mut e = prepared.dispatch.clone();
    e.state = "running".into();
    e.execution_id = Some(prepared.execution_id.clone());
    ServerStart {
        dispatch: e,
        newly_started,
    }
}
#[test]
fn persisted_receipt_survives_lost_ack_and_changed_body_is_a_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    assert!(
        !l.receive_dispatch(OWNER, &event(), Limits::default(), 10)
            .unwrap()
            .already_durable
    );
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Received
    );
    assert!(
        l.receive_dispatch(OWNER, &event(), Limits::default(), 11)
            .unwrap()
            .already_durable
    );
    let mut changed = event();
    changed.body = "changed command".into();
    assert!(matches!(
        l.receive_dispatch(OWNER, &changed, Limits::default(), 11),
        Err(Error::Conflict)
    ));
    let mut moved = event();
    moved.id = "different-dispatch".into();
    assert!(matches!(
        l.receive_dispatch(OWNER, &moved, Limits::default(), 11),
        Err(Error::Conflict)
    ));
    l.acknowledge_dispatch(OWNER, &event().id).unwrap();
    l.acknowledge_dispatch(OWNER, &event().id).unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Acknowledged
    );
}
#[test]
fn only_one_concurrent_start_confirmation_mints_a_run_permit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let prepared = l.prepare_execution(OWNER, &event().id).unwrap();
    assert_eq!(
        l.prepare_execution(OWNER, &event().id)
            .unwrap()
            .execution_id,
        prepared.execution_id
    );
    drop(l);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let p = prepared.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut l = Ledger::open(path, OWNER).unwrap();
                barrier.wait();
                l.confirm_execution_start(OWNER, &response(&p, true))
                    .unwrap()
                    .is_some()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|yes| *yes)
            .count(),
        1
    );
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(
        l.confirm_execution_start(OWNER, &response(&prepared, false))
            .unwrap()
            .is_none()
    );
}
#[test]
fn unknown_server_start_or_restart_never_replays_the_model() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let prepared = l.prepare_execution(OWNER, &event().id).unwrap();
    assert!(
        l.confirm_execution_start(OWNER, &response(&prepared, false))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Unknown
    );
    assert!(l.prepare_execution(OWNER, &event().id).is_err());
    assert!(
        l.confirm_execution_start(OWNER, &response(&prepared, true))
            .unwrap()
            .is_none()
    );
    drop(l);
    let mut l = Ledger::open(path, OWNER).unwrap();
    assert_eq!(l.recover_local_executions(OWNER).unwrap(), 0);
    assert!(l.prepare_execution(OWNER, &event().id).is_err());
}
#[test]
fn durable_reply_retries_without_regenerating_model_or_changing_content() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let prepared = l.prepare_execution(OWNER, &event().id).unwrap();
    let permit = l
        .confirm_execution_start(OWNER, &response(&prepared, true))
        .unwrap()
        .unwrap();
    let exec = permit.into_prepared().execution_id;
    l.persist_execution_reply(OWNER, &event().id, &exec, "model result")
        .unwrap();
    drop(l);
    let mut l = Ledger::open(path, OWNER).unwrap();
    assert_eq!(l.recover_local_executions(OWNER).unwrap(), 0);
    let record = l.inbox_record(OWNER, &event().id).unwrap();
    assert_eq!(record.state, State::ReplyReady);
    assert_eq!(record.reply.as_deref(), Some("model result"));
    l.persist_execution_reply(OWNER, &event().id, &exec, "model result")
        .unwrap();
    assert!(matches!(
        l.persist_execution_reply(OWNER, &event().id, &exec, "changed"),
        Err(Error::Conflict)
    ));
    assert!(l.prepare_execution(OWNER, &event().id).is_err());
    l.confirm_matrix_delivery(OWNER, &event().id, &exec)
        .unwrap();
    l.confirm_matrix_delivery(OWNER, &event().id, &exec)
        .unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Replied
    );
}
#[test]
fn wrong_owner_scope_execution_and_lease_fail_without_granting_run() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    assert!(matches!(
        l.receive_dispatch("@mallory:test", &event(), Limits::default(), 1),
        Err(Error::Unauthorized)
    ));
    let mut wrong = event();
    wrong.room_id = "!other:test".into();
    assert!(matches!(
        l.receive_dispatch(OWNER, &wrong, Limits::default(), 1),
        Err(Error::Unauthorized)
    ));
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    let mut r = response(&p, true);
    r.dispatch.dispatch_epoch = Some(2);
    assert!(matches!(
        l.confirm_execution_start(OWNER, &r),
        Err(Error::Conflict)
    ));
    r = response(&p, true);
    r.dispatch.execution_id = Some("foreign-execution".into());
    assert!(matches!(
        l.confirm_execution_start(OWNER, &r),
        Err(Error::Conflict)
    ));
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Prepared
    );
}
#[test]
fn startup_seals_execution_and_preserves_unreported_usage() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    let policy = crate::Policy {
        budget: crate::Budget {
            limit: crate::Limit::Tokens(100),
            period: crate::Period::Lifetime,
        },
        requests: crate::RequestPolicy::Allow,
        high_risk: crate::ToolPolicy::Deny,
    };
    l.set_policy(OWNER, &event().scope(), crate::Layer::Room, 0, &policy)
        .unwrap();
    l.reserve(&event().scope(), &p.execution_id, &event().id, 50, 10)
        .unwrap();
    drop(l);
    let mut l = Ledger::open(path, OWNER).unwrap();
    assert_eq!(l.recover_local_executions(OWNER).unwrap(), 1);
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Unknown
    );
    assert_eq!(
        l.account(
            &event().scope(),
            crate::Layer::Room,
            crate::Period::Lifetime,
            10
        )
        .unwrap(),
        (0, 50)
    );
    assert_eq!(l.outstanding_calls(OWNER).unwrap()[0].state, "unknown");
}
#[test]
fn capacity_rejects_before_receipt_and_safe_lease_redelivery_preserves_execution_identity() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    let bytes = json(&event()).unwrap().len() + PROOF_BYTES;
    assert!(
        l.receive_dispatch(
            OWNER,
            &event(),
            Limits {
                max_records: 1,
                max_bytes: bytes - 1
            },
            1
        )
        .is_err()
    );
    assert!(l.inbox_pending(OWNER, 10).unwrap().is_empty());
    l.receive_dispatch(
        OWNER,
        &event(),
        Limits {
            max_records: 1,
            max_bytes: bytes,
        },
        1,
    )
    .unwrap();
    let mut next = event();
    next.dispatch_epoch = Some(2);
    next.dispatch_device_id = Some("device-b".into());
    assert!(
        l.receive_dispatch(
            OWNER,
            &next,
            Limits {
                max_records: 1,
                max_bytes: bytes
            },
            1
        )
        .unwrap()
        .already_durable
    );
    l.acknowledge_dispatch(OWNER, &next.id).unwrap();
    let p = l.prepare_execution(OWNER, &next.id).unwrap();
    assert_eq!(p.dispatch.dispatch_epoch, Some(2));
    let mut later = next;
    later.dispatch_epoch = Some(3);
    l.receive_dispatch(OWNER, &later, Limits::default(), 1)
        .unwrap();
    assert_eq!(
        l.prepare_execution(OWNER, &later.id)
            .unwrap()
            .dispatch
            .dispatch_epoch,
        Some(2)
    );
}
#[test]
fn reply_bytes_and_duplicate_delivery_growth_cannot_bypass_storage_limit() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    let bytes = json(&event()).unwrap().len() + PROOF_BYTES;
    l.receive_dispatch(
        OWNER,
        &event(),
        Limits {
            max_records: 10,
            max_bytes: bytes,
        },
        1,
    )
    .unwrap();
    let mut growing = event();
    growing.dispatch_epoch = Some(2);
    growing.dispatch_device_id = Some("longer-device-identity".into());
    assert!(
        l.receive_dispatch(
            OWNER,
            &growing,
            Limits {
                max_records: 10,
                max_bytes: bytes
            },
            1
        )
        .is_err()
    );
    l.acknowledge_dispatch(OWNER, &event().id).unwrap();
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    assert_eq!(l.inbox_available_bytes(OWNER).unwrap(), 0);
    assert!(
        l.persist_execution_reply(OWNER, &event().id, &p.execution_id, "x")
            .is_err()
    );
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Running
    );
}

#[test]
fn matrix_sent_compacts_payload_frees_active_capacity_and_never_replays_model() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    let mut e = event();
    e.body = "request text ".repeat(200);
    let reply = "reply text ".repeat(200);
    let limits = Limits {
        max_records: 1,
        max_bytes: json(&e).unwrap().len() + PROOF_BYTES + reply.len(),
    };
    l.receive_dispatch(OWNER, &e, limits, 10).unwrap();
    l.acknowledge_dispatch(OWNER, &e.id).unwrap();
    let prepared = l.prepare_execution(OWNER, &e.id).unwrap();
    l.confirm_execution_start(OWNER, &response(&prepared, true))
        .unwrap()
        .unwrap();
    l.persist_execution_reply(OWNER, &e.id, &prepared.execution_id, &reply)
        .unwrap();
    let before = l.inbox_capacity(OWNER).unwrap();
    assert_eq!(before.active_records, 1);
    assert_eq!(l.inbox_available_bytes(OWNER).unwrap(), 0);
    let mut next = e.clone();
    next.id = "dispatch-next".into();
    next.event_id = "$next:test".into();
    assert!(l.receive_dispatch(OWNER, &next, limits, 11).is_err());
    let proof_before: (String, String, String) =
        l.db.query_row(
            "SELECT immutable_digest,execution_id,reply_digest FROM local_inbox WHERE id=?",
            [&e.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // Acceptance alone is not a cleanup trigger. Only Matrix-sent confirmation is.
    assert_eq!(
        l.inbox_record(OWNER, &e.id).unwrap().reply.as_deref(),
        Some(reply.as_str())
    );
    l.confirm_matrix_delivery(OWNER, &e.id, &prepared.execution_id)
        .unwrap();
    let after = l.inbox_capacity(OWNER).unwrap();
    assert_eq!(after.active_records, 0);
    assert_eq!(after.replied_receipts, 1);
    assert_eq!(
        before.charged_bytes - after.charged_bytes,
        (e.body.len() + reply.len()) as u64
    );
    assert!(after.charged_bytes > PROOF_BYTES as u64);
    let compact = l.inbox_record(OWNER, &e.id).unwrap();
    assert_eq!(compact.state, State::Replied);
    assert!(compact.dispatch.body.is_empty());
    assert!(compact.reply.is_none());
    assert_eq!(compact.dispatch.scope(), e.scope());
    let proof_after: (String, String, String) =
        l.db.query_row(
            "SELECT immutable_digest,execution_id,reply_digest FROM local_inbox WHERE id=?",
            [&e.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(proof_before, proof_after);
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(
        l.receive_dispatch(OWNER, &e, limits, 12)
            .unwrap()
            .already_durable
    );
    assert!(l.prepare_execution(OWNER, &e.id).is_err());
    assert!(
        l.confirm_execution_start(OWNER, &response(&prepared, true))
            .unwrap()
            .is_none()
    );
    l.persist_execution_reply(OWNER, &e.id, &prepared.execution_id, &reply)
        .unwrap();
    assert!(l.inbox_record(OWNER, &e.id).unwrap().reply.is_none());
    let mut changed = e.clone();
    changed.body.push_str(" changed");
    assert!(matches!(
        l.receive_dispatch(OWNER, &changed, limits, 12),
        Err(Error::Conflict)
    ));
    let mut alias = e.clone();
    alias.id = "alias-id".into();
    assert!(matches!(
        l.receive_dispatch(OWNER, &alias, limits, 12),
        Err(Error::Conflict)
    ));
    assert!(
        !l.receive_dispatch(OWNER, &next, limits, 12)
            .unwrap()
            .already_durable
    );
    assert_eq!(l.inbox_capacity(OWNER).unwrap().active_records, 1);
}

#[test]
fn uncertain_inbox_payload_budget_and_tool_evidence_survive_sent_compaction() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    let mut unknown = event();
    unknown.id = "unknown".into();
    unknown.event_id = "$unknown:test".into();
    unknown.body = "unresolved input".into();
    l.receive_dispatch(OWNER, &unknown, Limits::default(), 10)
        .unwrap();
    l.acknowledge_dispatch(OWNER, &unknown.id).unwrap();
    let uncertain = l.prepare_execution(OWNER, &unknown.id).unwrap();
    l.confirm_execution_start(OWNER, &response(&uncertain, true))
        .unwrap()
        .unwrap();
    let policy = crate::Policy {
        budget: crate::Budget {
            limit: crate::Limit::Tokens(100),
            period: crate::Period::Lifetime,
        },
        requests: crate::RequestPolicy::Allow,
        high_risk: crate::ToolPolicy::AskOwner,
    };
    for layer in [
        crate::Layer::Agent,
        crate::Layer::Room,
        crate::Layer::Requester,
    ] {
        l.set_policy(OWNER, &unknown.scope(), layer, 0, &policy)
            .unwrap();
    }
    l.reserve(&unknown.scope(), "charge-unknown", &unknown.id, 50, 10)
        .unwrap();
    let proposal = crate::ToolProposal {
        scope: unknown.scope(),
        dispatch: unknown.id.clone(),
        tool: "room.create".into(),
        arguments: serde_json::json!({"path":"evidence"}),
        canonical_directory: "/private/room".into(),
        risk: "high".into(),
        policy_revision: l
            .policy_snapshot(&unknown.scope())
            .unwrap()
            .map(|p| p.revision),
        expires: 100,
    };
    l.approve_tool(OWNER, &proposal, 11).unwrap();
    l.mark_unknown(&unknown.scope(), "charge-unknown").unwrap();
    l.finish_local_execution(OWNER, &unknown.id, &uncertain.execution_id, Finish::Unknown)
        .unwrap();
    let unknown_before = json(&l.inbox_record(OWNER, &unknown.id).unwrap().dispatch).unwrap();
    received(&mut l);
    let done = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&done, true))
        .unwrap()
        .unwrap();
    l.persist_execution_reply(OWNER, &event().id, &done.execution_id, "completed output")
        .unwrap();
    l.confirm_matrix_delivery(OWNER, &event().id, &done.execution_id)
        .unwrap();
    assert_eq!(
        json(&l.inbox_record(OWNER, &unknown.id).unwrap().dispatch).unwrap(),
        unknown_before
    );
    assert_eq!(
        l.inbox_record(OWNER, &unknown.id).unwrap().state,
        State::Unknown
    );
    assert!(
        l.confirm_matrix_delivery(OWNER, &unknown.id, &uncertain.execution_id)
            .is_err()
    );
    assert_eq!(l.outstanding_calls(OWNER).unwrap()[0].state, "unknown");
    assert_eq!(
        l.account(
            &unknown.scope(),
            crate::Layer::Room,
            crate::Period::Lifetime,
            10
        )
        .unwrap(),
        (0, 50)
    );
    let approvals: i64 =
        l.db.query_row("SELECT count(*) FROM approvals", [], |r| r.get(0))
            .unwrap();
    assert_eq!(approvals, 1);
    assert_eq!(l.inbox_capacity(OWNER).unwrap().active_records, 0);
}

#[test]
fn leased_agent_recovery_preserves_other_agent_running_and_work_selection() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut ledger = setup(&path);
    let mut other = event();
    other.id = "dispatch-b".into();
    other.binding_id = "binding-b".into();
    other.agent_id = "agent-b".into();
    other.event_id = "$event-b:test".into();
    ledger.register_binding(OWNER, &other.scope()).unwrap();
    let policy = crate::Policy {
        budget: crate::Budget {
            limit: crate::Limit::Tokens(100),
            period: crate::Period::Lifetime,
        },
        requests: crate::RequestPolicy::Allow,
        high_risk: crate::ToolPolicy::Deny,
    };
    for event in [event(), other.clone()] {
        ledger
            .set_policy(OWNER, &event.scope(), crate::Layer::Room, 0, &policy)
            .unwrap();
        ledger
            .receive_dispatch(OWNER, &event, Limits::default(), 10)
            .unwrap();
        ledger.acknowledge_dispatch(OWNER, &event.id).unwrap();
        let p = ledger.prepare_execution(OWNER, &event.id).unwrap();
        ledger
            .confirm_execution_start(OWNER, &response(&p, true))
            .unwrap()
            .unwrap();
        ledger
            .reserve(&event.scope(), &p.execution_id, &event.id, 10, 10)
            .unwrap();
    }
    assert_eq!(
        ledger.recover_agent_executions(OWNER, "agent-a").unwrap(),
        1
    );
    assert_eq!(
        ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
        State::Unknown
    );
    assert_eq!(
        ledger.inbox_record(OWNER, "dispatch-b").unwrap().state,
        State::Running
    );
    assert_eq!(
        ledger
            .inbox_for_agent(OWNER, "agent-b", State::Running, 1)
            .unwrap()[0]
            .dispatch
            .id,
        "dispatch-b"
    );
    let p = ledger.inbox_record(OWNER, "dispatch-b").unwrap();
    ledger
        .settle(
            &other.scope(),
            p.execution_id.as_deref().unwrap(),
            &crate::Usage {
                input: 4,
                output: 2,
                cached_input: 0,
                cache_write_input: None,
                reasoning_output: 0,
                accounting_version: "test".into(),
            },
        )
        .unwrap();
    assert_eq!(
        ledger
            .account(
                &other.scope(),
                crate::Layer::Room,
                crate::Period::Lifetime,
                10
            )
            .unwrap(),
        (6, 0)
    );
}
fn history(prepared: &Prepared) -> HistoryExecution {
    let d = &prepared.dispatch;
    HistoryExecution {
        dispatch_id: d.id.clone(),
        execution_id: prepared.execution_id.clone(),
        binding_id: d.binding_id.clone(),
        agent_id: d.agent_id.clone(),
        event_id: d.event_id.clone(),
        room_id: d.room_id.clone(),
        requester_mxid: d.requester_mxid.clone(),
        thread_root: d.thread_root.clone(),
        binding_generation: d.binding_generation,
        dispatch_epoch: d.dispatch_epoch.unwrap(),
        dispatch_device_id: d.dispatch_device_id.clone().unwrap(),
        immutable_digest: d.digest().unwrap(),
    }
}
fn charge_policy(l: &mut Ledger) {
    let p = crate::Policy {
        budget: crate::Budget {
            limit: crate::Limit::Tokens(100),
            period: crate::Period::Lifetime,
        },
        requests: crate::RequestPolicy::Allow,
        high_risk: crate::ToolPolicy::AskOwner,
    };
    for layer in [
        crate::Layer::Agent,
        crate::Layer::Room,
        crate::Layer::Requester,
    ] {
        l.set_policy(OWNER, &event().scope(), layer, 0, &p).unwrap();
    }
}
#[test]
fn continuity_missing_charge_is_zero_but_execution_identity_is_retained() {
    let t = tempfile::tempdir().unwrap();
    let mut l = setup(&t.path().join("local.db"));
    assert!(l.covers_execution_history(OWNER, "agent-a", &[]).unwrap());
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    assert!(l.covers_execution_history(OWNER, "agent-a", &[]).unwrap());
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}
#[test]
fn continuity_complete_unknown_hold_and_settled_cost_survive_restart_but_reset_balance_fails() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    charge_policy(&mut l);
    l.reserve(&event().scope(), &p.execution_id, &event().id, 50, 10)
        .unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.recover_agent_executions(OWNER, "agent-a").unwrap();
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    assert_eq!(
        l.reserve(&event().scope(), "fresh-call", "new-dispatch", 1, 11)
            .unwrap(),
        crate::Reservation::New
    );
    l.settle(
        &event().scope(),
        &p.execution_id,
        &crate::Usage {
            input: 10,
            output: 5,
            cached_input: 0,
            cache_write_input: None,
            reasoning_output: 0,
            accounting_version: "fixture".into(),
        },
    )
    .unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.db.execute("UPDATE accounts SET spent=0", []).unwrap();
    assert!(
        !l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}
#[test]
fn postpaid_history_survives_restart_and_compaction_without_forgiving_or_replaying_usage() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true)).unwrap();
    charge_policy(&mut l);
    l.begin_postpaid(&event().scope(), &p.execution_id, &event().id, 10).unwrap();
    assert!(l.covers_execution_history(OWNER, "agent-a", &[history(&p)]).unwrap());
    l.recover_agent_executions(OWNER, "agent-a").unwrap();
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(l.covers_execution_history(OWNER, "agent-a", &[history(&p)]).unwrap());
    assert_eq!(l.outstanding_calls(OWNER).unwrap()[0].state, "unknown");
    assert!(!l.inbox_record(OWNER, &event().id).unwrap().dispatch.body.is_empty());
    assert!(l.confirm_execution_start(OWNER, &response(&p, true)).unwrap().is_none());
    // A zero reservation is not enough: the original postpaid identity must match.
    l.db.execute("UPDATE calls SET digest='forged' WHERE id=?", [&p.execution_id]).unwrap();
    assert!(!l.covers_execution_history(OWNER, "agent-a", &[history(&p)]).unwrap());
    l.db.execute("UPDATE calls SET digest=? WHERE id=?", (call_digest(&event().scope(), &event().id, 0).unwrap(), &p.execution_id)).unwrap();
    let usage = crate::Usage {
        input: 140, output: 10, cached_input: 0, cache_write_input: None,
        reasoning_output: 0, accounting_version: "fixture-actual-v1".into(),
    };
    l.settle(&event().scope(), &p.execution_id, &usage).unwrap();
    l.settle(&event().scope(), &p.execution_id, &usage).unwrap();
    l.recover_agent_executions(OWNER, "agent-a").unwrap();
    drop(l);
    let l = Ledger::open(&path, OWNER).unwrap();
    assert!(l.inbox_record(OWNER, &event().id).unwrap().dispatch.body.is_empty());
    assert!(l.covers_execution_history(OWNER, "agent-a", &[history(&p)]).unwrap());
    assert_eq!(l.account(&event().scope(), crate::Layer::Room, crate::Period::Lifetime, 10).unwrap(), (150, 0));
    l.db.execute("UPDATE accounts SET spent=0", []).unwrap();
    assert!(!l.covers_execution_history(OWNER, "agent-a", &[history(&p)]).unwrap());
}
#[test]
fn continuity_rejection_seals_only_proven_no_call_and_preserves_existing_unknown_charge() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    l.finish_local_execution(OWNER, &event().id, &p.execution_id, Finish::Rejected)
        .unwrap();
    drop(l);
    let l = Ledger::open(&path, OWNER).unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    let t = tempfile::tempdir().unwrap();
    let mut l = setup(&t.path().join("local.db"));
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    charge_policy(&mut l);
    l.reserve(&event().scope(), &p.execution_id, &event().id, 50, 10)
        .unwrap();
    l.mark_unknown(&event().scope(), &p.execution_id).unwrap();
    l.finish_local_execution(OWNER, &event().id, &p.execution_id, Finish::Rejected)
        .unwrap();
    assert_eq!(l.outstanding_calls(OWNER).unwrap()[0].state, "unknown");
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}
#[test]
fn continuity_scope_digest_and_compacted_reply_require_exact_original_cost_proof() {
    let t = tempfile::tempdir().unwrap();
    let mut l = setup(&t.path().join("local.db"));
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    charge_policy(&mut l);
    l.reserve(&event().scope(), &p.execution_id, &event().id, 50, 10)
        .unwrap();
    l.settle(
        &event().scope(),
        &p.execution_id,
        &crate::Usage {
            input: 10,
            output: 5,
            cached_input: 0,
            cache_write_input: None,
            reasoning_output: 0,
            accounting_version: "fixture".into(),
        },
    )
    .unwrap();
    l.persist_execution_reply(OWNER, &event().id, &p.execution_id, "known output")
        .unwrap();
    l.confirm_matrix_delivery(OWNER, &event().id, &p.execution_id)
        .unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    let mut altered = history(&p);
    altered.immutable_digest = "0".repeat(64);
    assert!(
        !l.covers_execution_history(OWNER, "agent-a", &[altered])
            .unwrap()
    );
    let old: String =
        l.db.query_row(
            "SELECT snapshots FROM calls WHERE id=?",
            [&p.execution_id],
            |r| r.get(0),
        )
        .unwrap();
    let mut snapshots: Vec<crate::Snapshot> = serde_json::from_str(&old).unwrap();
    snapshots[1].scope = "wrong-room".into();
    l.db.execute(
        "UPDATE calls SET snapshots=? WHERE id=?",
        (json(&snapshots).unwrap(), &p.execution_id),
    )
    .unwrap();
    assert!(
        !l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.db.execute(
        "UPDATE calls SET snapshots=?,digest='invalid-digest' WHERE id=?",
        (&old, &p.execution_id),
    )
    .unwrap();
    assert!(
        !l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}
#[test]
fn binding_selection_filters_before_limit_for_known_reply_and_unknown_backlogs() {
    let t = tempfile::tempdir().unwrap();
    let mut l = setup(&t.path().join("local.db"));
    for state in [State::ReplyReady, State::Unknown] {
        for (idx, binding) in [(0, "binding-a"), (1, "binding-b")] {
            let mut e = event();
            e.id = format!(
                "dispatch-{}-{idx}",
                if state == State::ReplyReady {
                    "reply"
                } else {
                    "unknown"
                }
            );
            e.event_id = format!(
                "$event-{}-{idx}:test",
                if state == State::ReplyReady {
                    "reply"
                } else {
                    "unknown"
                }
            );
            e.binding_id = binding.into();
            if idx == 1 {
                e.room_id = "!room-b:test".into();
            }
            l.register_binding(OWNER, &e.scope()).unwrap();
            l.receive_dispatch(OWNER, &e, Limits::default(), 10 + idx)
                .unwrap();
            l.acknowledge_dispatch(OWNER, &e.id).unwrap();
            let p = l.prepare_execution(OWNER, &e.id).unwrap();
            l.confirm_execution_start(OWNER, &response(&p, true))
                .unwrap();
            if state == State::ReplyReady {
                l.persist_execution_reply(OWNER, &e.id, &p.execution_id, "known output")
                    .unwrap();
            } else {
                l.finish_local_execution(OWNER, &e.id, &p.execution_id, Finish::Unknown)
                    .unwrap();
            }
        }
        assert_eq!(
            l.inbox_for_agent(OWNER, "agent-a", state, 1).unwrap()[0]
                .dispatch
                .binding_id,
            "binding-a"
        );
        let selected = l
            .inbox_for_binding(OWNER, "agent-a", "binding-b", state, 1)
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].dispatch.binding_id, "binding-b");
    }
}

#[test]
fn terminal_rejections_free_payload_and_active_slots_without_losing_started_witnesses() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("local.db");
    let mut l = setup(&path);
    let limits = Limits {
        max_records: 1,
        max_bytes: 32_768,
    };
    let mut witnesses = Vec::new();
    for n in 0..20 {
        let mut e = event();
        e.id = format!("rejected-{n}");
        e.event_id = format!("$rejected-{n}:test");
        e.body = "deny this request".repeat(100);
        l.receive_dispatch(OWNER, &e, limits, 10).unwrap();
        l.acknowledge_dispatch(OWNER, &e.id).unwrap();
        let p = l.prepare_execution(OWNER, &e.id).unwrap();
        l.confirm_execution_start(OWNER, &response(&p, true))
            .unwrap();
        let bytes = l.inbox_capacity(OWNER).unwrap().charged_bytes;
        l.finish_local_execution(OWNER, &e.id, &p.execution_id, Finish::Rejected)
            .unwrap();
        assert_eq!(l.inbox_capacity(OWNER).unwrap().active_records, 0);
        assert_eq!(
            bytes - l.inbox_capacity(OWNER).unwrap().charged_bytes,
            e.body.len() as u64
        );
        assert!(
            l.inbox_record(OWNER, &e.id)
                .unwrap()
                .dispatch
                .body
                .is_empty()
        );
        assert!(
            l.receive_dispatch(OWNER, &e, limits, 11)
                .unwrap()
                .already_durable
        );
        let mut changed = e.clone();
        changed.body.push('!');
        assert!(matches!(
            l.receive_dispatch(OWNER, &changed, limits, 11),
            Err(Error::Conflict)
        ));
        witnesses.push(history(&p));
    }
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &witnesses)
            .unwrap()
    );
    assert!(matches!(
        l.prepare_execution(OWNER, "rejected-0"),
        Err(Error::Conflict)
    ));
    // Permanent proof metadata still counts against bytes: compaction is no purge.
    assert!(l.inbox_capacity(OWNER).unwrap().charged_bytes > 0);
}

#[test]
fn only_settled_failed_cost_can_compact_unknown_and_unsent_results_stay_retained() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("local.db");
    let mut l = setup(&path);
    charge_policy(&mut l);
    let mut witnesses = Vec::new();
    for (n, state) in [Finish::Failed, Finish::Rejected, Finish::Unknown]
        .into_iter()
        .enumerate()
    {
        let mut e = event();
        e.id = format!("terminal-{n}");
        e.event_id = format!("$terminal-{n}:test");
        e.body = "request".repeat(100);
        l.receive_dispatch(OWNER, &e, Limits::default(), 10)
            .unwrap();
        l.acknowledge_dispatch(OWNER, &e.id).unwrap();
        let p = l.prepare_execution(OWNER, &e.id).unwrap();
        l.confirm_execution_start(OWNER, &response(&p, true))
            .unwrap();
        l.reserve(&e.scope(), &p.execution_id, &e.id, 10, 10)
            .unwrap();
        if n == 0 {
            l.settle(
                &e.scope(),
                &p.execution_id,
                &crate::Usage {
                    input: 3,
                    output: 2,
                    cached_input: 0,
                    cache_write_input: None,
                    reasoning_output: 0,
                    accounting_version: "fixture-actual-v1".into(),
                },
            )
            .unwrap();
        }
        l.finish_local_execution(OWNER, &e.id, &p.execution_id, state)
            .unwrap();
        assert_eq!(
            l.inbox_record(OWNER, &e.id)
                .unwrap()
                .dispatch
                .body
                .is_empty(),
            n == 0
        );
        witnesses.push(history(&p));
    }
    assert_eq!(l.inbox_capacity(OWNER).unwrap().active_records, 1);
    assert_eq!(
        l.account(
            &event().scope(),
            crate::Layer::Room,
            crate::Period::Lifetime,
            10
        )
        .unwrap(),
        (5, 20)
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &witnesses)
            .unwrap()
    );
    let mut e = event();
    e.id = "unsent".into();
    e.event_id = "$unsent:test".into();
    l.receive_dispatch(OWNER, &e, Limits::default(), 10)
        .unwrap();
    l.acknowledge_dispatch(OWNER, &e.id).unwrap();
    let p = l.prepare_execution(OWNER, &e.id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    l.persist_execution_reply(OWNER, &e.id, &p.execution_id, "original undelivered reply")
        .unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &e.id).unwrap().reply.as_deref(),
        Some("original undelivered reply")
    );
    assert_eq!(l.inbox_record(OWNER, &e.id).unwrap().dispatch.body, e.body);
    assert_eq!(l.inbox_capacity(OWNER).unwrap().active_records, 2);
}

#[test]
fn direct_room_context_reuses_persisted_session_but_explicit_threads_and_rooms_stay_separate() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut ledger = setup(&path);
    let mut first = event();
    first.requester_mxid = OWNER.into();
    first.thread_root = first.room_id.clone();
    ledger
        .receive_dispatch(OWNER, &first, Limits::default(), 10)
        .unwrap();
    ledger
        .set_context_session(&first.scope(), "codex-existing-room-session")
        .unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let mut next = first.clone();
    next.id = "dispatch-next".into();
    next.event_id = "$event-next:test".into();
    next.body = "remember the previous message".into();
    ledger
        .receive_dispatch(OWNER, &next, Limits::default(), 11)
        .unwrap();
    assert_eq!(
        ledger.context_session(&next.scope()).unwrap().as_deref(),
        Some("codex-existing-room-session")
    );
    let mut threaded = next.clone();
    threaded.id = "dispatch-threaded".into();
    threaded.event_id = "$threaded-message:test".into();
    threaded.thread_root = "$explicit-thread:test".into();
    ledger
        .receive_dispatch(OWNER, &threaded, Limits::default(), 12)
        .unwrap();
    assert_eq!(ledger.context_session(&threaded.scope()).unwrap(), None);
    let mut another_room = next.clone();
    another_room.id = "dispatch-other-room".into();
    another_room.event_id = "$other-room-message:test".into();
    another_room.binding_id = "binding-other".into();
    another_room.room_id = "!other:test".into();
    another_room.thread_root = another_room.room_id.clone();
    ledger
        .register_binding(OWNER, &another_room.scope())
        .unwrap();
    ledger
        .receive_dispatch(OWNER, &another_room, Limits::default(), 13)
        .unwrap();
    assert_eq!(ledger.context_session(&another_room.scope()).unwrap(), None);
    // Existing completed/history roots remain their original event roots.
    assert_eq!(event().scope().thread, "$root:test");
    assert_ne!(
        event().scope().context_key(OWNER).unwrap(),
        first.scope().context_key(OWNER).unwrap()
    );
    let mut foreign = first;
    foreign.id = "foreign-context".into();
    foreign.thread_root = "!unrelated:test".into();
    assert!(
        ledger
            .receive_dispatch(OWNER, &foreign, Limits::default(), 14)
            .is_err()
    );
}

#[test]
fn no_provider_notice_is_atomic_recoverable_and_has_an_immutable_reply() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    assert!(
        matches!(
            l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "Budget unavailable"),
            Err(Error::Conflict)
        ),
        "prepared is not a confirmed running execution"
    );
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    assert!(matches!(
        l.persist_no_provider_reply(
            OWNER,
            &event().id,
            "foreign-execution",
            "Budget unavailable"
        ),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        l.persist_no_provider_reply(
            "@other:test",
            &event().id,
            &p.execution_id,
            "Budget unavailable"
        ),
        Err(Error::Unauthorized)
    ));
    l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "Budget unavailable")
        .unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::ReplyReady
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "Budget unavailable")
        .unwrap();
    assert!(matches!(
        l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "changed notice"),
        Err(Error::Conflict)
    ));
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(l.recover_agent_executions(OWNER, "agent-a").unwrap(), 0);
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().reply.as_deref(),
        Some("Budget unavailable")
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.confirm_matrix_delivery(OWNER, &event().id, &p.execution_id)
        .unwrap();
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Replied
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
    l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "Budget unavailable")
        .unwrap();
    assert!(matches!(
        l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "different"),
        Err(Error::Conflict)
    ));
}
#[test]
fn no_provider_notice_never_overwrites_pending_unknown_or_settled_model_charges() {
    for (state, postpaid) in ["pending", "unknown", "settled"]
        .into_iter().flat_map(|state| [(state, false), (state, true)]) {
        let temp = tempfile::tempdir().unwrap();
        let mut l = setup(&temp.path().join("local.db"));
        received(&mut l);
        let p = l.prepare_execution(OWNER, &event().id).unwrap();
        l.confirm_execution_start(OWNER, &response(&p, true))
            .unwrap();
        charge_policy(&mut l);
        if postpaid {
            l.begin_postpaid(&event().scope(), &p.execution_id, &event().id, 10).unwrap();
        } else {
            l.reserve(&event().scope(), &p.execution_id, &event().id, 50, 10).unwrap();
        }
        if state == "unknown" {
            l.mark_unknown(&event().scope(), &p.execution_id).unwrap();
        }
        if state == "settled" {
            l.settle(
                &event().scope(),
                &p.execution_id,
                &crate::Usage {
                    input: 3,
                    output: 2,
                    cached_input: 0,
                    cache_write_input: None,
                    reasoning_output: 0,
                    accounting_version: "fixture".into(),
                },
            )
            .unwrap();
        }
        let before:String=l.db.query_row("SELECT json_array(scope,reserved,snapshots,state,usage,digest) FROM calls WHERE id=?",[&p.execution_id],|r|r.get(0)).unwrap();
        assert!(matches!(
            l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "pretend no cost"),
            Err(Error::Conflict)
        ));
        let after:String=l.db.query_row("SELECT json_array(scope,reserved,snapshots,state,usage,digest) FROM calls WHERE id=?",[&p.execution_id],|r|r.get(0)).unwrap();
        assert_eq!(before, after);
        assert_eq!(
            l.inbox_record(OWNER, &event().id).unwrap().state,
            State::Running
        );
        assert_eq!(
            l.account(
                &event().scope(),
                crate::Layer::Agent,
                crate::Period::Lifetime,
                10
            )
            .unwrap(),
            if state == "settled" { (5, 0) } else { (0, if postpaid { 0 } else { 50 }) }
        );
    }
}
#[test]
fn no_provider_notice_capacity_failure_cannot_leave_a_false_zero_call_proof() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    l.db.execute("UPDATE inbox_limits SET max_bytes=1", [])
        .unwrap();
    assert!(
        l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "Budget unavailable")
            .is_err()
    );
    let count: u64 =
        l.db.query_row(
            "SELECT count(*) FROM calls WHERE id=?",
            [&p.execution_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Running
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}

#[test]
fn no_provider_notice_keeps_original_execution_unknown_but_new_requests_can_run() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let mut l = setup(&path);
    received(&mut l);
    let first = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&first, true))
        .unwrap();
    charge_policy(&mut l);
    l.reserve(&event().scope(), &first.execution_id, &event().id, 50, 10)
        .unwrap();
    l.mark_unknown(&event().scope(), &first.execution_id)
        .unwrap();
    l.finish_local_execution(OWNER, &event().id, &first.execution_id, Finish::Unknown)
        .unwrap();
    let mut notice = event();
    notice.id = "notice-dispatch".into();
    notice.event_id = "$notice:test".into();
    l.receive_dispatch(OWNER, &notice, Limits::default(), 11)
        .unwrap();
    l.acknowledge_dispatch(OWNER, &notice.id).unwrap();
    let p = l.prepare_execution(OWNER, &notice.id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    l.persist_no_provider_reply(
        OWNER,
        &notice.id,
        &p.execution_id,
        "Request denied; no model call.",
    )
    .unwrap();
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    l.recover_agent_executions(OWNER, "agent-a").unwrap();
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&first), history(&p)])
            .unwrap()
    );
    assert_eq!(
        l.inbox_record(OWNER, &event().id).unwrap().state,
        State::Unknown
    );
    assert_eq!(
        l.inbox_record(OWNER, &notice.id).unwrap().state,
        State::ReplyReady
    );
    assert_eq!(
        l.account(
            &event().scope(),
            crate::Layer::Room,
            crate::Period::Lifetime,
            12
        )
        .unwrap(),
        (0, 50)
    );
    assert_eq!(
        l.reserve(&event().scope(), "new-call", "new-dispatch", 1, 12)
            .unwrap(),
        crate::Reservation::New
    );
    l.db.execute("UPDATE accounts SET held=0", []).unwrap();
    assert!(
        !l.covers_execution_history(OWNER, "agent-a", &[history(&first), history(&p)])
            .unwrap()
    );
}

#[test]
fn no_provider_notice_cannot_relabel_a_settled_model_reply_even_when_body_matches() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = setup(&temp.path().join("local.db"));
    received(&mut l);
    let p = l.prepare_execution(OWNER, &event().id).unwrap();
    l.confirm_execution_start(OWNER, &response(&p, true))
        .unwrap();
    charge_policy(&mut l);
    l.reserve(&event().scope(), &p.execution_id, &event().id, 10, 10)
        .unwrap();
    l.settle(
        &event().scope(),
        &p.execution_id,
        &crate::Usage {
            input: 1,
            output: 1,
            cached_input: 0,
            cache_write_input: None,
            reasoning_output: 0,
            accounting_version: "fixture".into(),
        },
    )
    .unwrap();
    l.persist_execution_reply(OWNER, &event().id, &p.execution_id, "answer")
        .unwrap();
    assert!(matches!(
        l.persist_no_provider_reply(OWNER, &event().id, &p.execution_id, "answer"),
        Err(Error::Conflict)
    ));
    assert_eq!(
        l.account(
            &event().scope(),
            crate::Layer::Agent,
            crate::Period::Lifetime,
            10
        )
        .unwrap(),
        (2, 0)
    );
    assert!(
        l.covers_execution_history(OWNER, "agent-a", &[history(&p)])
            .unwrap()
    );
}
