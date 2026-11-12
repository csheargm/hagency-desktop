//! Logout State Machine Implementation
//!
//! This module implements a robust logout process using a state machine pattern to ensure
//! reliable session termination and proper resource cleanup.
//!
//! ## Design Rationale
//!
//! The logout process is complex and error-prone due to:
//! - Network operations that can fail or timeout
//! - Resource cleanup that must happen in specific order
//! - UI synchronization across desktop tabs
//! - Matrix SDK objects that can panic during destruction
//! - Need for progress feedback and cancellation support
//!
//! ## Critical Design Points
//!
//! ### Point of No Return
//! Desktop retirement becomes irreversible only after its authenticated revoke-only
//! tombstone is durable. Local cleanup may then complete while remote revocation is
//! pending. A cleanup error requires restart; it must never resume the retired SDK.
//! The compile-time mobile compatibility path retains its original server-logout
//! boundary and does not claim the native credential migration.
//!
//! ### Resource Management Strategy
//! - **During logout**: Resources are properly dropped to prevent memory leaks on re-login
//! - **On app exit**: Resources are intentionally leaked to avoid deadpool panic
//! - **Cleanup order**: Dependencies cleared before their dependencies (e.g., cache before runtime)
//!
//! ### Error Recovery Model
//! - **Before PointOfNoReturn**: A failed native commit is explicitly not durable;
//!   sync and sending remain stopped, and credentials are kept for retry.
//! - **After PointOfNoReturn**: Cleanup errors require restart; late cancellation
//!   cannot interrupt retirement. Remote pending is distinct from cleanup failure.
//! - The mobile compatibility path retains its original M_UNKNOWN_TOKEN behavior.
//!
//! ### Async Coordination
//! Uses Arc<Notify> for UI synchronization:
//! - `ClosingTabs`: Wait for desktop tabs to close (5s timeout)
//! - `CleaningAppState`: Wait for UI state cleanup (5s timeout)
//! - Bounded remote observation retains partial revocation receipts. Native prompts
//!   and original SDK IO remain owned until actual completion, even past shutdown.
//!
//! ### Progress Feedback
//! Each state maps to specific completion percentage (10%, 20%, 30%...100%)
//! enabling precise progress bar updates and user feedback.
//!
//! ## State Machine Execution Flow
//!
//! 1. **PreChecking**: Validate CLIENT, SYNC_SERVICE, and access_token existence
//! 2. **StoppingSyncService**: Stop sync service to prevent new data
//! 3. **LoggingOutFromServer**: Desktop persists revoke-only intent before bounded
//!    remote observation; mobile retains its original SDK logout compatibility path.
//! 4. **PointOfNoReturn**: Set global flags, delete saved user ID
//! 5. **ClosingTabs**: Close desktop tabs via `MainDesktopUiAction::CloseAllTabs`
//! 6. **CleaningAppState**: Drops globals and signals `LOGOUT_NOTIFY`. The
//!    login loop in `start_matrix_client_login_and_sync` does the actual
//!    teardown of per-session tasks. The tokio runtime stays alive, since
//!    tearing it down here would race with the new client's SQLite setup.
//! 7. **Completed**: Send `LogoutAction::LogoutSuccess`
//!
//! Progress updates are sent via `LogoutAction::ProgressUpdate` for UI feedback.
//! Errors are classified as `Recoverable` or `Unrecoverable` for appropriate handling.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use anyhow::{anyhow, Result};
use makepad_widgets::{Cx, log};

use crate::home::navigation_tab_bar::NavigationBarAction;
use crate::persistence::delete_latest_user_id;
use crate::sliding_sync::clear_app_state;
use crate::{
    home::main_desktop_ui::MainDesktopUiAction,
    sliding_sync::{get_client, get_sync_service},
};
use super::logout_confirm_modal::{LogoutAction, ClearedComponentType};
use super::logout_errors::{LogoutError, RecoverableError, UnrecoverableError};

