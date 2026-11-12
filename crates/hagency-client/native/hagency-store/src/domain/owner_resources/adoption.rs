//! Current canonical profile initialization and management authority.
use super::*;
use hagency_core::project::identifier;
mod current;
mod management;
pub use management::CurrentManagementStore;
#[cfg(test)]
pub(crate) use current::TestCurrentManagement;
#[cfg(test)]
mod tests;
/// Current profile confirmation retains the exact owner root and SDK epoch.
#[derive(Clone)]
pub struct ProfileInitializationAuthority {
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    expires: Instant,
    native: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl ProfileInitializationAuthority {
    pub fn new(
        registry: OwnerResourceRegistry,
        profile: ProfileAuthority,
        expires: Instant,
        native: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        if expires > Instant::now() + Duration::from_secs(60) {
            return Err(Error::LocalAuthority);
        }
        let value = Self {
            registry,
            profile,
            expires,
            native: Arc::new(native),
        };
        value.check()?;
        Ok(value)
    }
    fn check(&self) -> Result<(), Error> {
        self.profile.check()?;
        if Instant::now() >= self.expires
            || self.registry.0.closed.load(Ordering::Acquire)
            || !(self.native)()
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
