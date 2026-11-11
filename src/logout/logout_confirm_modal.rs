use std::sync::Arc;

use makepad_widgets::*;
use tokio::sync::Notify;
use crate::home::home_screen::effective_is_desktop;
use crate::sliding_sync::{submit_async_request, MatrixRequest};
use super::logout_state_machine::is_logout_past_point_of_no_return;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    // A modal dialog that displays logout confirmation
    mod.widgets.LogoutConfirmModal = set_type_default() do #(LogoutConfirmModal::register_widget(vm)) {
        ..mod.widgets.SmallModal

        title := ModalTitle {
            text: #(crate::i18n::tr("Confirm Logout")) i18n_text: "Confirm Logout"
        }

        body := ModalBody {
            text: #(crate::i18n::tr("Are you sure you want to logout?")) i18n_text: "Are you sure you want to logout?"
        }

        buttons_view := ModalButtonsRow {
            cancel_button := RobrixNeutralIconButton {
                width: Fit{min: FitBound.Abs(120.0)},
                align: Align{x: 0.5, y: 0.5}
                padding: Inset{left: 12 right: 12 top: 0 bottom: 0},
                draw_icon.svg: (ICON_FORBIDDEN)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"
            }

            confirm_button := RobrixNegativeIconButton {
                width: Fit{min: FitBound.Abs(120.0)}
                align: Align{x: 0.5, y: 0.5}
                padding: Inset{left: 12 right: 12 top: 0 bottom: 0},
                draw_icon.svg: (ICON_LOGOUT)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Log out now")) i18n_text: "Log out now"
            }
        }
    }
}

/// A modal dialog that displays logout confirmation.
#[derive(Script, ScriptHook, Widget)]
pub struct LogoutConfirmModal {
    #[deref] view: View,
    /// Whether the modal is in a final state, meaning the user can only click "Okay" to close it.
    ///
    /// * Set to `Some(true)` after a successful logout Action
    /// * Set to `Some(false)` after a logout error occurs.
    /// * Set to `None` when the user is still able to interact with the modal.
    #[rust] final_success: Option<bool>,
    #[rust] running: bool,
    #[rust] running_epoch: Option<u64>,
}

/// Actions handled by the parent widget of the [`LogoutConfirmModal`].
#[derive(Clone, Debug, Default)]
pub enum LogoutConfirmModalAction {
    /// The modal should be opened
    Open,
    /// The modal requested its parent widget to close.
    Close {
        /// `True` if the modal was closed after a successful logout action.
        /// `False` if the modal was dismissed or closed after a failure/error.
        successful: bool,
        /// Whether the modal was dismissed by the user clicking an internal button.
        was_internal: bool,
    },
    #[default]
    None,
}

/// Actions related to logout process 
pub enum LogoutAction {
    /// Local sign-out completed; remote revocation can still be pending.
    LogoutSuccess { revocation_pending: bool, epoch: u64 },
    /// A failed local/remote logout step in its original account scope.
    LogoutFailure { message: String, epoch: u64 },
    /// A request from the background task to the main UI thread to clear all app state.
    ClearAppState {
        on_clear_appstate: Arc<Notify>,
    },
    /// Signal that the application is in an invalid state and needs to be restarted.
    /// This happens when critical components have been cleaned up during a previous
    /// logout attempt that reached the point of no return, but the app wasn't restarted.
    ApplicationRequiresRestart {
        /// Indicates which critical component was cleared
        cleared_component: ClearedComponentType,
        epoch: u64,
    },
    /// Progress update from the logout state machine
    ProgressUpdate {
        epoch: u64,
        message: String,
        percentage: u8,
    },
    /// Indicates logout is in progress or not
    InProgress(bool),
}

impl std::fmt::Debug for LogoutAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogoutAction::LogoutSuccess { revocation_pending, epoch } => write!(f, "LogoutSuccess(remote_pending={revocation_pending}, epoch={epoch})"),
            LogoutAction::LogoutFailure { message, epoch } => write!(f, "LogoutFailure({message}, epoch={epoch})"),
            LogoutAction::ClearAppState { .. } => write!(f, "ClearAppState"),
            LogoutAction::ApplicationRequiresRestart { cleared_component, .. } => {
                write!(f, "ApplicationRequiresRestart({:?})", cleared_component)
            }
            LogoutAction::ProgressUpdate { message, percentage, .. } => {
                write!(f, "ProgressUpdate({}, {}%)", message, percentage)
            }
            LogoutAction::InProgress(value) => write!(f, "InProgress({})", value),
        }
    }
}

/// Indicates which critical component was cleared during a failed logout attempt
/// that reached the point of no return, requiring application restart.
#[derive(Clone, Copy, Debug, Default)]
pub enum ClearedComponentType {
    /// The Matrix client was cleared during logout
    Client,
    /// The sync service was cleared during logout
    SyncService,
    #[default]
    None,
}

