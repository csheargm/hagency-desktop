//! The runtime start request one-click setup sends.
use serde_json::Value;

/// The runtime start body. The endpoint rejects unknown fields (`invalid_arguments`),
/// so only `RuntimeStartRequest` fields may appear; it has no token reservation,
/// the runtime chooses its own, so `reservation` is not sent.
pub(super) fn input(_reservation: u64) -> Value {
    serde_json::to_value(crate::native_owner::RuntimeStartRequest { takeover: true, ..Default::default() }).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    /// Regression: the start body carried `reservation`, which the endpoint refuses as an unknown field.
    #[test]
    fn start_body_is_accepted_by_the_runtime_start_endpoint() {
        let body = super::input(0);
        let parsed: crate::native_owner::RuntimeStartRequest = serde_json::from_value(body.clone()).expect("endpoint accepts it");
        assert!(parsed.takeover);
        assert!(body.get("reservation").is_none());
    }
}
