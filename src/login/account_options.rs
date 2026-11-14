//! Login-screen account options that need Pasion before sign-in:
//! "Continue with GitHub", "Forgot password?" and registration with an
//! optional email. The screen owns the widgets; this module owns the rules,
//! the state and the background calls, so they stay testable without a UI.

/// Pasion `login_hint` that skips its chooser and opens GitHub directly.
pub const GITHUB_LOGIN_HINT: &str = "upstream:github";
/// Prefix that marks a provider id as a Pasion upstream hint.
const UPSTREAM_HINT_PREFIX: &str = "upstream:";

/// Source of request ids. Process-wide, so a reply can only ever match the
/// one login flow (in any window) that is still waiting for it.
static NEXT_REQUEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A fresh id for one background account call.
pub fn next_request_id() -> u64 { NEXT_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }

/// Whether a reply tagged `request` is the one `pending` is waiting for.
/// Stale replies (after a reset, close or newer request) are dropped.
pub fn accepts_reply(pending: Option<u64>, request: u64) -> bool { pending == Some(request) }

/// The server and credentials an email registration was started with, kept
/// so the code step and the automatic sign-in use exactly these values even
/// if the user edits the form while waiting for the email.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PendingRegistration {
    pub server: String,
    pub username: String,
    pub password: String,
    /// Set once Pasion has emailed a registration code.
    pub registration_id: Option<String>,
}

/// Results of background account calls, posted back to the login screen.
#[derive(Clone, Debug, Default)]
pub enum AccountOptionsAction {
    /// Whether the checked server advertises a GitHub provider.
    GithubAvailability { generation: u64, available: bool },
    /// Registration with an email either finished or needs the emailed code.
    EmailRegistration { request: u64, result: Result<EmailStep, String> },
    /// Recovery request accepted (`Ok`) or refused with a reason.
    RecoveryRequested { request: u64, result: Result<(), String> },
    #[default]
    None,
}

/// Where registration with an email stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmailStep {
    /// The account exists; continue with the normal sign-in.
    Registered,
    /// Pasion emailed a code that the user must enter.
    NeedsCode { registration_id: String },
}

/// The "Forgot password?" panel's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RecoveryState {
    #[default]
    Closed,
    EnterEmail,
    Sending,
    CheckEmail,
    Failed(String),
}

impl RecoveryState {
    /// Whether the panel is shown.
    pub fn is_open(&self) -> bool { *self != Self::Closed }
    /// Whether the send button may be pressed.
    pub fn can_send(&self) -> bool { matches!(self, Self::EnterEmail | Self::Failed(_)) }
    /// The status line under the email field (already translated).
    pub fn status_text(&self) -> String {
        match self {
            Self::Closed | Self::EnterEmail => crate::i18n::tr("Enter the email on your account. We'll send a link to reset your password.").to_owned(),
            Self::Sending => crate::i18n::tr("Sending…").to_owned(),
            Self::CheckEmail => crate::i18n::tr("Check your email. If an account uses this address, a reset link and your username are on the way.").to_owned(),
            Self::Failed(reason) => crate::i18n::format("Could not send the reset email: {error}", &[("error", reason.clone())]),
        }
    }
}

/// The hint to send to Pasion when `provider_id` names an upstream provider.
pub fn upstream_login_hint(provider_id: Option<&str>) -> Option<&str> {
    provider_id.filter(|id| id.starts_with(UPSTREAM_HINT_PREFIX))
}

/// GitHub is offered on the sign-in view of OAuth servers that advertise it.
pub fn github_button_visible(available: bool, oauth: bool, registration_open: bool) -> bool {
    available && oauth && !registration_open
}

/// Optional email: blank is fine, anything else must look like an address.
pub fn validate_optional_email(email: &str) -> Result<Option<String>, &'static str> {
    let email = email.trim();
    if email.is_empty() {
        Ok(None)
    } else if plausible_email(email) {
        Ok(Some(email.to_owned()))
    } else {
        Err("Enter a valid email address, or leave it empty.")
    }
}

#[cfg(unix)]
fn plausible_email(email: &str) -> bool { ::hagency::pasion::account::plausible_email(email) }
#[cfg(not(unix))]
fn plausible_email(email: &str) -> bool { email.contains('@') }

fn post(action: AccountOptionsAction) { makepad_widgets::Cx::post_action(action); }

/// Ask the server whether it advertises GitHub. A failure hides the button:
/// sign-in itself still works through the other methods.
pub fn spawn_github_check(server: String, generation: u64) {
    crate::sliding_sync::spawn_async_task(async move {
        #[cfg(unix)]
        let available = match ::hagency::pasion::account::upstream_providers(&server).await {
            Ok(providers) => ::hagency::pasion::account::offers_github(&providers),
            Err(error) => {
                // Hiding the optional button is the user-visible outcome.
                makepad_widgets::log!("GitHub provider check failed for {server}: {error}");
                false
            }
        };
        #[cfg(not(unix))]
        let available = { let _ = server; false };
        post(AccountOptionsAction::GithubAvailability { generation, available });
    });
}

/// Start "Forgot password?" for `email` on `server`.
pub fn spawn_recovery(request: u64, server: String, email: String) {
    crate::sliding_sync::spawn_async_task(async move {
        #[cfg(unix)]
        let result = ::hagency::pasion::account::start_recovery(&server, &email).await.map_err(|e| e.to_string());
        #[cfg(not(unix))]
        let result = { let _ = (server, email); Err(crate::i18n::tr("Reset your password in your browser.").to_owned()) };
        post(AccountOptionsAction::RecoveryRequested { request, result });
    });
}

/// Register with an email; Pasion may ask for the emailed code next.
pub fn spawn_email_registration(request: u64, pending: PendingRegistration, email: String) {
    let PendingRegistration { server, username, password, .. } = pending;
    crate::sliding_sync::spawn_async_task(async move {
        #[cfg(unix)]
        let result = ::hagency::pasion::account::register(&server, &username, &password, Some(&email)).await
            .map(email_step).map_err(|e| e.to_string());
        #[cfg(not(unix))]
        let result = { let _ = (server, username, password, email); Err(crate::i18n::tr("Create your account through Pasion on your Hagency server.").to_owned()) };
        post(AccountOptionsAction::EmailRegistration { request, result });
    });
}

/// Submit the emailed registration code.
pub fn spawn_email_code(request: u64, server: String, registration_id: String, username: String, code: String) {
    crate::sliding_sync::spawn_async_task(async move {
        #[cfg(unix)]
        let result = ::hagency::pasion::account::verify_registration_email(&server, &registration_id, &username, &code).await
            .map(email_step).map_err(|e| e.to_string());
        #[cfg(not(unix))]
        let result = { let _ = (server, registration_id, username, code); Err(String::new()) };
        post(AccountOptionsAction::EmailRegistration { request, result });
    });
}

#[cfg(unix)]
fn email_step(registration: ::hagency::pasion::account::Registration) -> EmailStep {
    use ::hagency::pasion::account::Registration;
    match registration {
        Registration::Existing | Registration::Created => EmailStep::Registered,
        Registration::NeedsEmailCode { registration_id } => EmailStep::NeedsCode { registration_id },
    }
}

#[cfg(test)]
#[path = "account_options_tests.rs"]
mod tests;
