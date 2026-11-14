//! Login-screen wiring for the account options in `super::super::account_options`:
//! "Continue with GitHub", "Forgot password?" and registration with an
//! optional email plus the emailed code.

use makepad_widgets::*;

use crate::login::account_options::{self as options, AccountOptionsAction, EmailStep, PendingRegistration, RecoveryState};
use crate::sliding_sync::{submit_async_request, MatrixRequest};

use super::LoginScreen;

/// Per-server state of the account options.
#[derive(Default)]
pub struct AccountUi {
    github_available: bool,
    recovery: RecoveryState,
    /// The recovery call whose reply is awaited, if any.
    recovery_request: Option<u64>,
    /// The email-registration call whose reply is awaited, if any.
    email_request: Option<u64>,
    /// Server and credentials of the email registration in progress.
    registration: Option<PendingRegistration>,
}

impl LoginScreen {
    fn homeserver(&self) -> Option<String> { self.methods.as_ref().map(|m| m.homeserver.clone()) }

    pub(super) fn reset_account_options(&mut self, cx: &mut Cx) {
        self.account = AccountUi::default();
        self.view.button(cx, ids!(github_login_button)).set_visible(cx, false);
        self.view.view(cx, ids!(recovery_form)).set_visible(cx, false);
        self.view.text_input(cx, ids!(registration_email)).set_text(cx, "");
        self.view.text_input(cx, ids!(registration_code)).set_text(cx, "");
        self.view.view(cx, ids!(registration_code_row)).set_visible(cx, false);
    }

    /// Called whenever the method view is (re)shown.
    pub(super) fn refresh_account_options(&mut self, cx: &mut Cx) {
        let oauth = self.methods.as_ref().is_some_and(|m| m.oauth);
        let visible = options::github_button_visible(self.account.github_available, oauth, self.registration_form_open);
        self.view.button(cx, ids!(github_login_button)).set_visible(cx, visible);
        let password = self.methods.as_ref().is_some_and(|m| m.password || m.oauth);
        self.view.button(cx, ids!(forgot_password_button)).set_visible(cx, password);
        if oauth && !self.account.github_available {
            if let Some(server) = self.homeserver() {
                options::spawn_github_check(server, self.discovery_generation);
            }
        }
    }

    fn set_recovery(&mut self, cx: &mut Cx, state: RecoveryState) {
        self.view.view(cx, ids!(recovery_form)).set_visible(cx, state.is_open());
        self.view.label(cx, ids!(recovery_status)).set_text(cx, &state.status_text());
        self.view.button(cx, ids!(send_recovery_button)).set_enabled(cx, state.can_send());
        self.view.view(cx, ids!(password_form)).set_visible(cx, !state.is_open() && self.password_form_open);
        self.account.recovery = state;
        self.redraw(cx);
    }

    /// Returns true when the registration was taken over by the email flow.
    pub(super) fn submit_email_registration(&mut self, cx: &mut Cx, username: &str, password: &str) -> bool {
        let Some(server) = self.homeserver() else { return false };
        if let Some(pending) = self.account.registration.clone().filter(|p| p.registration_id.is_some()) {
            let code = self.view.text_input(cx, ids!(registration_code)).text().trim().to_owned();
            if code.is_empty() {
                self.show_status(cx, crate::i18n::tr("Check registration details"), crate::i18n::tr("Enter the code from the email."), crate::i18n::tr("Okay"), true);
            } else {
                let request = self.start_email_call(cx);
                let registration_id = pending.registration_id.unwrap_or_default();
                options::spawn_email_code(request, pending.server, registration_id, pending.username, code);
            }
            return true;
        }
        let email = self.view.text_input(cx, ids!(registration_email)).text();
        match options::validate_optional_email(&email) {
            Ok(None) => false,
            Err(message) => {
                self.show_status(cx, crate::i18n::tr("Check registration details"), crate::i18n::tr(message), crate::i18n::tr("Okay"), true);
                true
            }
            Ok(Some(_)) if username.is_empty() || password.is_empty() => false,
            Ok(Some(email)) => {
                let pending = PendingRegistration { server, username: username.to_owned(), password: password.to_owned(), registration_id: None };
                self.account.registration = Some(pending.clone());
                let request = self.start_email_call(cx);
                options::spawn_email_registration(request, pending, email);
                true
            }
        }
    }

    fn start_email_call(&mut self, cx: &mut Cx) -> u64 {
        let request = options::next_request_id();
        self.login_pending = true;
        self.account.email_request = Some(request);
        self.show_status(cx, crate::i18n::tr("Creating account"), crate::i18n::tr("Contacting your server…"), crate::i18n::tr("Please wait…"), false);
        request
    }

