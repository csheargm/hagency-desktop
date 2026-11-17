//! Ordinary closure preserves the original native enrollment; explicit logout
//! durably revokes it before any asynchronous drain or SDK state removal.
use super::*;
use std::sync::atomic::Ordering;

impl Service {
    /// No new owner may be published while the process drains its last owner.
    /// Unlike logout, this fence intentionally has no persistent credentials effect.
    pub fn begin_process_shutdown() {
        PROCESS_CLOSING.store(true, Ordering::Release);
        Self::retire_current();
    }
    pub(super) fn revoke_resumption(&self) -> Result<(), String> {
        self.0.logout_requested.store(true, Ordering::Release);
        self.retire();
        if let Some(home)=self.daemon_home()? {
            return ::hagency::daemon::revocation::request(&home,&self.0.origin,&self.0.owner).map_err(str::to_owned);
        }
        if self.0.remote.lock().unwrap_or_else(|p|p.into_inner()).is_some() {return Ok(());}
        let registered = self
            .0
            .native_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(owner) = registered.as_ref() {
            owner.retire_and_revoke_resumption().map_err(|e| e.code)?;
        } else {
            NativeOwner::revoke_saved_matrix_resumption(self.root()?, &self.0.origin, &self.0.owner)
                .map_err(|e| e.code)?;
        }
        Ok(())
    }

    /// Runs one command on the in-process owner. A demo server restart revokes
    /// the owner's resume grant, which left the cached owner retired; every
    /// later command failed until the window restarted. On such a failure the
    /// stale owner is dropped so the next command authorizes a fresh one. Only
    /// reads are retried at once: the owner can report these codes after a
    /// mutation was already sent, and a mutation must never run twice.
    pub(super) async fn execute_native(&self, command: Command) -> Result<Value, String> {
        let owner = self.native_owner().await?;
        let read = super::management_read(&command);
        let retry = read.then(|| command.clone());
        match owner.execute(command).await.map_err(|e| e.code) {
            Err(code) if stale_owner_code(&code) => {
                self.drop_stale_native_owner(&owner).await?;
                match retry {
                    Some(command) => self.native_owner().await?.execute(command).await.map_err(|e| e.code),
                    None => Err(code),
                }
            }
            other => other,
        }
    }

    /// Drops `failed` only if it is still the cached owner: a concurrent
    /// caller may already have replaced it, and that owner must keep running.
    async fn drop_stale_native_owner(&self, failed: &NativeOwner) -> Result<(), String> {
        let mut native = self.0.native.lock().await;
        self.check()?;
        if native.as_ref().is_some_and(|current| !current.same_instance(failed)) {
            return Ok(());
        }
        failed.retire();
        self.close_native_owner(failed).await?;
        native.take();
        let mut registered = self.0.native_retirement.lock().unwrap_or_else(|p| p.into_inner());
        if registered.as_ref().is_some_and(|owner| owner.same_instance(failed)) {
            registered.take();
        }
        Ok(())
    }

    pub fn revoke_current() -> Result<(), String> {
        let service = CURRENT
            .lock()
            .map_err(|_| "native_owner_unavailable")?
            .clone();
        if let Some(service) = service {
            service.revoke_resumption()?;
        } else if let Some(client) = crate::sliding_sync::get_client() {
            let owner = client.user_id().ok_or("matrix_sign_in_required")?;
            let origin = client.homeserver().to_string();
            if let Some(home)=Self::daemon_home_for(&origin,owner.as_str())? {
                ::hagency::daemon::revocation::request(&home,&origin,owner.as_str()).map_err(str::to_owned)?;
            } else {
                let locations = crate::hagency::hosting::Locations::system()?;
                let state = Self::state_for(&locations, &origin, owner.as_str())?;
                NativeOwner::revoke_saved_matrix_resumption(&state, &origin, owner.as_str()).map_err(|e| e.code)?;
            }
        }
        Ok(())
    }

    pub(super) async fn close_native_owner(&self, owner: &NativeOwner) -> Result<(), String> {
        if self.0.logout_requested.load(Ordering::Acquire) {
            owner.shutdown_checked().await.map_err(|e| e.code)
        } else {
            owner.suspend().await.map_err(|e| e.code)?;
            // A logout that raced with suspension owns the final disposition.
            if self.0.logout_requested.load(Ordering::Acquire) {
                owner.shutdown_checked().await.map_err(|e| e.code)?;
            }
            Ok(())
        }
    }