/// Represents the current state of the logout process
#[derive(Debug, Clone, PartialEq)]
pub enum LogoutState {
    /// Initial state before logout starts
    Idle,
    /// Checking prerequisites (client, sync service existence)
    PreChecking,
    /// Stopping the sync service
    StoppingSyncService,
    /// Performing server-side logout
    LoggingOutFromServer,
    /// Reached point of no return - session invalidated
    PointOfNoReturn,
    /// Closing UI tabs (desktop only)
    ClosingTabs,
    /// Cleaning up application state.
    CleaningAppState,
    /// Logout completed successfully
    Completed,
    /// Logout failed with error
    Failed(LogoutError),
}

/// Progress information for logout operations
#[derive(Debug, Clone)]
pub struct LogoutProgress {
    pub state: LogoutState,
    pub message: String,
    pub percentage: u8,
    pub started_at: Instant,
    pub step_started_at: Instant,
}

impl LogoutProgress {
    fn new(state: LogoutState, message: String, percentage: u8) -> Self {
        let now = Instant::now();
        Self {
            state,
            message,
            percentage,
            started_at: now,
            step_started_at: now,
        }
    }

    fn update(&mut self, state: LogoutState, message: String, percentage: u8) {
        self.state = state;
        self.message = message;
        self.percentage = percentage;
        self.step_started_at = Instant::now();
    }
}

/// Configuration for logout process
#[derive(Debug, Clone)]
pub struct LogoutConfig {
    /// Timeout for closing tabs
    pub tab_close_timeout: Duration,
    /// Timeout for cleaning app state
    pub app_state_cleanup_timeout: Duration,
    /// Timeout for server logout
    pub server_logout_timeout: Duration,
    /// Whether to allow cancellation before point of no return
    pub allow_cancellation: bool,
    /// Whether this is desktop mode
    pub is_desktop: bool,
}

impl Default for LogoutConfig {
    fn default() -> Self {
        Self {
            tab_close_timeout: Duration::from_secs(5),
            app_state_cleanup_timeout: Duration::from_secs(5),
            server_logout_timeout: Duration::from_secs(60),
            allow_cancellation: true,
            is_desktop: true,
        }
    }
}

/// State machine for managing the logout process
pub struct LogoutStateMachine {
    current_state: Arc<Mutex<LogoutState>>,
    progress: Arc<Mutex<LogoutProgress>>,
    config: LogoutConfig,
    point_of_no_return: Arc<AtomicBool>,
    cancellation_requested: Arc<AtomicBool>,
    revocation_pending: AtomicBool,
}

