use super::*;
mod postpaid;
const OWNER: &str = "@alice:example.org";
#[test]
fn agent_policy_before_binding_shares_runtime_budget_and_preserves_charges() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert!(matches!(
        l.set_agent_policy("@other:example.org", "a1", 0, &config(50, Period::Lifetime)),
        Err(Error::Unauthorized)
    ));
    l.set_agent_policy(OWNER, "a1", 0, &config(50, Period::Lifetime))
        .unwrap();
    let model = ModelProfile {
        model: "test-model".into(),
        credential_ref: "keychain:codex:owner".into(),
        workspace_root: tmp.path().to_str().unwrap().into(),
        reasoning_effort: String::new(),
        web_search: true,
    };
    l.set_agent_model_profile(OWNER, "a1", &model).unwrap();
    assert!(
        l.set_agent_model_profile("@other:example.org", "a1", &model)
            .is_err()
    );
    assert!(matches!(
        l.set_agent_policy(OWNER, "a1", 0, &config(999, Period::Lifetime)),
        Err(Error::Conflict)
    ));
    assert_eq!(
        l.agent_account(OWNER, "a1", Period::Lifetime, 10).unwrap(),
        (0, 0)
    );
    l.register_binding(OWNER, &scope()).unwrap();
    assert_eq!(l.model_profile(&scope()).unwrap(), Some(model.clone()));
    l.set_policy(
        OWNER,
        &scope(),
        Layer::Room,
        0,
        &config(500, Period::Lifetime),
    )
    .unwrap();
    assert_eq!(
        l.policy_snapshot(&scope()).unwrap()[0].policy.budget.limit,
        Limit::Tokens(50)
    );
    l.reserve(&scope(), "call", "dispatch", 40, 10).unwrap();
    assert_eq!(
        l.agent_account(OWNER, "a1", Period::Lifetime, 10).unwrap(),
        (0, 40)
    );
    assert!(matches!(
        l.reserve(&scope(), "second", "dispatch2", 20, 10),
        Err(Error::Budget)
    ));
    l.settle(&scope(), "call", &usage(25)).unwrap();
    l.set_agent_policy(OWNER, "a1", 1, &config(100, Period::Lifetime))
        .unwrap();
    drop(l);
    let l = Ledger::open(path, OWNER).unwrap();
    assert_eq!(l.agent_model_profile(OWNER, "a1").unwrap(), Some(model));
    assert_eq!(l.agent_policy(OWNER, "a1").unwrap().revision, 2);
    assert_eq!(
        l.account(&scope(), Layer::Agent, Period::Lifetime, 10)
            .unwrap(),
        (25, 0)
    );
}
fn scope() -> Scope {
    Scope {
        agent: "a1".into(),
        binding: "b1".into(),
        room: "!r:example.org".into(),
        requester: "@bob:example.org".into(),
        thread: "main".into(),
    }
}
fn config(n: u64, period: Period) -> Policy {
    Policy {
        budget: Budget {
            limit: Limit::Tokens(n),
            period,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::AskOwner,
    }
}
fn setup(path: &Path) -> Ledger {
    let mut l = Ledger::open(path, OWNER).unwrap();
    l.register_binding(OWNER, &scope()).unwrap();
    l.set_policy(
        OWNER,
        &scope(),
        Layer::Room,
        0,
        &config(100, Period::UtcDay),
    )
    .unwrap();
    l
}
fn usage(n: u64) -> Usage {
    Usage {
        input: n,
        output: 0,
        cached_input: 0,
        cache_write_input: None,
        reasoning_output: 0,
        accounting_version: "codex/v1-input-output".into(),
    }
}
#[test]
fn concurrent_call_reservations_do_not_overspend() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    setup(&path);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut l = Ledger::open(path, OWNER).unwrap();
                barrier.wait();
                l.reserve(&scope(), &format!("c{i}"), "d", 30, 10).is_ok()
            })
        })
        .collect();
    let successes = threads
        .into_iter()
        .filter_map(|t| t.join().ok())
        .filter(|ok| *ok)
        .count();
    assert_eq!(successes, 3);
    assert_eq!(
        Ledger::open(&path, OWNER)
            .unwrap()
            .account(&scope(), Layer::Room, Period::UtcDay, 10)
            .unwrap(),
        (0, 90)
    );
}
#[test]
fn requester_limits_and_cross_room_context_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    l.set_policy(
        OWNER,
        &s,
        Layer::Requester,
        0,
        &config(10, Period::Lifetime),
    )
    .unwrap();
    assert!(matches!(
        l.reserve(&s, "c", "d", 11, 10),
        Err(Error::Budget)
    ));
    let mut other = s.clone();
    other.requester = "@charlie:example.org".into();
    assert_eq!(
        l.reserve(&other, "c2", "d", 40, 10).unwrap(),
        Reservation::New
    );
    l.set_context_session(&s, "session-a").unwrap();
    assert_eq!(l.context_session(&other).unwrap(), None);
    other.binding = "b2".into();
    other.room = "!r2:example.org".into();
    l.register_binding(OWNER, &other).unwrap();
    assert_eq!(l.context_session(&other).unwrap(), None);
    other.thread = "thread2".into();
    assert_eq!(l.context_session(&other).unwrap(), None);
    other.binding = s.binding;
    assert!(matches!(
        l.context_session(&other),
        Err(Error::Unauthorized)
    ));
}
#[test]
fn restart_retains_unreported_usage_and_late_receipt_is_charged_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    let mut l = setup(&path);
    l.reserve(&scope(), "c", "d", 80, 10).unwrap();
    drop(l);
    let mut l = Ledger::open(path, OWNER).unwrap();
    assert_eq!(l.recover_interrupted(OWNER).unwrap(), 1);
    assert_eq!(
        l.reserve(&scope(), "new", "d", 10, 86410).unwrap(),
        Reservation::New
    );
    assert_eq!(
        l.account(&scope(), Layer::Room, Period::UtcDay, 10)
            .unwrap(),
        (0, 80)
    );
    l.settle(&scope(), "c", &usage(60)).unwrap();
    l.settle(&scope(), "c", &usage(60)).unwrap();
    assert!(matches!(
        l.settle(&scope(), "c", &usage(61)),
        Err(Error::Conflict)
    ));
    assert_eq!(
        l.account(&scope(), Layer::Room, Period::UtcDay, 10)
            .unwrap(),
        (60, 0)
    );
    assert_eq!(
        l.account(&scope(), Layer::Room, Period::UtcDay, 86410)
            .unwrap(),
        (0, 10)
    );
    assert_eq!(
        l.reserve(&scope(), "c", "d", 80, 86410).unwrap(),
        Reservation::AlreadySettled(usage(60))
    );
    assert!(matches!(
        l.reserve(&scope(), "c", "other", 80, 10),
        Err(Error::Conflict)
    ));
}
fn tool(s: &Scope, revisions: [i64; 3], risk: &str) -> ToolProposal {
    ToolProposal {
        scope: s.clone(),
        dispatch: "dispatch".into(),
        tool: "Bash".into(),
        arguments: serde_json::json!({"command":"ls"}),
        canonical_directory: "/any/folder".into(),
        risk: risk.into(),
        policy_revision: revisions,
        expires: 100,
    }
}
/// Full trust on the Agent is the owner's trust in their own messages: for
/// those a Room or member layer the owner never saved adds no rule, and one the
/// owner saved still applies.
#[test]
fn full_trust_on_the_agent_covers_layers_the_owner_never_saved() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let mut s = scope();
    s.requester = OWNER.into();
    ledger.register_binding(OWNER, &s).unwrap();
    let mut trusted = config(1000, Period::Lifetime);
    trusted.high_risk = ToolPolicy::full_trust();
    ledger
        .set_policy(OWNER, &s, Layer::Agent, 0, &trusted)
        .unwrap();
    ledger.reserve(&s, "call", "dispatch", 10, 10).unwrap();
    assert!(ledger.fully_trusts(&s, "dispatch"));
    assert!(!ledger.fully_trusts(&s, "another dispatch"));
    let any = tool(&s, [1, 0, 0], "high");
    assert!(matches!(
        ledger.tool_disposition(&any, 11).unwrap(),
        ToolPolicy::AllowWithRules { .. }
    ));
    ledger.authorize_tool(&any, 11).unwrap();
    // A native request for wider permissions is still the owner's to answer.
    assert_eq!(
        ledger
            .tool_disposition(&tool(&s, [1, 0, 0], "permission_expansion"), 11)
            .unwrap(),
        ToolPolicy::AskOwner
    );
    // The owner saves "ask" for this member: that layer applies again.
    ledger
        .set_policy(
            OWNER,
            &s,
            Layer::Requester,
            0,
            &config(1000, Period::Lifetime),
        )
        .unwrap();
    assert!(!ledger.fully_trusts(&s, "dispatch"));
    assert_eq!(
        ledger
            .tool_disposition(&tool(&s, [1, 0, 1], "high"), 11)
            .unwrap(),
        ToolPolicy::AskOwner
    );
    let mut denied = config(1000, Period::Lifetime);
    denied.high_risk = ToolPolicy::Deny;
    ledger
        .set_policy(OWNER, &s, Layer::Requester, 1, &denied)
        .unwrap();
    assert!(matches!(
        ledger.tool_disposition(&tool(&s, [1, 0, 2], "high"), 11),
        Err(Error::Denied)
    ));
}
/// Another member's messages meet every layer under the Agent's full trust: an
/// unsaved Room or member layer still denies, and they are fully trusted only
/// where the owner also saved full trust on that Room and on that member.
#[test]
fn full_trust_on_the_agent_alone_gives_another_member_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let member = scope();
    assert_ne!(member.requester, OWNER);
    ledger.register_binding(OWNER, &member).unwrap();
    let mut trusted = config(1000, Period::Lifetime);
    trusted.high_risk = ToolPolicy::full_trust();
    ledger
        .set_policy(OWNER, &member, Layer::Agent, 0, &trusted)
        .unwrap();
    ledger.reserve(&member, "call", "dispatch", 10, 10).unwrap();
    assert!(!ledger.fully_trusts(&member, "dispatch"));
    let first = tool(&member, [1, 0, 0], "high");
    assert!(matches!(
        ledger.tool_disposition(&first, 11),
        Err(Error::Denied)
    ));
    assert!(matches!(
        ledger.authorize_tool(&first, 11),
        Err(Error::Denied)
    ));
    // The owner's own message in the same Room is covered.
    let mut own = member.clone();
    own.requester = OWNER.into();
    ledger.reserve(&own, "own call", "own", 10, 12).unwrap();
    assert!(ledger.fully_trusts(&own, "own"));
    // Full trust saved on the Room but not on this member: still denied.
    ledger
        .set_policy(OWNER, &member, Layer::Room, 0, &trusted)
        .unwrap();
    ledger.reserve(&member, "second", "room only", 10, 13).unwrap();
    assert!(!ledger.fully_trusts(&member, "room only"));
    let mut room_only = tool(&member, [1, 1, 0], "high");
    room_only.dispatch = "room only".into();
    assert!(matches!(
        ledger.tool_disposition(&room_only, 14),
        Err(Error::Denied)
    ));
    // Saved on this member too: now they are fully trusted.
    ledger
        .set_policy(OWNER, &member, Layer::Requester, 0, &trusted)
        .unwrap();
    ledger.reserve(&member, "third", "every layer", 10, 15).unwrap();
    assert!(ledger.fully_trusts(&member, "every layer"));
    let mut every = tool(&member, [1, 1, 1], "high");
    every.dispatch = "every layer".into();
    ledger.authorize_tool(&every, 16).unwrap();
    // A native request for wider permissions is still the owner's to answer.
    let mut wider = tool(&member, [1, 1, 1], "permission_expansion");
    wider.dispatch = "every layer".into();
    assert_eq!(
        ledger.tool_disposition(&wider, 16).unwrap(),
        ToolPolicy::AskOwner
    );
}
/// Without full trust on the Agent an unsaved layer keeps its default, which
/// denies, and full trust on a Room or member alone opens nothing.
#[test]
fn full_trust_below_the_agent_does_not_lift_the_agents_own_rule() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    let mut trusted = config(1000, Period::Lifetime);
    trusted.high_risk = ToolPolicy::full_trust();
    ledger
        .set_policy(OWNER, &s, Layer::Room, 0, &trusted)
        .unwrap();
    ledger
        .set_policy(OWNER, &s, Layer::Requester, 0, &trusted)
        .unwrap();
    ledger.reserve(&s, "call", "dispatch", 10, 10).unwrap();
    // The Agent layer was never saved: it denies.
    assert!(!ledger.fully_trusts(&s, "dispatch"));
    assert!(matches!(
        ledger.tool_disposition(&tool(&s, [0, 1, 1], "high"), 11),
        Err(Error::Denied)
    ));
    // The Agent asks its owner: trusted layers below add nothing to that.
    ledger
        .set_policy(OWNER, &s, Layer::Agent, 0, &config(1000, Period::Lifetime))
        .unwrap();
    ledger.reserve(&s, "next", "later", 10, 12).unwrap();
    let mut later = tool(&s, [1, 1, 1], "high");
    later.dispatch = "later".into();
    assert!(!ledger.fully_trusts(&s, "later"));
    assert_eq!(
        ledger.tool_disposition(&later, 13).unwrap(),
        ToolPolicy::AskOwner
    );
    // An Agent that asks, with a Room nobody saved, still denies there.
    let mut other = s.clone();
    other.binding = "b2".into();
    other.room = "!other:example.org".into();
    ledger.register_binding(OWNER, &other).unwrap();
    ledger.reserve(&other, "third", "elsewhere", 10, 14).unwrap();
    let mut elsewhere = tool(&other, [1, 0, 0], "high");
    elsewhere.dispatch = "elsewhere".into();
    assert!(matches!(
        ledger.tool_disposition(&elsewhere, 15),
        Err(Error::Denied)
    ));
}
/// Only the one rule that names every tool in every directory is full trust;
/// any other rule is matched exactly, as before.
#[test]
fn a_rule_that_only_resembles_full_trust_is_matched_exactly() {
    assert!(ToolPolicy::full_trust().is_full_trust());
    for (tools, directories) in [
        (vec!["*"], vec!["/workspace"]),
        (vec!["Bash"], vec!["/"]),
        (vec!["*", "Bash"], vec!["/"]),
        (vec!["*"], vec!["/", "/workspace"]),
    ] {
        let rule = ToolPolicy::AllowWithRules {
            tools: tools.into_iter().map(str::to_owned).collect(),
            directories: directories.into_iter().map(str::to_owned).collect(),
        };
        assert!(!rule.is_full_trust(), "{rule:?}");
    }
    assert!(!ToolPolicy::AskOwner.is_full_trust() && !ToolPolicy::Deny.is_full_trust());
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    let mut partial = config(1000, Period::Lifetime);
    partial.high_risk = ToolPolicy::AllowWithRules {
        tools: vec!["*".into()],
        directories: vec!["/any/folder".into()],
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &s, layer, 0, &partial).unwrap();
    }
    ledger.reserve(&s, "call", "dispatch", 10, 10).unwrap();
    assert!(!ledger.fully_trusts(&s, "dispatch"));
    assert!(matches!(
        ledger.tool_disposition(&tool(&s, [1, 1, 1], "high"), 11),
        Err(Error::Denied)
    ));
}
#[test]
fn saved_tool_rules_do_not_preapprove_native_permission_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    let mut p = config(1000, Period::Lifetime);
    p.high_risk = ToolPolicy::AllowWithRules {
        tools: vec!["codex.permissions".into()],
        directories: vec!["/workspace".into()],
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &s, layer, 0, &p).unwrap();
    }
    ledger.reserve(&s, "call", "dispatch", 10, 10).unwrap();
    let proposal = ToolProposal {
        scope: s,
        dispatch: "dispatch".into(),
        tool: "codex.permissions".into(),
        arguments: serde_json::json!({"fileSystem":{"write":["/outside"]}}),
        canonical_directory: "/workspace".into(),
        risk: "permission_expansion".into(),
        policy_revision: [1, 1, 1],
        expires: 100,
    };
    assert_eq!(
        ledger.tool_disposition(&proposal, 11).unwrap(),
        ToolPolicy::AskOwner
    );
    assert!(matches!(
        ledger.authorize_tool(&proposal, 11),
        Err(Error::Denied)
    ));
    assert!(matches!(
        ledger.approve_tool("@member:test", &proposal, 11),
        Err(Error::Unauthorized)
    ));
    ledger.approve_tool(OWNER, &proposal, 11).unwrap();
    let mut changed = proposal.clone();
    changed.arguments = serde_json::json!({"fileSystem":{"write":["/"]}});
    assert!(matches!(
        ledger.authorize_tool(&changed, 11),
        Err(Error::Denied)
    ));
    ledger.authorize_tool(&proposal, 11).unwrap();
    assert!(matches!(
        ledger.authorize_tool(&proposal, 11),
        Err(Error::Denied)
    ));
}
#[test]
fn tools_require_owner_parameter_bound_single_use_confirmation_and_current_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    l.set_policy(OWNER, &s, Layer::Agent, 0, &config(1000, Period::Lifetime))
        .unwrap();
    l.set_policy(
        OWNER,
        &s,
        Layer::Requester,
        0,
        &config(100, Period::Lifetime),
    )
    .unwrap();
    l.reserve(&s, "tool-call", "d", 1, 10).unwrap();
    let p = ToolProposal {
        scope: s.clone(),
        dispatch: "d".into(),
        tool: "shell".into(),
        arguments: serde_json::json!({"command":"pwd"}),
        canonical_directory: "/tmp/task".into(),
        risk: "high".into(),
        policy_revision: [1, 1, 1],
        expires: 100,
    };
    assert!(matches!(
        l.approve_tool("@mallory:example.org", &p, 10),
        Err(Error::Unauthorized)
    ));
    l.approve_tool(OWNER, &p, 10).unwrap();
    let mut changed = p.clone();
    changed.arguments = serde_json::json!({"command":"rm -rf /"});
    assert!(matches!(l.authorize_tool(&changed, 10), Err(Error::Denied)));
    l.authorize_tool(&p, 10).unwrap();
    assert!(matches!(l.authorize_tool(&p, 10), Err(Error::Denied)));
    let mut p2 = p.clone();
    p2.dispatch = "d2".into();
    l.reserve(&s, "tool-call2", "d2", 1, 10).unwrap();
    l.approve_tool(OWNER, &p2, 10).unwrap();
    l.set_policy(OWNER, &s, Layer::Room, 1, &config(100, Period::UtcDay))
        .unwrap();
    assert!(matches!(l.authorize_tool(&p2, 10), Err(Error::Denied)));
}
#[test]
fn request_ask_owner_cannot_be_reused_by_other_requester_or_after_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    let mut p = config(100, Period::Lifetime);
    p.requests = RequestPolicy::AskOwner;
    l.set_policy(OWNER, &s, Layer::Requester, 0, &p).unwrap();
    assert!(matches!(
        l.reserve(&s, "c", "d", 10, 10),
        Err(Error::Denied)
    ));
    l.approve_request(OWNER, &s, "d", 100, 10).unwrap();
    l.reserve(&s, "c", "d", 10, 10).unwrap();
    assert!(matches!(
        l.reserve(&s, "c2", "d2", 10, 10),
        Err(Error::Denied)
    ));
    l.set_policy(OWNER, &s, Layer::Requester, 1, &p).unwrap();
    assert!(matches!(
        l.reserve(&s, "c3", "d", 10, 10),
        Err(Error::Denied)
    ));
}
#[test]
fn foreign_schema_owner_and_scope_mismatch_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.db");
    Connection::open(&path)
        .unwrap()
        .execute("CREATE TABLE fleet(id TEXT)", [])
        .unwrap();
    assert!(matches!(
        Ledger::open(path, OWNER),
        Err(Error::ForeignDatabase)
    ));
    let path = tmp.path().join("local.db");
    let mut l = setup(&path);
    assert!(matches!(
        Ledger::open(path, "@mallory:example.org"),
        Err(Error::Unauthorized)
    ));
    let mut s = scope();
    s.agent = "other".into();
    assert!(matches!(
        l.register_binding(OWNER, &s),
        Err(Error::Unauthorized)
    ));
    assert!(
        l.db.execute("UPDATE identity SET owner='@mallory:example.org'", [])
            .is_err()
    );
    assert!(l.db.execute("DELETE FROM bindings", []).is_err());
}
#[test]
fn zero_and_overflow_reject_but_estimate_overrun_does_not_block() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    let mut p = config(0, Period::UtcDay);
    l.set_policy(OWNER, &s, Layer::Room, 1, &p).unwrap();
    assert!(matches!(l.reserve(&s, "c", "d", 1, 10), Err(Error::Budget)));
    p.budget.limit = Limit::Unlimited;
    l.set_policy(OWNER, &s, Layer::Room, 2, &p).unwrap();
    assert!(l.reserve(&s, "over", "d", u64::MAX, 10).is_err());
    l.reserve(&s, "c", "d", 10, 10).unwrap();
    l.settle(&s, "c", &usage(11)).unwrap();
    assert_eq!(
        l.account(&s, Layer::Room, Period::UtcDay, 10).unwrap(),
        (11, 0)
    );
    assert_eq!(
        l.reserve(&s, "next", "d", 1, 86410).unwrap(),
        Reservation::New
    );
}
#[test]
fn policy_widening_does_not_upgrade_an_existing_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    l.set_policy(OWNER, &s, Layer::Agent, 0, &config(1000, Period::Lifetime))
        .unwrap();
    let mut p = config(20, Period::Lifetime);
    p.high_risk = ToolPolicy::Deny;
    l.set_policy(OWNER, &s, Layer::Requester, 0, &p).unwrap();
    l.reserve(&s, "c", "old", 10, 10).unwrap();
    p.budget.limit = Limit::Unlimited;
    p.high_risk = ToolPolicy::AskOwner;
    l.set_policy(OWNER, &s, Layer::Requester, 1, &p).unwrap();
    assert!(matches!(
        l.reserve(&s, "c2", "old", 15, 10),
        Err(Error::Budget)
    ));
    l.reserve(&s, "c3", "new", 15, 10).unwrap();
    let proposal = ToolProposal {
        scope: s,
        dispatch: "old".into(),
        tool: "shell".into(),
        arguments: serde_json::json!({}),
        canonical_directory: "/tmp/task".into(),
        risk: "high".into(),
        policy_revision: [1, 1, 2],
        expires: 100,
    };
    assert!(matches!(
        l.approve_tool(OWNER, &proposal, 10),
        Err(Error::Denied)
    ));
}
#[test]
fn local_model_profile_contains_only_a_keychain_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    let mut profile = ModelProfile {
        model: "codex".into(),
        credential_ref: "raw-secret".into(),
        workspace_root: "/tmp/task".into(),
        reasoning_effort: String::new(),
        web_search: true,
    };
    assert!(l.set_model_profile(OWNER, &s, &profile).is_err());
    profile.credential_ref = "keychain:hagency/codex/alice".into();
    profile.reasoning_effort = "high".into();
    l.set_model_profile(OWNER, &s, &profile).unwrap();
    assert_eq!(l.model_profile(&s).unwrap(), Some(profile.clone()));
    drop(l);
    let mut reopened = Ledger::open(tmp.path().join("local.db"), OWNER).unwrap();
    assert_eq!(reopened.model_profile(&s).unwrap(), Some(profile.clone()));
    profile.reasoning_effort = "unsupported-effort".into();
    assert!(reopened.set_model_profile(OWNER, &s, &profile).is_err());
}
#[test]
fn web_search_defaults_on_and_disabled_profile_survives_restart_and_legacy_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    let mut ledger = setup(&path);
    let mut profile: ModelProfile = serde_json::from_value(serde_json::json!({"model":"codex","credential_ref":"keychain:owner","workspace_root":"/tmp/task","reasoning_effort":"low"})).unwrap();
    assert!(profile.web_search);
    profile.web_search = false;
    ledger.set_model_profile(OWNER, &scope(), &profile).unwrap();
    drop(ledger);
    let mut reopened = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(
        reopened.model_profile(&scope()).unwrap(),
        Some(profile.clone())
    );
    assert!(
        reopened
            .set_model_profile("@foreign:test", &scope(), &profile)
            .is_err()
    );
    reopened
        .db
        .execute_batch("ALTER TABLE model_profiles DROP COLUMN web_search")
        .unwrap();
    drop(reopened);
    let migrated = Ledger::open(&path, OWNER).unwrap();
    profile.web_search = true;
    assert_eq!(migrated.model_profile(&scope()).unwrap(), Some(profile));
}
#[test]
fn concurrent_tool_consumers_cannot_reuse_one_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    let mut l = setup(&path);
    let s = scope();
    l.set_policy(OWNER, &s, Layer::Agent, 0, &config(1000, Period::Lifetime))
        .unwrap();
    l.set_policy(
        OWNER,
        &s,
        Layer::Requester,
        0,
        &config(100, Period::Lifetime),
    )
    .unwrap();
    l.reserve(&s, "c", "d", 1, 10).unwrap();
    let proposal = ToolProposal {
        scope: s,
        dispatch: "d".into(),
        tool: "shell".into(),
        arguments: serde_json::json!({"command":"pwd"}),
        canonical_directory: "/tmp/task".into(),
        risk: "high".into(),
        policy_revision: [1, 1, 1],
        expires: 100,
    };
    l.approve_tool(OWNER, &proposal, 10).unwrap();
    drop(l);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let path = path.clone();
            let p = proposal.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut l = Ledger::open(path, OWNER).unwrap();
                barrier.wait();
                l.authorize_tool(&p, 10).is_ok()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
}

