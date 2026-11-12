#![allow(dead_code)]
use hagency_agent_local::{
    Budget, Layer, Ledger, Limit, Period, Policy, RequestPolicy, Scope, ToolPolicy,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
pub const OWNER: &str = "@release-owner:example.test";
pub fn owned_path(state: &Path) -> PathBuf {
    let digest = Sha256::digest(serde_json::to_vec(&("https://example.test", OWNER)).unwrap());
    state
        .join("owned-agent-owners")
        .join(format!("owner_{digest:x}"))
        .join("agent-local.sqlite")
}
pub fn scope() -> Scope {
    Scope {
        agent: "release-agent".into(),
        binding: "release-binding".into(),
        room: "!release:example.test".into(),
        requester: OWNER.into(),
        thread: "$release-thread".into(),
    }
}
/// Actual owner ledger: absent usage is zero, known usage survives. No provider is called.
pub fn seed_owned_state(state: &Path) {
    let path = owned_path(state);
    hagency_store::private::directory(&state.join("owned-agent-owners")).unwrap();
    hagency_store::private::directory(path.parent().unwrap()).unwrap();
    hagency_store::private::write_new(&path, b"").unwrap();
    let mut ledger = Ledger::open(&path, OWNER).unwrap();
    let scope = scope();
    ledger.register_binding(OWNER, &scope).unwrap();
    let policy = Policy {
        budget: Budget {
            limit: Limit::Tokens(1000),
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        ledger.set_policy(OWNER, &scope, layer, 0, &policy).unwrap();
    }
    ledger
        .reserve(&scope, "release-unknown-call", "release-dispatch", 100, 10)
        .unwrap();
    ledger.mark_unknown(&scope, "release-unknown-call").unwrap();
    ledger
        .reserve(&scope, "release-known-call", "known-dispatch", 100, 10)
        .unwrap();
    ledger
        .settle(
            &scope,
            "release-known-call",
            &hagency_agent_local::Usage {
                input: 20,
                output: 5,
                cached_input: 0,
                cache_write_input: None,
                reasoning_output: 0,
                accounting_version: "release-fixture".into(),
            },
        )
        .unwrap();
}
pub fn assert_owned_survives(state: &Path) {
    let mut ledger = Ledger::open(owned_path(state), OWNER).unwrap();
    let outstanding = ledger.outstanding_calls(OWNER).unwrap();
    let original = outstanding
        .iter()
        .find(|call| call.id == "release-unknown-call")
        .unwrap();
    assert_eq!(original.state, "unknown");
    assert_eq!(original.reserved, 100);
    let prior_check = outstanding.iter().any(|call| call.id == "different-call");
    assert_eq!(outstanding.len(), if prior_check { 2 } else { 1 });
    assert!(
        matches!(
            ledger.reserve(
                &scope(),
                "release-unknown-call",
                "release-dispatch",
                100,
                10,
            ),
            Err(hagency_agent_local::Error::Unknown)
        ),
        "the original unresolved call must survive without being replayed"
    );
    for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
        assert_eq!(
            ledger
                .account(&scope(), layer, Period::Lifetime, 10)
                .unwrap(),
            (25, if prior_check { 110 } else { 100 })
        );
    }
    assert!(matches!(
        ledger.reserve(&scope(), "different-call", "different-dispatch", 10, 10),
        Ok(hagency_agent_local::Reservation::New) | Err(hagency_agent_local::Error::Unknown)
    ));
    ledger.mark_unknown(&scope(), "different-call").unwrap();
    assert!(Ledger::open(owned_path(state), "@other:example.test").is_err());
    assert!(!state.join("domain.sqlite3").exists());
    assert!(!state.join("operator.token").exists());
}
