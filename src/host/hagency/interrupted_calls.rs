//! Closed native API for an owner's interrupted pool-Agent calls. The host
//! never supplies usage figures of its own; the owner's declaration travels
//! unchanged to the device ledger, which bounds it by what was observed.
use super::{parse, Value};
use ::hagency::native_owner::{InterruptedCallQuery, ReconcileInterruptedCall};
pub(super) const LIST: &str = "hagency.agents.calls.list";
pub(super) const RECONCILE: &str = "hagency.agents.calls.reconcile";
pub(super) enum Call {
    List(InterruptedCallQuery),
    Reconcile(ReconcileInterruptedCall),
}
pub(super) fn call(service: &str, args: Value) -> Result<Option<Call>, String> {
    let invalid = |_| "invalid_management_arguments".to_owned();
    Ok(Some(match service {
        LIST => {
            let q: InterruptedCallQuery = parse(args)?;
            q.validate().map_err(invalid)?;
            Call::List(q)
        }
        RECONCILE => {
            let q: ReconcileInterruptedCall = parse(args)?;
            q.validate().map_err(invalid)?;
            Call::Reconcile(q)
        }
        _ => return Ok(None),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn interrupted_call_services_are_closed_and_declared() {
        let reconcile = json!({"commandId":"cmd","agentId":"agt_1","callId":"mac_1","expectedReservedTokens":50000000,"consumedTokens":0,"evidenceReference":"process ended before any model request"});
        for (service, args) in [(LIST, json!({"agentId":"agt_1"})), (RECONCILE, reconcile.clone())] {
            assert!(call(service, args.clone()).unwrap().is_some());
            assert!(octosense_app_contract::hagency::SERVICES.contains(&service));
            assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&service));
            assert!(octosense_app_contract::hagency::words(service).is_some());
            for field in ["owner", "profile", "token", "reservationId", "force", "path", "knownConsumedTokens"] {
                let mut forged = args.clone();
                forged[field] = json!("forged");
                assert!(call(service, forged).is_err(), "{service}: {field}");
            }
        }
        assert!(call("hagency.agents.calls.other", json!({})).unwrap().is_none());
    }
    #[test]
    fn reconciliation_declaration_is_bounded_before_it_leaves_the_host() {
        let base = json!({"commandId":"cmd","agentId":"agt_1","callId":"mac_1","expectedReservedTokens":100,"consumedTokens":40,"evidenceReference":"provider usage page"});
        let Some(Call::Reconcile(q)) = call(RECONCILE, base.clone()).unwrap() else { panic!("expected reconciliation") };
        assert_eq!((q.consumed_tokens, q.expected_reserved_tokens), (40, 100));
        // Runs are admitted postpaid: an old estimate or a zero reservation
        // never caps what the owner declares as actually used.
        let mut above = base.clone();
        above["consumedTokens"] = json!(101);
        assert!(matches!(call(RECONCILE, above), Ok(Some(Call::Reconcile(q))) if q.consumed_tokens == 101));
        for (field, value) in [
            ("consumedTokens", json!(-1)),
            ("consumedTokens", json!(9_007_199_254_740_992u64)),
            ("evidenceReference", json!("")),
            ("evidenceReference", json!("   ")),
            ("evidenceReference", json!("a\u{7}b")),
            ("evidenceReference", json!("x".repeat(257))),
            ("callId", json!("")),
            ("agentId", json!("../other")),
        ] {
            let mut invalid = base.clone();
            invalid[field] = value;
            assert!(call(RECONCILE, invalid).is_err(), "{field}");
        }
    }
}
