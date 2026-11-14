use super::*;

#[test]
fn upstream_hint_accepts_only_upstream_ids() {
    assert_eq!(upstream_login_hint(Some(GITHUB_LOGIN_HINT)), Some("upstream:github"));
    assert_eq!(upstream_login_hint(Some("github")), None);
    assert_eq!(upstream_login_hint(None), None);
}

#[test]
fn github_button_needs_provider_oauth_and_sign_in_view() {
    assert!(github_button_visible(true, true, false));
    assert!(!github_button_visible(false, true, false));
    assert!(!github_button_visible(true, false, false));
    assert!(!github_button_visible(true, true, true));
}

#[test]
fn optional_email_blank_valid_or_rejected() {
    assert_eq!(validate_optional_email("   "), Ok(None));
    assert_eq!(validate_optional_email(" a@example.com "), Ok(Some("a@example.com".to_owned())));
    assert!(validate_optional_email("not-an-email").is_err());
}

#[test]
fn recovery_state_controls_panel_and_send() {
    assert!(!RecoveryState::Closed.is_open());
    assert!(RecoveryState::EnterEmail.is_open());
    assert!(RecoveryState::EnterEmail.can_send());
    assert!(RecoveryState::Failed("x".into()).can_send());
    assert!(!RecoveryState::Sending.can_send());
    assert!(!RecoveryState::CheckEmail.can_send());
}

#[test]
fn recovery_failure_text_includes_reason() {
    assert!(RecoveryState::Failed("smtp down".into()).status_text().contains("smtp down"));
}

#[test]
fn request_ids_are_unique_so_replies_never_cross_flows() {
    let first = next_request_id();
    let second = next_request_id();
    assert_ne!(first, second);
    assert!(accepts_reply(Some(second), second));
    // A reply to an older request, or after a reset/close, is dropped.
    assert!(!accepts_reply(Some(second), first));
    assert!(!accepts_reply(None, second));
}

#[test]
fn pending_registration_keeps_the_submitted_credentials() {
    let pending = PendingRegistration { server: "https://hs".into(), username: "alice".into(), password: "pw1".into(), registration_id: None };
    let mut stored = pending.clone();
    stored.registration_id = Some("reg".into());
    assert_eq!((stored.username.as_str(), stored.password.as_str(), stored.server.as_str()), ("alice", "pw1", "https://hs"));
}
