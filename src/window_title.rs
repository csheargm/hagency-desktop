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

/// Sets the window title for the current account. Returns true once it is final: signed out,
/// or the display name was known (the localpart fallback is retried by the caller).
pub fn refresh(cx: &mut Cx, window: &WindowRef, signed_in: bool) -> bool {
    let user = if signed_in { crate::sliding_sync::current_user_id() } else { None };
    let Some(user) = user else {
        window.set_title(cx, APP_NAME);
        return true;
    };
    // No fetch here: the account bar already requests the own profile.
    let name = crate::profile::user_profile_cache::with_user_profile(cx, user.clone(), None, false, |profile, _| profile.username.clone()).flatten();
    window.set_title(cx, &title_for(name.as_deref(), Some(&user)));
    name.is_some_and(|name| !name.trim().is_empty())
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
}
