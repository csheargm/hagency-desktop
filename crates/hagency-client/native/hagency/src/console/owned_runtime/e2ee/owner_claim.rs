//! The Agent publishes whose Agent it is in its own Matrix profile
//! ([`hagency_core::agent_owner`]), so a room member on another server can
//! name its owner before it has posted anything. Sent once per Agent and
//! owner per process, the first time its device is used; a failed write is
//! tried again the next time the device is used.
use super::*;
use hagency_core::agent_owner::{OWNER_FIELD, may_claim};
use matrix_sdk::ruma::api::client::profile::ProfileFieldValue;
use std::collections::HashSet;

/// `"<agent> <owner>"` pairs published (or being published) by this process.
static CLAIMED: LazyLock<std::sync::Mutex<HashSet<String>>> = LazyLock::new(Default::default);

impl AgentDevice {
    /// Publishes `owner` as this Agent's owner unless it already did, in the
    /// background: the device's work never waits for it.
    pub(in crate::console) fn claim_owner(&self, owner: &str) {
        let Some(agent) = self.client.user_id().map(ToString::to_string) else { return };
        let Some(key) = start_claim(&agent, owner) else { return };
        let (client, owner) = (self.client.clone(), owner.to_owned());
        tokio::spawn(async move {
            if let Err(error) = publish(&client, &owner).await {
                // Retried on the next use; until then readers fall back to
                // the room roster and the Agent's run cards.
                tracing::warn!(stage = "agent_owner_claim", %error, "Agent owner not published; retrying on next use");
                forget_claim(&key);
            }
        });
    }
}

/// The key to publish under, or `None` when this pair is already published or
/// is not a claim the Agent may make (an owner on another server).
fn start_claim(agent: &str, owner: &str) -> Option<String> {
    if !may_claim(agent, owner) {
        tracing::warn!(stage = "agent_owner_claim", "Agent owner is not a person on the Agent's server; not published");
        return None;
    }
    let key = format!("{agent} {owner}");
    CLAIMED.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()).then_some(key)
}

fn forget_claim(key: &str) {
    CLAIMED.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
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
        let key = start_claim(agent, owner).expect("first use publishes");
        assert_eq!(start_claim(agent, owner), None, "a second use does not send again");
        forget_claim(&key);
        assert!(start_claim(agent, owner).is_some(), "a failed write is retried");
        assert!(start_claim(agent, "@eve:beta").is_some(), "a new owner is published");
    }

    #[test]
    fn an_owner_on_another_server_is_never_published() {
        assert_eq!(start_claim("@_hagency_agt_claim2:beta", "@anna:alpha"), None);
        assert_eq!(start_claim("@bird:beta", "@bird:beta"), None, "not an Agent");
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