impl LogoutStateMachine {
    pub fn new(config: LogoutConfig) -> Self {
        let initial_progress =
            LogoutProgress::new(LogoutState::Idle, "Ready to logout".to_string(), 0);

        Self {
            current_state: Arc::new(Mutex::new(LogoutState::Idle)),
            progress: Arc::new(Mutex::new(initial_progress)),
            config,
            point_of_no_return: Arc::new(AtomicBool::new(false)),
            revocation_pending: AtomicBool::new(false),
            cancellation_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get current state
    pub async fn current_state(&self) -> LogoutState {
        self.current_state.lock().await.clone()
    }

    /// Get current progress
    pub async fn progress(&self) -> LogoutProgress {
        self.progress.lock().await.clone()
    }

    /// Request cancellation (only works before point of no return)
    pub fn request_cancellation(&self) {
        if self.config.allow_cancellation && !self.point_of_no_return.load(Ordering::Acquire) {
            self.cancellation_requested.store(true, Ordering::Release);
        }
    }

    /// Check if cancellation was requested
    fn is_cancelled(&self) -> bool {
        self.cancellation_requested.load(Ordering::Acquire)
    }

    /// Transition to a new state
    async fn transition_to(
        &self,
        new_state: LogoutState,
        message: String,
        percentage: u8,
    ) -> Result<()> {
        // Check for cancellation before transitioning
        if self.is_cancelled()
            && !self.point_of_no_return.load(Ordering::Acquire)
            && !matches!(new_state, LogoutState::Failed(_))
        {
            let error = LogoutError::Recoverable(RecoverableError::Cancelled);
            *self.current_state.lock().await = LogoutState::Failed(error.clone());
            self.progress.lock().await.update(
                LogoutState::Failed(error.clone()),
                crate::i18n::tr("Logout cancelled by user").into(),
                0,
            );
            self.handle_error(&error).await;
            return Err(anyhow!(error));
        }

        log!(
            "Logout state transition: {:?} -> {:?}",
            self.current_state.lock().await.clone(),
            new_state
        );

        // Update state and progress, then extract values for UI update
        let mut state = self.current_state.lock().await;
        *state = new_state.clone();
        drop(state);

        let mut progress = self.progress.lock().await;
        progress.update(new_state, message.clone(), percentage);
        let progress_message = progress.message.clone();
        let progress_percentage = progress.percentage;
        drop(progress);

        // Send progress update to UI
        log!(
            "Sending progress update: {} ({}%)",
            progress_message,
            progress_percentage
        );
        Cx::post_action(LogoutAction::ProgressUpdate {
            epoch: crate::account_session::epoch(),
            message: progress_message,
            percentage: progress_percentage,
        });

        Ok(())
    }

    /// Execute the logout process
    pub async fn execute(&self) -> Result<()> {
        log!("LogoutStateMachine::execute() started");

        let logged_out_user =
            get_client().and_then(|client| client.user_id().map(ToOwned::to_owned));
        // Set logout in progress flag
        set_logout_in_progress(true);

        // Reset global point of no return flag
        set_logout_point_of_no_return(false);

        // Start from Idle state
        self.transition_to(
            LogoutState::PreChecking,
            "Checking prerequisites...".to_string(),
            10,
        )
        .await?;

        // Pre-checks
        if let Err(e) = self.perform_prechecks().await {
            self.transition_to(
                LogoutState::Failed(e.clone()),
                format!("Precheck failed: {}", e),
                0,
            )
            .await?;
            self.handle_error(&e).await;
            return Err(anyhow!(e));
        }

        // Stop sync service
        self.transition_to(
            LogoutState::StoppingSyncService,
            "Stopping sync service...".to_string(),
            20,
        )
        .await?;

        if let Err(e) = self.stop_sync_service().await {
            self.transition_to(
                LogoutState::Failed(e.clone()),
                format!("Failed to stop sync service: {}", e),
                0,
            )
            .await?;
            self.handle_error(&e).await;
            return Err(anyhow!(e));
        }

        // Server logout
        self.transition_to(
            LogoutState::LoggingOutFromServer,
            "Logging out from server...".to_string(),
            30,
        )
        .await?;

        match self.perform_server_logout().await {
            Ok(_) => {
                self.point_of_no_return.store(true, Ordering::Release);
                set_logout_point_of_no_return(true);
                self.transition_to(
                    LogoutState::PointOfNoReturn,
                    "Point of no return reached".to_string(),
                    50,
                )
                .await?;

                // We delete latest_user_id after reaching LOGOUT_POINT_OF_NO_RETURN:
                // 1. To prevent auto-login with invalid session on next start
                // 2. While keeping session file intact for potential future login
                if let Err(e) = delete_latest_user_id().await {
                    log!("Warning: Failed to delete latest user ID: {}", e);
                }
            }
            Err(e) => {
                // Check if it's an M_UNKNOWN_TOKEN error
                if matches!(&e, LogoutError::Recoverable(RecoverableError::ServerLogoutFailed(msg)) if msg.contains("M_UNKNOWN_TOKEN"))
                {
                    log!("Token already invalidated, continuing with logout");
                    self.point_of_no_return.store(true, Ordering::Release);
                    set_logout_point_of_no_return(true);
                    self.transition_to(
                        LogoutState::PointOfNoReturn,
                        "Token already invalidated".to_string(),
                        50,
                    )
                    .await?;

                    // Same delete operation as in the success case above
                    if let Err(e) = delete_latest_user_id().await {
                        log!("Warning: Failed to delete latest user ID: {}", e);
                    }
                } else {
                    // Restart sync service since we haven't reached point of no return
                    #[cfg(any(target_os = "ios", target_os = "android"))]
                    if let Some(sync_service) = get_sync_service() {
                        sync_service.start().await;
                    }

                    self.transition_to(
                        LogoutState::Failed(e.clone()),
                        format!("Server logout failed: {}", e),
                        0,
                    )
                    .await?;
                    self.handle_error(&e).await;
                    return Err(anyhow!(e));
                }
            }
        }

        // From here on, all failures are unrecoverable

        // Close tabs (desktop only)
        if self.config.is_desktop {
            self.transition_to(
                LogoutState::ClosingTabs,
                "Closing all tabs...".to_string(),
                60,
            )
            .await?;

            if let Err(e) = self.close_all_tabs().await {
                let error = LogoutError::Unrecoverable(
                    UnrecoverableError::PostPointOfNoReturnFailure(e.to_string()),
                );
                self.transition_to(
                    LogoutState::Failed(error.clone()),
                    "Failed to close tabs".to_string(),
                    0,
                )
                .await?;
                self.handle_error(&error).await;
                return Err(anyhow!(error));
            }
        }

        // Stop token-refresh callbacks before deleting just this identity's
        // token file. Keep its data; other accounts are untouched.
        crate::account_session::close().await;
        self.finish_account_cleanup(async {
            if let Some(user) = logged_out_user {
                crate::accounts::remove_session(crate::app_data_dir(), &user).await?;
                crate::accounts::refresh()?;
            }
            Ok(())
        })
        .await?;
        // Clean app state
        self.transition_to(
            LogoutState::CleaningAppState,
            "Cleaning up application state...".to_string(),
            70,
        )
        .await?;

        // All static resources (CLIENT, SYNC_SERVICE, etc.) are defined in the sliding_sync module,
        // so the state machine delegates the cleanup operation to sliding_sync's clear_app_state function
        // rather than accessing these static variables directly from outside the module.
        if let Err(e) = clear_app_state(&self.config).await {
            let error = LogoutError::Unrecoverable(UnrecoverableError::PostPointOfNoReturnFailure(
                e.to_string(),
            ));
            self.transition_to(
                LogoutState::Failed(error.clone()),
                "Failed to clean app state".to_string(),
                0,
            )
            .await?;
            self.handle_error(&error).await;
            return Err(anyhow!(error));
        }

        self.transition_to(
            LogoutState::Completed,
            if self.revocation_pending.load(Ordering::Acquire) {
                "Signed out locally. Remote token revocation remains pending.".into()
            } else {
                crate::i18n::tr("Signed out locally.").into()
            },
            100,
        )
        .await?;

        // Close the settings screen after logout, since its content
        // is specific to the currently-logged-in user's account.
        Cx::post_action(NavigationBarAction::CloseSettings);

        self.publish_success_with(|| {});
        Ok(())
    }

    fn publish_success_with(&self, after_release: impl FnOnce()) {
        // Freeze the completed operation's scope before accepting another
        // account change. The sender may be descheduled immediately afterwards.
        let epoch = crate::account_session::epoch();
        set_logout_in_progress(false);
        after_release();
        Cx::post_action(LogoutAction::LogoutSuccess {
            revocation_pending: self.revocation_pending.load(Ordering::Acquire),
            epoch,
        });
    }

    /// Every real local cleanup error reaches the UI as a failed, retired
    /// operation. Never expose filesystem/native diagnostics or resume sync.
    async fn finish_account_cleanup(
        &self,
        cleanup: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        if cleanup.await.is_err() {
            let error = LogoutError::Unrecoverable(
                UnrecoverableError::PostPointOfNoReturnFailure(
                    crate::i18n::tr("Local sign-out is durable. Local session cleanup failed; restart before signing in again. Any pending remote revocation will be retried.").into(),
                ),
            );
            self.transition_to(
                LogoutState::Failed(error.clone()),
                crate::i18n::tr("Local session cleanup remains incomplete").into(),
                0,
            )
            .await?;
            self.handle_error(&error).await;
            return Err(anyhow!(error));
        }
        Ok(())
    }

    // Individual step implementations
    async fn perform_prechecks(&self) -> Result<(), LogoutError> {
        log!("perform_prechecks started");

        // Check client existence
        if get_client().is_none() {
            log!("perform_prechecks: client cleared");
            return Err(LogoutError::Unrecoverable(
                UnrecoverableError::ComponentsCleared,
            ));
        }

        // Check sync service
        if get_sync_service().is_none() {
            log!("perform_prechecks: sync service cleared");
            return Err(LogoutError::Unrecoverable(
                UnrecoverableError::ComponentsCleared,
            ));
        }
        log!("perform_prechecks: sync service exists");

        // Check access token
        if let Some(client) = get_client() {
            if client.access_token().is_none() {
                log!("perform_prechecks: no access token");
                return Err(LogoutError::Recoverable(RecoverableError::NoAccessToken));
            }
            log!("perform_prechecks: access token exists");
        }

        log!("perform_prechecks completed successfully");
        Ok(())
    }

    async fn stop_sync_service(&self) -> Result<(), LogoutError> {
        if let Some(sync_service) = get_sync_service() {
            sync_service.stop().await;
            Ok(())
        } else {
            Err(LogoutError::Unrecoverable(
                UnrecoverableError::ComponentsCleared,
            ))
        }
    }

    async fn perform_server_logout(&self) -> Result<(), LogoutError> {
        crate::hagency::backend::Service::stop_current().await
            .map_err(|code| LogoutError::Recoverable(RecoverableError::ServerLogoutFailed(code)))?;
        let Some(client) = get_client() else {
            return Err(LogoutError::Unrecoverable(
                UnrecoverableError::ComponentsCleared,
            ));
        };

        if self.is_cancelled() && !self.point_of_no_return.load(Ordering::Acquire) {
            return Err(LogoutError::Recoverable(RecoverableError::Cancelled));
        }

        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            client.send_queue().set_enabled(false).await;
            let pending = crate::persistence::credential_state::begin_logout(&client).await
                .map_err(|_| LogoutError::Unrecoverable(UnrecoverableError::PostPointOfNoReturnFailure(
                    "Local sign-out is not durably confirmed. Sync and sending remain stopped; keep this account's local data and retry after credential storage is available.".into()
                )))?;
            // The revoke-only tombstone, including the actual pair, is durable.
            // Later outcomes are remote pending, never a reason to resume sync.
            self.point_of_no_return.store(true, Ordering::Release);
            set_logout_point_of_no_return(true);
            match crate::persistence::credential_state::retry_revocation(pending).await {
                Ok(true) => (),
                Ok(false) | Err(_) => {
                    self.revocation_pending.store(true, Ordering::Release);
                    log!("Local sign-out is durable; remote token revocation remains pending");
                    Cx::post_action(LogoutAction::ProgressUpdate {
                        epoch: crate::account_session::epoch(),
                        message: "Signed out locally. Remote token revocation is pending and will be retried.".into(),
                        percentage: 45,
                    });
                }
            }
            Ok(())
        }
        #[cfg(any(target_os = "ios", target_os = "android"))]
        match tokio::time::timeout(
            self.config.server_logout_timeout,
            crate::login::homeserver::logout_sdk(&client),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(LogoutError::Recoverable(
                RecoverableError::ServerLogoutFailed(e.to_string()),
            )),
            Err(_) => Err(LogoutError::Recoverable(RecoverableError::Timeout(
                "Server logout timed out".to_string(),
            ))),
        }
    }

    async fn close_all_tabs(&self) -> Result<()> {
        let on_close_all = Arc::new(Notify::new());
        Cx::post_action(MainDesktopUiAction::CloseAllTabs {
            on_close_all: on_close_all.clone(),
        });

        match tokio::time::timeout(self.config.tab_close_timeout, on_close_all.notified()).await {
            Ok(_) => {
                log!("Received signal that all tabs were closed successfully");
                Ok(())
            }
            Err(_) => Err(anyhow!("Timed out waiting for tabs to close")),
        }
    }

    /// Handle errors by posting appropriate actions
    async fn handle_error(&self, error: &LogoutError) {
        let epoch = crate::account_session::epoch();
        // Reset logout in progress flag on error (unless we've reached point of no return)
        if !is_logout_past_point_of_no_return() {
            set_logout_in_progress(false);
        }

        match error {
            LogoutError::Unrecoverable(UnrecoverableError::ComponentsCleared) => {
                Cx::post_action(LogoutAction::ApplicationRequiresRestart {
                    cleared_component: ClearedComponentType::Client,
                    epoch,
                });
            }
            LogoutError::Recoverable(RecoverableError::Cancelled) => {
                log!("Logout cancelled by user");
                Cx::post_action(LogoutAction::LogoutFailure {
                    message: crate::i18n::tr("Logout cancelled by user").into(),
                    epoch,
                });
            }
            _ => {
                Cx::post_action(LogoutAction::LogoutFailure {
                    message: error.to_string(),
                    epoch,
                });
            }
        }
    }
}

/// Global atomic flag indicating if the logout process has reached the "point of no return"
/// where aborting the logout operation is no longer safe.
static LOGOUT_POINT_OF_NO_RETURN: AtomicBool = AtomicBool::new(false);

/// Global atomic flag indicating if logout is in progress
static LOGOUT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn is_logout_past_point_of_no_return() -> bool {
    LOGOUT_POINT_OF_NO_RETURN.load(Ordering::Relaxed)
}

pub fn is_logout_in_progress() -> bool {
    LOGOUT_IN_PROGRESS.load(Ordering::Acquire)
}

fn set_logout_point_of_no_return(value: bool) {
    LOGOUT_POINT_OF_NO_RETURN.store(value, Ordering::Relaxed);
}

fn set_logout_in_progress(value: bool) {
    // Block replacement services before synchronously retiring native authority.
    let prev = LOGOUT_IN_PROGRESS.swap(value, Ordering::AcqRel);
    if value && crate::hagency::backend::Service::revoke_current().is_err() {
        // perform_server_logout retries the durable operation and exposes its
        // checked failure before any SDK credentials or UI state are removed.
        makepad_widgets::error!("Native owner logout persistence requires retry");
    }
    if value { crate::article_app::invalidate_sessions(); crate::assistant::invalidate(); }
    if prev != value {
        // Emit the action here (only when the value has changed)
        Cx::post_action(LogoutAction::InProgress(value));
    }
}

/// Execute logout using the state machine
pub async fn logout_with_state_machine(is_desktop: bool) -> Result<()> {
    log!(
        "logout_with_state_machine called with is_desktop: {}",
        is_desktop
    );

    let config = LogoutConfig {
        is_desktop,
        ..Default::default()
    };

    let state_machine = LogoutStateMachine::new(config);
    let result = state_machine.execute().await;

    log!(
        "logout_with_state_machine finished with result: {:?}",
        result.is_ok()
    );
    result
}

#[cfg(all(test, not(any(target_os = "ios", target_os = "android"))))]
mod tests;
