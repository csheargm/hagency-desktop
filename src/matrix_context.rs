//! Fence asynchronous UI work to the Matrix session that initiated it.
use matrix_sdk::Client;

pub(crate) fn is_current(client: &Client) -> bool {
    crate::sliding_sync::get_client().is_some_and(|current| {
        // SessionMeta is stored inside the SDK's shared BaseClient. Its address
        // remains equal for Client clones, but differs after a same-user/device
        // re-login. Checking only the IDs would accept work from that old session.
        client
            .session_meta()
            .zip(current.session_meta())
            .is_some_and(|(a, b)| std::ptr::eq(a, b))
            && client.user_id().is_some()
            && current.user_id() == client.user_id()
            && current.device_id() == client.device_id()
            && current.homeserver() == client.homeserver()
    })
}

pub(crate) fn ensure_current(client: &Client) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_current(client),
        "The Matrix session changed. Reopen this panel."
    );
    Ok(())
}

/// Opaque, in-process identity of the SDK session that initiated UI work.
/// Same-user/device re-login must not accept a queued result from its old SDK.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SessionPin {
    epoch: u64,
    revision: u64,
    instance: usize,
    owner: matrix_sdk::ruma::OwnedUserId,
    device: matrix_sdk::ruma::OwnedDeviceId,
    origin: String,
}
impl std::fmt::Debug for SessionPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixSession")
            .field("epoch", &self.epoch)
            .field("owner", &self.owner)
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}
impl SessionPin {
    pub(crate) fn capture(client: &Client) -> Option<Self> {
        Some(Self {
            epoch: crate::account_session::epoch(),
            revision: crate::sliding_sync::client_revision(),
            instance: std::ptr::from_ref(client.session_meta()?) as usize,
            owner: client.user_id()?.to_owned(),
            device: client.device_id()?.to_owned(),
            origin: client.homeserver().to_string(),
        })
    }
    pub(crate) fn is_current(&self) -> bool {
        self.epoch == crate::account_session::epoch()
            && !crate::sliding_sync::account_changing()
            && !crate::account_session::is_closing()
            && !crate::logout::logout_state_machine::is_logout_in_progress()
            && crate::sliding_sync::get_client()
                .is_some_and(|client| Self::capture(&client).as_ref() == Some(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::crypto_settings::test_support::{Server, install_link_client};

    #[test]
    fn recovery_session_pin_retires_on_same_sdk_replacement_and_epoch_close() {
        const NAME: &str = "matrix_context::tests::recovery_session_pin_retires_on_same_sdk_replacement_and_epoch_close";
        if std::env::var("HAGENCY_SESSION_PIN_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_SESSION_PIN_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let server = Server::with_handler(false, std::sync::Arc::new(|_| None));
            let client = server.room().await.client();
            install_link_client(Some(client.clone()));
            let original = SessionPin::capture(&client).unwrap();
            assert!(original.is_current());
            // Even the same SDK object's IDs and pointer cannot survive Desktop retirement.
            install_link_client(Some(client.clone()));
            assert!(!original.is_current());
            let replacement = SessionPin::capture(&client).unwrap();
            assert!(replacement.is_current());
            crate::account_session::close().await;
            assert!(!replacement.is_current());
            crate::account_session::open();
            assert!(!replacement.is_current());
            assert!(SessionPin::capture(&client).unwrap().is_current());
            install_link_client(None);
        });
    }
}