impl Widget for LogoutConfirmModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WidgetMatchEvent for LogoutConfirmModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        let cancel_button = self.button(cx, ids!(cancel_button));
        let mut confirm_button = self.button(cx, ids!(confirm_button));

        let modal_dismissed = actions.iter().any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)));
        let cancel_clicked = cancel_button.clicked(actions);

        if !self.running && (cancel_clicked || modal_dismissed) {
            cx.action(LogoutConfirmModalAction::Close { successful: false, was_internal: cancel_clicked });
            self.reset_state(cx);
            return;
        }

        let mut needs_redraw = false;
        if !self.running && confirm_button.clicked(actions) {
            if let Some(successful) = self.final_success {
                if is_logout_past_point_of_no_return() && !successful {
                    log!("User requested immediate restart after unrecoverable logout error");
                    cx.request_quit(QuitReason::App);
                }

                cx.action(LogoutConfirmModalAction::Close { successful, was_internal: true });
                self.reset_state(cx);
                return;
            } else {
                self.set_message(cx, "Waiting for logout...");
                confirm_button.set_enabled(cx, false);

                // No cancellation capability is wired to this modal. Keep the
                // operation visible rather than pretending closing can abort it.
                self.running = true;
                self.running_epoch = Some(crate::account_session::epoch());
                cancel_button.set_enabled(cx, false);

                submit_async_request(MatrixRequest::Logout { is_desktop: effective_is_desktop(cx) });
                needs_redraw = true;
            }
        }

        for action in actions {
            match action.downcast_ref() {
                Some(LogoutAction::LogoutSuccess { revocation_pending, epoch }) => {
                    if *epoch != crate::account_session::epoch() { continue; }
                    self.running = false;
                    self.running_epoch = None;
                    self.final_success = Some(true);
                    self.set_message(cx, crate::i18n::tr(if *revocation_pending {
                        "Signed out locally. Remote token revocation remains pending."
                    } else {
                        "Signed out locally."
                    }));
                    confirm_button.set_text(cx, crate::i18n::tr("Okay"));
                    confirm_button.set_enabled(cx, true);
                    cancel_button.set_visible(cx, false);

                    needs_redraw = true;
                }

                Some(LogoutAction::LogoutFailure { message: error, epoch }) => {
                    if *epoch != crate::account_session::epoch() { continue; }
                    self.running = false;
                    self.running_epoch = None;
                    if is_logout_past_point_of_no_return() {
                        self.label(cx, ids!(title)).set_text(cx, crate::i18n::tr("Logout error, please restart Rinx."));
                        self.set_message(cx, &crate::i18n::format("Logout failed: {0}", &[("0", error.clone())]));

                        confirm_button.set_text(cx, crate::i18n::tr("Restart now"));
                        script_apply_eval!(cx, confirm_button, {
                            draw_bg +: {
                                color: mod.widgets.COLOR_FG_DANGER_RED
                            }
                        });
                        confirm_button.set_enabled(cx, true);

                        cancel_button.set_visible(cx, false);

                    } else {
                        self.set_message(cx, &crate::i18n::format("Logout failed: {0}", &[("0", (error).to_string())]));
                        confirm_button.set_text(cx, crate::i18n::tr("Okay"));
                        confirm_button.set_enabled(cx, true);
                        cancel_button.set_visible(cx, false);
                    }

                    self.final_success = Some(false);
                    needs_redraw = true;
                }

                Some(LogoutAction::ApplicationRequiresRestart { epoch, .. }) => {
                    if *epoch != crate::account_session::epoch() { continue; }
                    self.running = false;
                    self.running_epoch = None;
                    self.label(cx, ids!(title)).set_text(cx, crate::i18n::tr("Logout error, please restart Rinx."));
                    self.set_message(cx, "Application is in an inconsistent state and needs to be restarted to continue.");

                    confirm_button.set_text(cx, crate::i18n::tr("Restart now"));
                    script_apply_eval!(cx, confirm_button, {
                        draw_bg +: {
                            color: mod.widgets.COLOR_FG_DANGER_RED
                        }
                    });
                    confirm_button.set_enabled(cx, true);
                    cancel_button.set_visible(cx, false);

                    self.final_success = Some(false);
                    needs_redraw = true;
                }

                Some(LogoutAction::ProgressUpdate { message, percentage, epoch }) => {
                    if *epoch != crate::account_session::epoch() { continue; }
                    self.running_epoch.get_or_insert(*epoch);
                    // Just update the message text to show progress
                    self.set_message(cx, &format!("{} ({}%)", message, percentage));
                    self.running = true;
                    confirm_button.set_enabled(cx, false);
                    cancel_button.set_enabled(cx, false);
                    needs_redraw = true;
                }

                _ => {} // Handle other actions or None
            }
        }

        if needs_redraw {
            self.redraw(cx);
        }

    }
}

impl LogoutConfirmModal {
    #[cfg(all(test, not(any(target_os = "ios", target_os = "android"))))]
    pub(super) fn test_view(&self) -> &View { &self.view }
    #[cfg(all(test, not(any(target_os = "ios", target_os = "android"))))]
    pub(super) fn test_reopen(&mut self, cx: &mut Cx) { self.reset_state(cx); }

    /// Sets the message text displayed in the body of the modal.
    pub fn set_message(&mut self, cx: &mut Cx, message: &str) {
        self.label(cx, ids!(body)).set_text(cx, message);
    }

    fn reset_state(&mut self, cx: &mut Cx) {
        if self.running && (super::logout_state_machine::is_logout_in_progress()
            || self.running_epoch == Some(crate::account_session::epoch())) { return; }
        let cancel_button = self.button(cx, ids!(cancel_button));
        let confirm_button = self.button(cx, ids!(confirm_button));
        self.final_success = None;
        self.running = false;
        self.running_epoch = None;
        self.set_message(cx, crate::i18n::tr("Are you sure you want to logout?"));
        confirm_button.set_enabled(cx, true);
        confirm_button.set_text(cx, crate::i18n::tr("Log out now"));
        cancel_button.set_visible(cx, true);
        cancel_button.set_enabled(cx, true);
        cancel_button.set_text(cx, crate::i18n::tr("Cancel"));
        cancel_button.reset_hover(cx);
        confirm_button.reset_hover(cx);
        self.redraw(cx);
    }

}

impl LogoutConfirmModalRef {
    /// See [`LogoutConfirmModal::set_message()`].
    pub fn set_message(&self, cx: &mut Cx, message: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_message(cx, message);
        }
    }

    pub fn reset_state(&self,cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.reset_state(cx);
        }
    }

}