#[test]
fn unreported_cost_does_not_block_authorized_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let mut l = setup(&tmp.path().join("local.db"));
    let s = scope();
    for layer in [Layer::Agent, Layer::Requester] {
        l.set_policy(OWNER, &s, layer, 0, &config(100, Period::Lifetime))
            .unwrap();
    }
    l.reserve(&s, "c", "d", 1, 10).unwrap();
    let proposal = ToolProposal {
        scope: s.clone(),
        dispatch: "d".into(),
        tool: "shell".into(),
        arguments: serde_json::json!({"command":"pwd"}),
        canonical_directory: "/tmp/task".into(),
        risk: "high".into(),
        policy_revision: [1, 1, 1],
        expires: 100,
    };
    l.approve_tool(OWNER, &proposal, 10).unwrap();
    l.mark_unknown(&s, "c").unwrap();
    l.authorize_tool(&proposal, 11).unwrap();
    l.settle(&s, "c", &usage(1)).unwrap();
    assert!(l.authorize_tool(&proposal, 12).is_err());
}

#[test]
fn exact_request_confirmation_cannot_authorize_changed_policy_or_other_requester() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    let mut ledger = setup(&path);
    let s = scope();
    let mut ask = config(100, Period::Lifetime);
    ask.requests = RequestPolicy::AskOwner;
    ledger
        .set_policy(OWNER, &s, Layer::Requester, 0, &ask)
        .unwrap();
    assert_eq!(
        ledger.request_disposition(&s, "dispatch").unwrap(),
        RequestPolicy::AskOwner
    );
    let revisions = ledger.policy_snapshot(&s).unwrap().map(|v| v.revision);
    let mut concurrent = Ledger::open(&path, OWNER).unwrap();
    ask.budget.limit = Limit::Tokens(200);
    concurrent
        .set_policy(OWNER, &s, Layer::Requester, 1, &ask)
        .unwrap();
    assert!(matches!(
        ledger.approve_request_exact(OWNER, &s, "dispatch", revisions, 100, 10),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        ledger.reserve(&s, "call", "dispatch", 10, 10),
        Err(Error::Denied)
    ));
    let revisions = ledger.policy_snapshot(&s).unwrap().map(|v| v.revision);
    ledger
        .approve_request_exact(OWNER, &s, "dispatch", revisions, 100, 10)
        .unwrap();
    ledger.reserve(&s, "call", "dispatch", 10, 10).unwrap();
    let mut other = s.clone();
    other.requester = "@other:example.org".into();
    assert!(matches!(
        ledger.reserve(&other, "other-call", "dispatch", 10, 10),
        Err(Error::Unauthorized)
    ));
    ask.requests = RequestPolicy::Deny;
    concurrent
        .set_policy(OWNER, &s, Layer::Requester, 2, &ask)
        .unwrap();
    assert_eq!(
        ledger.request_disposition(&s, "dispatch").unwrap(),
        RequestPolicy::Deny
    );
    let revisions = ledger.policy_snapshot(&s).unwrap().map(|v| v.revision);
    assert!(matches!(
        ledger.approve_request_exact(OWNER, &s, "dispatch", revisions, 100, 10),
        Err(Error::Denied)
    ));
}

