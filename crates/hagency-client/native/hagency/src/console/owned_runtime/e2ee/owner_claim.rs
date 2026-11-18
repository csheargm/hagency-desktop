//! The Agent publishes whose Agent it is in its own Matrix profile
//! ([`hagency_core::agent_owner`]), so a room member on another server can
//! name its owner before it has posted anything. Sent once per Agent and
//! owner per process, the first time its device is used (and again whenever
//! its owner changes, back to an earlier one too); a failed write is tried
//! again the next time the device is used.
use super::*;
use hagency_core::agent_owner::{OWNER_FIELD, may_claim};
use matrix_sdk::ruma::api::client::profile::ProfileFieldValue;
use std::collections::HashMap;

/// Agent → the owner this process last published (or is publishing) for it.
/// Only the last value counts: after A → B, naming A again must write A.
static CLAIMED: LazyLock<std::sync::Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

impl AgentDevice {
    /// Publishes `owner` as this Agent's owner unless it already did, in the
    /// background: the device's work never waits for it.
    pub(in crate::console) fn claim_owner(&self, owner: &str) {
        let Some(agent) = self.client.user_id().map(ToString::to_string) else { return };
        if !start_claim(&agent, owner) { return; }
        let (client, owner) = (self.client.clone(), owner.to_owned());
        tokio::spawn(async move {
            if let Err(error) = publish(&client, &owner).await {
                // Retried on the next use; until then readers fall back to
                // the room roster and the Agent's run cards.
                tracing::warn!(stage = "agent_owner_claim", %error, "Agent owner not published; retrying on next use");
                forget_claim(&agent, &owner);
            }
        });
    }
}

/// Whether to publish `owner` now: not when it is already the last value
/// published for `agent`, or not a claim the Agent may make (an owner on
/// another server).
fn start_claim(agent: &str, owner: &str) -> bool {
    if !may_claim(agent, owner) {
        tracing::warn!(stage = "agent_owner_claim", "Agent owner is not a person on the Agent's server; not published");
        return false;
    }
    let mut claimed = CLAIMED.lock().unwrap_or_else(|e| e.into_inner());
    if claimed.get(agent).is_some_and(|last| last == owner) { return false; }
    claimed.insert(agent.to_owned(), owner.to_owned());
    true
}

/// A failed write of `owner`: forgotten so the next use retries, unless a
/// newer owner was started since (that write is the one that counts).
fn forget_claim(agent: &str, owner: &str) {
    let mut claimed = CLAIMED.lock().unwrap_or_else(|e| e.into_inner());
    if claimed.get(agent).is_some_and(|last| last == owner) { claimed.remove(agent); }
}

async fn publish(client: &Client, owner: &str) -> Result<(), matrix_sdk::Error> {
    let value = ProfileFieldValue::new(OWNER_FIELD, owner.into())?;
    client.account().set_profile_field(value).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_is_published_once_and_again_after_a_failure() {
        let (agent, owner) = ("@_hagency_agt_claim1:beta", "@bird:beta");
        assert!(start_claim(agent, owner), "first use publishes");
        assert!(!start_claim(agent, owner), "a second use does not send again");
        forget_claim(agent, owner);
        assert!(start_claim(agent, owner), "a failed write is retried");
        assert!(start_claim(agent, "@eve:beta"), "a new owner is published");
    }

    /// GPT integration review: the cache held every pair ever published, so
    /// after A → B naming A again was skipped and the profile kept saying B.
    #[test]
    fn going_back_to_an_earlier_owner_publishes_it_again() {
        let agent = "@_hagency_agt_claim4:beta";
        assert!(start_claim(agent, "@bird:beta"));
        assert!(start_claim(agent, "@eve:beta"));
        assert!(start_claim(agent, "@bird:beta"), "A → B → A writes A again");
        assert!(!start_claim(agent, "@bird:beta"), "A is now the last value");
        forget_claim(agent, "@eve:beta");
        assert!(!start_claim(agent, "@bird:beta"), "an older failed write does not drop the newer owner");
    }

    #[test]
    fn an_owner_on_another_server_is_never_published() {
        assert!(!start_claim("@_hagency_agt_claim2:beta", "@anna:alpha"));
        assert!(!start_claim("@bird:beta", "@bird:beta"), "not an Agent");
    }

    /// User flow: the Agent's device is signed in and the claim reaches
    /// the Agent's homeserver as its own profile field.
    #[tokio::test]
    async fn the_claim_is_the_agents_own_profile_field() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        let client = Client::builder().homeserver_url(&origin)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
        client.matrix_auth().restore_session(MatrixSession {
            meta: SessionMeta { user_id: "@_hagency_agt_claim3:beta".try_into().unwrap(), device_id: "HAGE2EE1".into() },
            tokens: SessionTokens { access_token: "t".into(), refresh_token: None },
        }, RoomLoadSettings::default()).await.unwrap();
        publish(&client, "@bird:beta").await.unwrap();
        let request = server.await.unwrap();
        let head = request.lines().next().unwrap_or_default();
        assert!(head.starts_with("PUT ") && head.contains("_hagency_agt_claim3") && head.contains("/io.hagency.owner "), "{head}");
        assert!(request.ends_with(r#"{"io.hagency.owner":"@bird:beta"}"#), "{request}");
    }
}
