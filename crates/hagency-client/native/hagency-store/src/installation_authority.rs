//! Cooperative installation-wide resource writer exclusion.
//! Current resource writes retain a finite native permission gate in addition
//! to the OS lock. Neither files nor locks grant a resource/profile authority.
use crate::Error;
#[cfg(test)]
use crate::private;
use hagency_platform::legacy_writer::LegacyWriterGuard;
#[cfg(any(test, feature = "test-support"))]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[cfg(test)]
const LOCK: &str = "resource-authority.lock";
const MAX_LEASE: Duration = Duration::from_secs(300);
impl From<hagency_platform::legacy_writer::Error> for Error {
    fn from(error: hagency_platform::legacy_writer::Error) -> Self {
        use hagency_platform::legacy_writer::Error as E;
        match error {
            E::Io(e) => Self::Io(e),
            E::Private => Self::Private,
            E::Locked => Self::Locked,
            E::Unavailable => Self::PlatformUnavailable,
            E::TestRoot => Self::LocalAuthority,
        }
    }
}
fn deadline(expires: Instant) -> Result<(), Error> {
    let now = Instant::now();
    if expires <= now || expires > now + MAX_LEASE {
        Err(Error::LocalAuthority)
    } else {
        Ok(())
    }
}

struct LegacyInner {
    _files: LegacyWriterGuard,
    live: AtomicBool,
    expires: Instant,
    gate: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Retain through every queued resource effect, including its final commit.
/// Retirement prevents effects but releases no lock until all copies are gone.
#[derive(Clone)]
pub struct LegacyResourceLease(Arc<LegacyInner>);
impl LegacyResourceLease {
    /// Blocking native acquisition, using the OS account rather than app paths.
    /// The additional gate must be synchronous and must perform no I/O.
    pub fn acquire(
        expires: Instant,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        deadline(expires)?;
        let gate: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(gate);
        if !gate() {
            return Err(Error::LocalAuthority);
        }
        Self::from_guard(LegacyWriterGuard::acquire()?, expires, gate)
    }
    #[cfg(any(test, feature = "test-support"))]
    fn at(
        root: &Path,
        expires: Instant,
        gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, Error> {
        deadline(expires)?;
        if !gate() {
            return Err(Error::LocalAuthority);
        }
        Self::from_guard(LegacyWriterGuard::isolated_for_tests(root)?, expires, gate)
    }
    fn from_guard(
        files: LegacyWriterGuard,
        expires: Instant,
        gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, Error> {
        let lease = Self(Arc::new(LegacyInner {
            _files: files,
            live: AtomicBool::new(true),
            expires,
            gate,
        }));
        lease.check()?;
        Ok(lease)
    }
    /// Synchronous writer fence; the retained OS lock excludes cooperating
    /// exclusive writers without doing filesystem I/O inside a transaction.
    pub fn check(&self) -> Result<(), Error> {
        if !self.0.live.load(Ordering::Acquire)
            || Instant::now() >= self.0.expires
            || !(self.0.gate)()
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub fn retire(&self) {
        self.0.live.store(false, Ordering::Release);
    }
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn isolated_for_tests(
        root: &Path,
        expires: Instant,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        Self::at(root, expires, Arc::new(gate))
    }
}

#[cfg(test)]
mod tests;