#[test]
fn profile_identity_survives_restore_and_rejects_same_mxid_foreign_profile_copy() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("a.sqlite");
    let copy = temp.path().join("b.sqlite");
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let mut ledger = Ledger::open_scoped(&original, OWNER, &a).unwrap();
    ledger.register_binding(OWNER, &scope()).unwrap();
    ledger
        .set_policy(
            OWNER,
            &scope(),
            Layer::Room,
            0,
            &config(40, Period::Lifetime),
        )
        .unwrap();
    drop(ledger);
    std::fs::copy(&original, &copy).unwrap();
    assert!(matches!(
        Ledger::open_scoped(&copy, OWNER, &b),
        Err(Error::Unauthorized)
    ));
    let restored = Ledger::open_scoped(&copy, OWNER, &a).unwrap();
    assert_eq!(
        restored.policy_snapshot(&scope()).unwrap()[1]
            .policy
            .budget
            .limit,
        Limit::Tokens(40)
    );
    assert!(
        restored
            .db
            .execute("UPDATE profile_identity SET digest=?", [b])
            .is_err()
    );
    assert!(
        restored
            .db
            .execute("DELETE FROM profile_identity", [])
            .is_err()
    );
    let unscoped = temp.path().join("unscoped.sqlite");
    let mut old = Ledger::open(&unscoped, OWNER).unwrap();
    old.register_binding(OWNER, &scope()).unwrap();
    drop(old);
    assert!(matches!(
        Ledger::open_scoped(&unscoped, OWNER, &a),
        Err(Error::ForeignDatabase)
    ));
}

