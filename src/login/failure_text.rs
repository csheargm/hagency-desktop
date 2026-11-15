//! Turns raw sign-in and registration errors into text a person can act on.

/// Server error code Pasion returns when a new password scores too low.
const WEAK_PASSWORD_CODE: &str = "password_too_weak";
const WEAK_PASSWORD_TEXT: &str =
    "Choose a stronger password: try 3–4 random words with a number, like purple-river-lamp-42.";
/// Pasion refused sign-up because another account already has the name.
const USERNAME_TAKEN_TEXT: &str =
    "That username is already taken on this server. Choose another name, or sign in if it is your account.";
/// The SDK's text for an OAuth `access_denied` callback. The in-app password
/// sign-in reports its own failures that way, so this means Pasion refused
/// the username and password, not that a person cancelled anything.
const ACCESS_DENIED_SDK_TEXT: &str = "authorization cancelled by the user";
const ACCESS_DENIED_TEXT: &str =
    "The server did not accept this username and password. Check them and try again.";

/// Title of the failure dialog: sign-up errors must not read as sign-in errors.
pub fn login_failure_title(register: bool) -> &'static str {
    if register { "Couldn't create account" } else { "Sign-in failed" }
}

/// Message shown when registration or sign-in fails with `error`.
pub fn login_failure_text(register: bool, error: &str) -> String {
    if error.contains(WEAK_PASSWORD_CODE) {
        return crate::i18n::tr(WEAK_PASSWORD_TEXT).to_string();
    }
    if error.contains(::hagency::pasion::account::USERNAME_TAKEN_CODE) {
        return crate::i18n::tr(USERNAME_TAKEN_TEXT).to_string();
    }
    if error.contains(ACCESS_DENIED_SDK_TEXT) {
        return crate::i18n::tr(ACCESS_DENIED_TEXT).to_string();
    }
    format!("{} failed: {error}", if register { "Registration" } else { "Sign-in" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_password_gets_a_hint_instead_of_raw_json() {
        let raw = r#"pasion login failed at register: status 200 OK: {"error":"password_too_weak","status":"error"}"#;
        let text = login_failure_text(true, raw);
        assert!(text.contains("stronger password"), "{text}");
        assert!(!text.contains("pasion"), "{text}");
    }

    #[test]
    fn a_taken_username_says_so() {
        let raw = "pasion login failed at register: username_taken";
        assert_eq!(login_failure_text(true, raw), USERNAME_TAKEN_TEXT);
    }

    #[test]
    fn a_refused_in_app_sign_in_does_not_claim_the_user_cancelled() {
        let raw = "authorization code grant failed: authorization cancelled by the user";
        assert_eq!(login_failure_text(true, raw), ACCESS_DENIED_TEXT);
    }

    #[test]
    fn registration_failures_are_titled_as_account_creation() {
        assert_eq!(login_failure_title(true), "Couldn't create account");
        assert_eq!(login_failure_title(false), "Sign-in failed");
    }

    #[test]
    fn other_errors_keep_their_detail() {
        assert_eq!(login_failure_text(true, "taken"), "Registration failed: taken");
        assert_eq!(login_failure_text(false, "bad"), "Sign-in failed: bad");
    }
}
