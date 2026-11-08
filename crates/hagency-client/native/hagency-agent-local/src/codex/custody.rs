//! Retained whole-process-tree custody for native Codex tools and extensions.
use super::{Profile, Result, Session};
use hagency_platform::SupervisedProcess;
use hagency_runtime::owned::Cleanup;
use std::{
    ops::{Deref, DerefMut},
    path::Path,
    time::Duration,
};
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::net::unix::pipe::{Receiver, Sender};
#[cfg(windows)]
use hagency_platform::{WindowsPipe as Receiver, WindowsPipe as Sender};

pub struct Owned {
    session: Session<Receiver, Sender>,
    owner: SupervisedProcess,
    stderr: tokio::task::JoinHandle<()>,
    cleanup: Cleanup,
}
impl Owned {
    pub fn spawn(profile: &Profile, guardian: &Path) -> Result<Self> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(super::Error::Profile(
                "Codex process requires an IO runtime",
            ));
        }
        profile.validate()?;
        let (mut owner, pipes) = SupervisedProcess::spawn_piped(guardian, &profile.launch(&[])?)?;
        let connected = (|| -> std::io::Result<_> {
            #[cfg(unix)]
            {
                let (stdin, stdout, stderr) = pipes.into_parts();
                Ok((
                    Sender::from_owned_fd(stdin)?,
                    Receiver::from_owned_fd(stdout)?,
                    Receiver::from_owned_fd(stderr)?,
                ))
            }
            #[cfg(windows)]
            {
                pipes.into_async_parts()
            }
        })();
        let (stdin, stdout, mut stderr) = match connected {
            Ok(pipes) => pipes,
            Err(error) => {
                let _ = owner.stop(Duration::from_secs(3));
                return Err(error.into());
            }
        };
        // Bounded memory, no provider text logged, and no detached reader when
        // this owner stops. Native helpers must never block on a full stderr.
        let stderr = tokio::spawn(async move {
            let mut buffer = [0; 8192];
            while matches!(stderr.read(&mut buffer).await, Ok(n) if n > 0) {}
        });
        Ok(Self {
            session: profile.session(stdout, stdin),
            owner,
            stderr,
            cleanup: Cleanup::Pending,
        })
    }
    pub fn stop(&mut self) -> Cleanup {
        if !matches!(self.cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = match self.owner.stop(Duration::from_secs(3)) {
                Ok(report) => Cleanup::Observed(report),
                Err(error) => Cleanup::Unknown { kind: error.kind() },
            };
        }
        self.stderr.abort();
        self.cleanup
    }
}
impl Deref for Owned {
    type Target = Session<Receiver, Sender>;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}
impl DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}