#[test]
fn simultaneous_fresh_profile_openers_share_one_initialized_schema() {
    let tmp = tempfile::tempdir().unwrap();
    for attempt in 0..12 {
        let path = tmp.path().join(format!("fresh-{attempt}.sqlite"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Ledger::open_scoped(path, OWNER, &"a".repeat(64)).map(|_| ())
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        let l = Ledger::open_scoped(&path, OWNER, &"a".repeat(64)).unwrap();
        assert_eq!(
            l.db.query_row("SELECT count(*) FROM identity", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            l.db.query_row("SELECT count(*) FROM profile_identity", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(matches!(
            Ledger::open_scoped(&path, OWNER, &"b".repeat(64)),
            Err(Error::Unauthorized)
        ));
    }
}

#[test]
fn missing_limits_allow_requests_and_still_account_for_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("optional-limits.db");
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    for version in ledger.policy_snapshot(&s).unwrap() {
        assert_eq!(version.revision, 0);
        assert_eq!(version.policy.budget.limit, Limit::Unlimited);
        assert_eq!(version.policy.requests, RequestPolicy::Allow);
        assert_eq!(version.policy.high_risk, ToolPolicy::Deny);
    }
    assert_eq!(
        ledger.request_disposition(&s, "d").unwrap(),
        RequestPolicy::Allow
    );
    ledger.reserve(&s, "c", "d", 40, 10).unwrap();
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        assert_eq!(
            ledger.account(&s, layer, Period::Lifetime, 10).unwrap(),
            (0, 40)
        );
    }
    ledger.settle(&s, "c", &usage(25)).unwrap();
    drop(ledger);
    let ledger = Ledger::open(path, OWNER).unwrap();
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        assert_eq!(
            ledger.account(&s, layer, Period::Lifetime, 10).unwrap(),
            (25, 0)
        );
    }
}

#[test]
fn every_configured_layer_limits_spent_and_concurrent_holds() {
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        let tmp = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(tmp.path().join("limits.db"), OWNER).unwrap();
        let s = scope();
        ledger.register_binding(OWNER, &s).unwrap();
        ledger
            .set_policy(OWNER, &s, layer.clone(), 0, &config(20, Period::Lifetime))
            .unwrap();
        assert!(matches!(
            ledger.reserve(&s, "too-big", "d0", 21, 10),
            Err(Error::Budget)
        ));
        ledger.reserve(&s, "pending", "d1", 20, 10).unwrap();
        assert!(matches!(
            ledger.reserve(&s, "held", "d2", 1, 10),
            Err(Error::Budget)
        ));
        ledger.settle(&s, "pending", &usage(10)).unwrap();
        assert!(matches!(
            ledger.reserve(&s, "spent", "d3", 11, 10),
            Err(Error::Budget)
        ));
        ledger.reserve(&s, "remaining", "d4", 10, 10).unwrap();
        assert_eq!(
            ledger.account(&s, layer, Period::Lifetime, 10).unwrap(),
            (10, 10)
        );
    }
}

#[test]
fn explicit_denial_remains_effective_without_any_token_limit() {
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        let tmp = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(tmp.path().join("denial.db"), OWNER).unwrap();
        let s = scope();
        ledger.register_binding(OWNER, &s).unwrap();
        let mut policy = ledger.policy_snapshot(&s).unwrap()[0].policy.clone();
        policy.requests = RequestPolicy::Deny;
        ledger.set_policy(OWNER, &s, layer, 0, &policy).unwrap();
        assert_eq!(
            ledger.request_disposition(&s, "d").unwrap(),
            RequestPolicy::Deny
        );
        assert!(matches!(
            ledger.reserve(&s, "c", "d", 1, 10),
            Err(Error::Denied)
        ));
        for checked in [Layer::Agent, Layer::Room, Layer::Requester] {
            assert_eq!(
                ledger.account(&s, checked, Period::Lifetime, 10).unwrap(),
                (0, 0)
            );
        }
    }
}