    pub(super) fn handle_account_options(&mut self, cx: &mut Cx, actions: &Actions) {
        if !self.login_pending && self.view.button(cx, ids!(github_login_button)).clicked(actions) {
            self.start_sso(cx, Some(options::GITHUB_LOGIN_HINT.to_owned()), false);
        }
        if !self.login_pending && self.view.button(cx, ids!(forgot_password_button)).clicked(actions) {
            let user = self.view.text_input(cx, ids!(user_id_input)).text();
            if user.contains('@') && !user.starts_with('@') {
                self.view.text_input(cx, ids!(recovery_email)).set_text(cx, user.trim());
            }
            self.set_recovery(cx, RecoveryState::EnterEmail);
        }
        if self.view.button(cx, ids!(close_recovery_button)).clicked(actions) {
            self.account.recovery_request = None;
            self.set_recovery(cx, RecoveryState::Closed);
        }
        if self.account.recovery.can_send() && self.view.button(cx, ids!(send_recovery_button)).clicked(actions) {
            self.send_recovery(cx);
        }
        for action in actions {
            match action.downcast_ref() {
                Some(AccountOptionsAction::GithubAvailability { generation, available }) if *generation == self.discovery_generation => {
                    self.account.github_available = *available;
                    let oauth = self.methods.as_ref().is_some_and(|m| m.oauth);
                    let visible = options::github_button_visible(*available, oauth, self.registration_form_open);
                    self.view.button(cx, ids!(github_login_button)).set_visible(cx, visible);
                    self.redraw(cx);
                }
                Some(AccountOptionsAction::RecoveryRequested { request, result }) if options::accepts_reply(self.account.recovery_request, *request) => {
                    self.account.recovery_request = None;
                    let state = match result { Ok(()) => RecoveryState::CheckEmail, Err(e) => RecoveryState::Failed(e.clone()) };
                    self.set_recovery(cx, state);
                }
                Some(AccountOptionsAction::EmailRegistration { request, result }) if options::accepts_reply(self.account.email_request, *request) => {
                    self.account.email_request = None;
                    self.finish_email_step(cx, result.clone());
                }
                _ => {}
            }
        }
        self.view.button(cx, ids!(github_login_button)).set_enabled(cx, !self.login_pending);
    }

    fn send_recovery(&mut self, cx: &mut Cx) {
        let email = self.view.text_input(cx, ids!(recovery_email)).text().trim().to_owned();
        match (options::validate_optional_email(&email), self.homeserver()) {
            (Ok(Some(email)), Some(server)) => {
                let request = options::next_request_id();
                self.account.recovery_request = Some(request);
                self.set_recovery(cx, RecoveryState::Sending);
                options::spawn_recovery(request, server, email);
            }
            _ => self.set_recovery(cx, RecoveryState::Failed(crate::i18n::tr("Enter a valid email address.").to_owned())),
        }
    }

    fn finish_email_step(&mut self, cx: &mut Cx, result: Result<EmailStep, String>) {
        self.login_pending = false;
        match result {
            Ok(EmailStep::NeedsCode { registration_id }) => {
                if let Some(pending) = self.account.registration.as_mut() {
                    pending.registration_id = Some(registration_id);
                }
                self.view.view(cx, ids!(registration_code_row)).set_visible(cx, true);
                self.view.text_input(cx, ids!(registration_code)).set_key_focus(cx);
                self.show_status(cx, crate::i18n::tr("Check your email"), crate::i18n::tr("Enter the code we emailed you, then press Create account again."), crate::i18n::tr("Okay"), true);
            }
            Ok(EmailStep::Registered) => {
                // Sign in with the exact values the account was created with;
                // taking them out clears the stored password.
                let Some(PendingRegistration { server: homeserver_url, username, password, .. }) = self.account.registration.take() else { return };
                self.login_pending = true;
                self.show_status(cx, crate::i18n::tr("Signing in"), crate::i18n::tr("Connecting to your account…"), crate::i18n::tr("Please wait…"), false);
                submit_async_request(MatrixRequest::PasswordSignIn { homeserver_url, username, password, token: String::new() });
            }
            Err(error) => {
                // A failed first step forgets the credentials so a retry uses
                // the edited form; a failed code keeps them for another try.
                if self.account.registration.as_ref().is_some_and(|p| p.registration_id.is_none()) {
                    self.account.registration = None;
                }
                self.show_status(cx, crate::i18n::tr("Could not create the account"), &error, crate::i18n::tr("Okay"), true);
            }
        }
    }
}
