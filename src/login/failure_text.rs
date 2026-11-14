//! Turns raw sign-in and registration errors into text a person can act on.

/// Server error code Pasion returns when a new password scores too low.
const WEAK_PASSWORD_CODE: &str = "password_too_weak";
const WEAK_PASSWORD_TEXT: &str =
    "Choose a stronger password: try 3–4 random words with a number, like purple-river-lamp-42.";

/// Title of the failure dialog: sign-up errors must not read as sign-in errors.
pub fn login_failure_title(register: bool) -> &'static str {
    if register { "Couldn't create account" } else { "Sign-in failed" }
}

/// Message shown when registration or sign-in fails with `error`.
pub fn login_failure_text(register: bool, error: &str) -> String {
    if error.contains(WEAK_PASSWORD_CODE) {
        return crate::i18n::tr(WEAK_PASSWORD_TEXT).to_string();
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