#[test]
fn retained_unset_is_unrestricted_but_does_not_widen_captured_dispatch_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("unset.db");
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    let mut policy = config(100, Period::Lifetime);
    policy.budget.limit = Limit::Unset;
    ledger
        .set_policy(OWNER, &s, Layer::Room, 0, &policy)
        .unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(
        ledger.policy_snapshot(&s).unwrap()[1].policy.budget.limit,
        Limit::Unset
    );
    ledger
        .reserve(&s, "unrestricted", "old-unset", 10, 10)
        .unwrap();
    ledger.settle(&s, "unrestricted", &usage(5)).unwrap();
    policy.budget.limit = Limit::Tokens(20);
    ledger
        .set_policy(OWNER, &s, Layer::Room, 1, &policy)
        .unwrap();
    // A newly tightened limit also applies to a dispatch captured without a limit.
    assert!(matches!(
        ledger.reserve(&s, "tightened", "old-unset", 16, 10),
        Err(Error::Budget)
    ));
    ledger
        .reserve(&s, "limited", "old-limited", 10, 10)
        .unwrap();
    ledger.settle(&s, "limited", &usage(5)).unwrap();
    policy.budget.limit = Limit::Unset;
    ledger
        .set_policy(OWNER, &s, Layer::Room, 2, &policy)
        .unwrap();
    // Removing the current limit does not upgrade a previously captured limit.
    assert!(matches!(
        ledger.reserve(&s, "widened", "old-limited", 11, 10),
        Err(Error::Budget)
    ));
    ledger
        .reserve(&s, "fresh", "new-unrestricted", 100, 10)
        .unwrap();
}

