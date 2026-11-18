//! The device of a proof that is still live, read without waiting for a
//! renewal.
//!
//! Why (Nov 18, demo windows anna and betty): every `authorized_device` call
//! takes the one `renewal` line and, at most five seconds after the last
//! check, asks the server again (up to 8 s). An Agent's turn makes several such
//! calls a second (running check, lease, messages), so while the server was
//! slow (502s on lease renewal) the owner's read of that Agent's waiting
//! requests queued behind them, missed its 2 s bound and was reported
//! `approvals_read_timeout` (43 and 64 log lines in one hour, clustered with
//! the lease failures); the cards it held expired unseen. That read is
//! in-memory and needs only a proof that is still valid, not a fresh one.
use super::*;

/// A proof this close to its end is renewed first, never used as live.
const LIVE_MARGIN: Duration = Duration::from_secs(2);

impl ServerLogin {
    /// The current device while its server proof is still valid for at least
    /// `LIVE_MARGIN`, without taking the renewal line. A lapsed, invalidated
    /// or stopped proof falls back to `authorized_device` (which renews or
    /// refuses), so this never serves authority the full path would not.
    pub(in crate::console) async fn live_device(&self) -> Result<AuthorizedDevice, Error> {
        if let Some(device) = self.live_device_now().await {
            return Ok(device);
        }
        self.authorized_device().await
    }

    async fn live_device_now(&self) -> Option<AuthorizedDevice> {
        if self.stopped.load(Ordering::Acquire) {
            return None;
        }
        let sessions = self.sessions.lock().await;
        // The same session `authorized_device` selects: the first one still valid.
        let (_, session) = sessions.iter().find(|(_, s)| !s.invalidated)?;
        if session.authorized_until <= Instant::now() + LIVE_MARGIN {
            return None;
        }
        let mut device = session.device.clone()?;
        device.valid_until = session.authorized_until;
        device.stopped = self.stopped.clone();
        (!self.stopped.load(Ordering::Acquire)).then_some(device)
    }
}