    async fn finish_close(&self) -> Result<(), String> {
        self.retire();
        if self.0.logout_requested.load(Ordering::Acquire) {
            if let Some(home)=self.daemon_home()? {
                let remote=::hagency::daemon::bridge::ControllerClient::new(home,self.0.owner.clone(),self.0.origin.clone(),self.0.epoch+1);
                // The client persists the owner-scoped revocation fence before
                // socket IO. Offline delivery may finish later; a persistence
                // or scope error still prevents unsafe local sign-out.
                remote.sign_out().await.map_err(|e|e.code)?;
                remote.retire();
            }
        }
        #[cfg(feature = "shared-execution")]
        self.0.shared.drain().await;
                self.0.allocations.drain().await;
                self.0.host_watchdog.drain().await;
        // Keep both handles until the original drain succeeds. Cancellation or
        // an unknown process outcome must not turn the next call into success.
        let mut native = self.0.native.lock().await;
        let registered = self
            .0
            .native_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(owner) = native.as_ref().cloned().or(registered) {
            self.close_native_owner(&owner).await?;
        }
        native.take();
        self.0
            .native_retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        Ok(())
    }

    pub(super) async fn suspend(&self) -> Result<(), String> {
        self.finish_close().await
    }

    pub(super) async fn shutdown_checked(&self) -> Result<(), String> {
        let durable = self.revoke_resumption();
        let cleanup = self.finish_close().await;
        durable.and(cleanup)
    }

    // Existing failure paths retain their error while keeping a failed drain
    // attached. User-initiated logout uses stop_current's checked result.
    pub(super) async fn shutdown(&self) {
        if self.shutdown_checked().await.is_err() {
            makepad_widgets::warning!("Native owner logout requires recovery");
        }
    }

    async fn close_current(logout: bool) -> Result<(), String> {
        let service = CURRENT
            .lock()
            .map_err(|_| "native_owner_unavailable")?
            .clone();
        if let Some(service) = service {
            if logout {
                service.shutdown_checked().await?;
            } else {
                service.suspend().await?;
            }
            let mut current = CURRENT.lock().map_err(|_| "native_owner_unavailable")?;
            if current
                .as_ref()
                .is_some_and(|other| Arc::ptr_eq(&service.0, &other.0))
            {
                current.take();
            }
        } else if logout {
            Self::revoke_current()?;
            if let Some(client)=crate::sliding_sync::get_client() {
                let owner=client.user_id().ok_or("matrix_sign_in_required")?.to_string();
                let origin=client.homeserver().to_string();
                if let Some(home)=Self::daemon_home_for(&origin,&owner)? {
                    let remote=::hagency::daemon::bridge::ControllerClient::new(home,owner,origin,crate::account_session::epoch()+1);
                    // The client persists the owner-scoped revocation fence before
                // socket IO. Offline delivery may finish later; a persistence
                // or scope error still prevents unsafe local sign-out.
                remote.sign_out().await.map_err(|e|e.code)?;remote.retire();
                }
            }
        }
        Ok(())
    }

    pub async fn suspend_current() -> Result<(), String> {
        Self::close_current(false).await
    }
    pub async fn stop_current() -> Result<(), String> {
        Self::close_current(true).await
    }
}

/// Owner-authorization failures that a fresh owner can clear: the server
/// revoked or lost this window's resume grant (a restart), or the cached
/// device grant names another account. A real account switch is caught
/// earlier by `verify_matrix`, which retires the whole service instead.
pub(super) fn stale_owner_code(code: &str) -> bool {
    matches!(code, "native_resume_revoked" | "native_resume_missing" | "matrix_account_mismatch")
}

#[cfg(test)]
mod stale_owner_tests {
    use super::stale_owner_code;

    #[test]
    fn only_owner_authorization_failures_reopen_the_owner() {
        for code in ["native_resume_revoked", "native_resume_missing", "matrix_account_mismatch"] {
            assert!(stale_owner_code(code), "{code}");
        }
        for code in ["server_unavailable", "forbidden", "matrix_account_changed", "agent_command_recovery_required"] {
            assert!(!stale_owner_code(code), "{code}");
        }
    }
}
