//! Select custody from durable state (ADR-192), never socket liveness. On
//! macOS only the owner daemon executes: without its enrollment of the
//! account's registered root nothing runs, and setup is required.
use super::*;
use crate::hagency::hosting::{self, Execution};
use ::hagency::daemon::bridge::ControllerClient;

impl Service {
    pub(super) fn execution(&self) -> Result<Execution, String> {
        hosting::execution(&self.0.locations, &self.0.origin, &self.0.owner)
    }

    pub(super) fn local_management_only(&self) -> Result<bool, String> {
        match self.execution()? {
            Execution::InProcess => Ok(false),
            Execution::Setup(_) => Ok(true),
            Execution::Daemon { .. } => Err("owner_daemon_management_required".into()),
        }
    }

    /// The daemon home when the daemon hosts this owner.
    pub(super) fn daemon_home(&self) -> Result<Option<PathBuf>, String> {
        Ok(match self.execution()? {
            Execution::Daemon { home } => Some(home),
            Execution::InProcess | Execution::Setup(_) => None,
        })
    }

    /// [`Self::daemon_home`] for an account without a current service.
    pub(super) fn daemon_home_for(origin: &str, owner: &str) -> Result<Option<PathBuf>, String> {
        let locations = hosting::Locations::system()?;
        Ok(match hosting::execution(&locations, origin, owner)? {
            Execution::Daemon { home } => Some(home),
            Execution::InProcess | Execution::Setup(_) => None,
        })
    }

    /// Stop any in-process executor before attaching to the daemon. Repeated
    /// calls retain a failed drain. `Ok(None)` means this process serves the
    /// account: it executes only where no daemon ships; on macOS an account
    /// the daemon does not host gets a management-only owner (see
    /// `native_owner`), so permission and Agent requests keep working.
    pub(super) async fn remote_client(&self) -> Result<Option<Arc<ControllerClient>>, String> {
        self.check()?;
        let home = match self.execution()? {
            Execution::Daemon { home } => home,
            _ => {
                if self
                    .0
                    .remote
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_some()
                {
                    self.retire();
                    return Err("native_owner_restart_required".into());
                }
                return Ok(None);
            }
        };
        self.release_local_for_daemon().await?;
        self.check()?;
        if self.daemon_home()?.as_ref() != Some(&home) {
            return Err("daemon_owner_assignment_changed".into());
        }
        let mut slot = self.0.remote.lock().unwrap_or_else(|p| p.into_inner());
        Ok(Some(
            slot.get_or_insert_with(|| {
                ControllerClient::new(
                    home,
                    self.0.owner.clone(),
                    self.0.origin.clone(),
                    self.0.epoch + 1,
                )
            })
            .clone(),
        ))
    }

    /// Drain accepted activation before taking the native lock: its task needs
    /// that lock to finish. Keep both task and owner in their slots across a
    /// cancelled waiter or failed drain. Completed selections also own the
    /// canonical registry writer, even after the native owner has suspended.
    pub(super) async fn release_local_for_daemon(&self) -> Result<(), String> {
                let mut activation = self.0.activation.lock().await;
                if matches!(*activation, super::adoption::ActivationTask::Running(_)) {
            let _ = Self::await_activation(&mut activation).await;
        }
        let mut native = self.0.native.lock().await;
        let registered = self
            .0
            .native_retirement
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let owner = native.as_ref().cloned().or(registered);
        if let Some(owner) = &owner {
            owner.retire();
        }
                {
            self.0.allocations.retire();
            self.0.allocations.drain().await;
        }
        #[cfg(feature = "shared-execution")]
        {
            self.0.shared.retire();
            self.0.shared.drain().await;
        }
        if let Some(owner) = owner {
            self.close_native_owner(&owner).await?;
            native.take();
            self.0
                .native_retirement
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
        }
                {
            self.0.current_initialization.lock()
                .map_err(|_| "management_unavailable")?.take();
            *activation = super::adoption::ActivationTask::Empty;
            self.0.setup_pending.store(false, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    /// The signed-in account (server origin and owner) this service pins.
    pub fn account(&self) -> Result<hosting::Account, String> {
        self.check()?;
        Ok(hosting::Account {
            origin: self.0.origin.clone(),
            owner: self.0.owner.clone(),
        })
    }

    pub(super) fn watch_daemon_assignment(&self) {
        let weak = Arc::downgrade(&self.0);
        crate::sliding_sync::backend_runtime().spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let Some(inner) = weak.upgrade() else { break };
                let service = Service(inner);
                if service.check().is_err() {
                    break;
                }
                if service.daemon_home().ok().flatten().is_some() {
                    let _ = service.remote_client().await;
                }
            }
        });
    }
}
