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
    /// refuses). Tradeoff: a withdrawal on the server is seen at the next
    /// renewal or when the proof lapses, as for any request already admitted.
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

    /// The current device for checking an answer already received, without
    /// waiting for a renewal: its identity and revocation, even once its proof
    /// passed its local deadline (GPT batch review, Nov 18: waiting here
    /// turned a successful lease renewal into a lost answer). `None` once
    /// signed out or stopped.
    pub(in crate::console) async fn current_device_now(&self) -> Option<AuthorizedDevice> {
        if self.stopped.load(Ordering::Acquire) {
            return None;
        }
        let sessions = self.sessions.lock().await;
        let (_, session) = sessions.iter().find(|(_, s)| !s.invalidated)?;
        let mut device = session.device.clone()?;
        device.stopped = self.stopped.clone();
        Some(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer is checked against the current device at once, even when
    /// its proof has lapsed and a renewal would have to wait.
    #[tokio::test]
    async fn the_current_device_is_read_without_waiting_for_a_lapsed_proof() {
        let login = ServerLogin::new(None).unwrap();
        let origin = "https://server.example/";
        let (device, _) = fixture_device(origin, "owner", "@owner:test");
        let now = Instant::now();
        login.sessions.lock().await.insert(session_key("lapsed-cookie"), RemoteSession {
            matrix_source: None, token: "owner-session".into(), oauth_token: "oauth".into(),
            user_id: "owner".into(), device: Some(device), refresh_token: None,
            oauth_expires: now, authorized_until: now, invalidated: false,
            binding: Binding { origin: origin.into(), client_id: "client".into(),
                installation_id: "lapsed-installation".into(), name: "fixture".into(),
                owner: Some("@owner:test".into()), subject: Some("owner".into()) },
            checked: now, expires: now + Duration::from_secs(900),
        });
        assert!(login.live_device_now().await.is_none(), "the lapsed proof is not live");
        let current = login.current_device_now().await.expect("read without a renewal");
        assert_eq!(current.subject(), "owner");
        login.stopped.store(true, Ordering::Release);
        assert!(login.current_device_now().await.is_none(), "a stopped login has no device");
    }
}
