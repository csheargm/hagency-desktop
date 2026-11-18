use super::*;

/// The owner's Claude Code is signed out: the check before any model input
/// names the Claude login, so the host can answer that the request never ran.
#[tokio::test]
async fn a_signed_out_claude_is_reported_before_any_model_input() {
    let mut f = Fixture::new(Kind::Claude, "signed_out", ToolPolicy::Deny);
    let guardian = std::env::var_os("HAGENCY_TEST_GUARDIAN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe")));
    let mut process = spawn_for_scope_with_guardian(&f.profile, &f.reference, &mut f.ledger, &f.scope, Some(&guardian))
        .await
        .unwrap();
    process.session.initialize().await.unwrap();
    process.session.verify_host_environment().await.unwrap();
    let error = process.session.require_local_account().await.unwrap_err();
    assert!(matches!(error, Error::Profile(CLAUDE_LOGIN_REQUIRED)), "{error:?}");
    let frames = fs::read_to_string(f.path("frames")).unwrap_or_default();
    assert!(!frames.contains("\"type\": \"user\"") && !frames.contains("\"type\":\"user\""), "no model input was sent");
    assert!(f.ledger.outstanding_calls("@owner:test").unwrap().is_empty());
    process.stop().await.unwrap();
}