#[test]
fn unlimited_defaults_preserve_unreported_charges_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("unknown-unlimited.db");
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let s = scope();
    ledger.register_binding(OWNER, &s).unwrap();
    ledger.reserve(&s, "c", "d", 40, 10).unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(path, OWNER).unwrap();
    assert_eq!(ledger.recover_interrupted(OWNER).unwrap(), 1);
    assert_eq!(ledger.outstanding_calls(OWNER).unwrap()[0].state, "unknown");
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        assert_eq!(
            ledger.account(&s, layer, Period::Lifetime, 86410).unwrap(),
            (0, 40)
        );
    }
    ledger.settle(&s, "c", &usage(25)).unwrap();
    ledger
        .reserve(&s, "next", "next-dispatch", 1, 86410)
        .unwrap();
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        assert_eq!(
            ledger.account(&s, layer, Period::Lifetime, 86410).unwrap(),
            (25, 1)
        );
    }
}

#[test]
fn legacy_unknown_hold_needs_receipt_without_replaying_or_losing_context() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.db");
    let mut l = setup(&path);
    let s = scope();
    l.reserve(&s, "old-call", "old-dispatch", 80, 10).unwrap();
    l.db.execute("UPDATE calls SET state='unknown' WHERE id='old-call'", [])
        .unwrap();
    let context = s.context_key(OWNER).unwrap();
    l.db.execute(
        "INSERT INTO contexts VALUES (?,?)",
        (&context, "original-thread"),
    )
    .unwrap();
    drop(l);
    let mut l = Ledger::open(&path, OWNER).unwrap();
    assert_eq!(
        l.reserve(&s, "next-call", "next-dispatch", 10, 10).unwrap(),
        Reservation::New
    );
    assert_eq!(
        l.account(&s, Layer::Room, Period::UtcDay, 10).unwrap(),
        (0, 90)
    );
    assert!(matches!(
        l.reserve(&s, "old-call", "old-dispatch", 80, 10),
        Err(Error::Unknown)
    ));
    assert_eq!(
        l.db.query_row(
            "SELECT model_session FROM contexts WHERE scope=?",
            [&context],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "original-thread"
    );
    assert_eq!(
        l.outstanding_calls(OWNER)
            .unwrap()
            .iter()
            .find(|c| c.id == "old-call")
            .unwrap()
            .state,
        "unknown"
    );
    l.settle(&s, "old-call", &usage(25)).unwrap();
    l.settle(&s, "old-call", &usage(25)).unwrap();
    assert_eq!(
        l.account(&s, Layer::Room, Period::UtcDay, 10).unwrap(),
        (25, 10)
    );
}

