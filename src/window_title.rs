//! Main window title: who is signed in, and where.
//!
//! "Hagency — ann · localhost:8450" while signed in (display name, falling back to the
//! localpart until the profile arrives), plain "Hagency" when signed out.

use makepad_widgets::{Cx, WindowRef};
use matrix_sdk::ruma::UserId;

/// The app name shown alone when nobody is signed in.
pub const APP_NAME: &str = "Hagency";

/// Title for `user` (None = signed out), using `display_name` when it is known and non-empty.
pub fn title_for(display_name: Option<&str>, user: Option<&UserId>) -> String {
    let Some(user) = user else { return APP_NAME.to_string() };
    let name = display_name.map(str::trim).filter(|name| !name.is_empty()).unwrap_or(user.localpart());
    format!("{APP_NAME} — {name} · {}", user.server_name())
}

/// Sets the window title for the current account when it differs from `shown`
/// (the title last set). Called on every action pass, so a display name that
/// arrives late or changes (here or on another device) retitles the window.
pub fn refresh(cx: &mut Cx, window: &WindowRef, signed_in: bool, shown: &mut String) {
    let user = if signed_in { crate::sliding_sync::current_user_id() } else { None };
    // No fetch here: the account bar already requests the own profile.
    let name = user.as_ref().and_then(|user| {
        crate::profile::user_profile_cache::with_user_profile(cx, user.clone(), None, false, |profile, _| profile.username.clone()).flatten()
    });
    if let Some(title) = retitle(shown, title_for(name.as_deref(), user.as_deref())) {
        window.set_title(cx, title);
    }
}
/// The title to set, or None when the window already shows `title`.
fn retitle(shown: &mut String, title: String) -> Option<&str> {
    if *shown == title {
        return None;
    }
    *shown = title;
    Some(shown.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn signed_out_shows_only_the_app_name() {
        assert_eq!(title_for(Some("Ann"), None), "Hagency");
    }

    #[test]
    fn signed_in_shows_display_name_and_homeserver_with_port() {
        assert_eq!(title_for(Some("ann"), Some(user_id!("@ann:localhost:8450"))), "Hagency — ann · localhost:8450");
    }

    #[test]
    fn missing_or_blank_display_name_falls_back_to_localpart() {
        let ben = user_id!("@ben:localhost:8452");
        assert_eq!(title_for(None, Some(ben)), "Hagency — ben · localhost:8452");
        assert_eq!(title_for(Some("  "), Some(ben)), "Hagency — ben · localhost:8452");
    }

    /// Regression: the title kept the first display name for the whole session.
    #[test]
    fn a_renamed_account_is_retitled_once() {
        let user = user_id!("@ann:example.org");
        let mut shown = String::new();
        assert!(retitle(&mut shown, title_for(Some("Ann"), Some(user))).is_some());
        assert_eq!(retitle(&mut shown, title_for(Some("Ann"), Some(user))), None);
        assert_eq!(retitle(&mut shown, title_for(Some("Alice"), Some(user))), Some("Hagency — Alice · example.org"));
    }
}
