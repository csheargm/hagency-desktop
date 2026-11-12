//! Finite native package authority without a database or resource handle.
use super::*;
use std::sync::Weak;

/// A trusted host supplies the authenticated context and reviewed permissions.
/// This is deliberately not Deserialize and contains no storage authority.
#[derive(Clone)]
pub struct ManagementAuthority {
    pub(super) scope: Arc<Scope>,
}
/// Revocation observation only. This cannot keep a scope alive or recover a
/// session, permissions, storage handle, or a new finite authority.
#[derive(Clone)]
pub struct WeakManagementAuthority {
    scope: Weak<Scope>,
}
impl WeakManagementAuthority {
    pub fn is_live(&self) -> bool {
        self.scope
            .upgrade()
            .is_some_and(|scope| ManagementAuthority { scope }.is_live())
    }
    pub fn retire(&self) {
        if let Some(scope) = self.scope.upgrade() {
            ManagementAuthority { scope }.retire();
        }
    }
}
impl ManagementAuthority {
    pub fn downgrade(&self) -> WeakManagementAuthority {
        WeakManagementAuthority {
            scope: Arc::downgrade(&self.scope),
        }
    }
    pub fn new(
        context: HostContext,
        permissions: impl IntoIterator<Item = Permission>,
        expires: Instant,
    ) -> Result<Self, Error> {
        let now = Instant::now();
        if expires <= now || expires.duration_since(now) > MAX_SESSION {
            return Err(Error::LocalAuthority);
        }
        let fence = ResourcePublicationRetirement::default();
        Ok(Self {
            scope: Arc::new(Scope {
                context,
                permissions: permissions.into_iter().collect(),
                expires,
                retired: AtomicBool::new(false),
                commands: tokio::sync::Mutex::new(()),
                pending: tokio::sync::Semaphore::new(8),
                fence,
            }),
        })
    }
    pub fn retire(&self) {
        self.scope.retired.store(true, Ordering::Release);
        self.scope.fence.retire();
    }
    pub fn is_live(&self) -> bool {
        !self.scope.retired.load(Ordering::Acquire) && Instant::now() < self.scope.expires
    }
    pub fn authorize(&self, context: &HostContext, permission: Permission) -> Result<(), Error> {
        if self.scope.context != *context
            || !self.is_live()
            || !self.scope.permissions.contains(&permission)
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