#[test]
fn model_profile_checked_edit_is_atomic_scoped_and_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("checked.db");
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let empty = model_profile_digest(None).unwrap();
    assert_eq!(empty, format!("{:x}", Sha256::digest(b"null")));
    let first = ModelProfile {
        model: "fixture".into(),
        credential_ref: "keychain:codex:owner".into(),
        workspace_root: tmp.path().to_string_lossy().into(),
        reasoning_effort: "high".into(),
        web_search: true,
    };
    assert!(matches!(
        ledger.set_agent_model_profile_checked("@other:test", "a1", &empty, &first),
        Err(Error::Unauthorized)
    ));
    ledger
        .set_agent_model_profile_checked(OWNER, "a1", &empty, &first)
        .unwrap();
    let first_digest = model_profile_digest(Some(&first)).unwrap();
    assert_ne!(empty, first_digest);
    let mut changed = first.clone();
    changed.web_search = false;
    assert!(matches!(
        ledger.set_agent_model_profile_checked(OWNER, "a1", &empty, &changed),
        Err(Error::Conflict)
    ));
    assert_eq!(
        ledger.agent_model_profile(OWNER, "a1").unwrap(),
        Some(first.clone())
    );
    let mut second_handle = Ledger::open(&path, OWNER).unwrap();
    ledger
        .set_agent_model_profile_checked(OWNER, "a1", &first_digest, &changed)
        .unwrap();
    let mut stale = first.clone();
    stale.workspace_root.push_str("/stale");
    assert!(matches!(
        second_handle.set_agent_model_profile_checked(OWNER, "a1", &first_digest, &stale),
        Err(Error::Conflict)
    ));
    let digest = model_profile_digest(Some(&changed)).unwrap();
    let mut invalid = changed.clone();
    invalid.credential_ref = "secret-token".into();
    assert!(
        ledger
            .set_agent_model_profile_checked(OWNER, "a1", &digest, &invalid)
            .is_err()
    );
    ledger.db.execute_batch("CREATE TRIGGER reject_model BEFORE UPDATE ON model_profiles BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        ledger
            .set_agent_model_profile_checked(OWNER, "a1", &digest, &first)
            .is_err()
    );
    assert_eq!(
        ledger.agent_model_profile(OWNER, "a1").unwrap(),
        Some(changed.clone())
    );
    drop(ledger);
    drop(second_handle);
    assert_eq!(
        Ledger::open(&path, OWNER)
            .unwrap()
            .agent_model_profile(OWNER, "a1")
            .unwrap(),
        Some(changed)
    );
}

#[test]
fn analytics_preserves_cache_writes_without_changing_budget_charge() {
    let measured = Usage {
        input: 17,
        output: 3,
        cached_input: 5,
        cache_write_input: Some(2),
        reasoning_output: 0,
        accounting_version: "claude-reported-v1".into(),
    };
    assert_eq!(measured.total().unwrap(), 20);
    assert_eq!(
        measured.category_counts(),
        (Some(10), Some(5), Some(2), Some(3))
    );
    let historical: Usage = serde_json::from_str(r#"{"input":17,"output":3,"cached_input":5,"reasoning_output":0,"accounting_version":"octos-reported-v1"}"#).unwrap();
    assert_eq!(historical.total().unwrap(), 20);
    assert_eq!(historical.category_counts(), (None, Some(5), None, Some(3)));
    assert_eq!(
        unreported_usage().category_counts(),
        (None, None, None, None)
    );
    let invalid = Usage {
        cache_write_input: Some(13),
        ..measured
    };
    assert!(invalid.total().is_err());
    assert_eq!(invalid.category_counts().0, None);
}
