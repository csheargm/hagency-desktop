//! The Agent publishes whose Agent it is, and the runtime it runs on, in its
//! own Matrix profile ([`hagency_core::agent_owner`]), so a room member on
//! another server can name its owner and runtime before it has posted
//! anything. Each field is sent once per Agent and value per process, the
//! first time its device is used (and again whenever the value changes, back
//! to an earlier one too); a failed write is tried again the next time the
//! device is used.
use super::*;
use hagency_core::agent_owner::{OWNER_FIELD, RUNTIME_FIELD, may_claim, may_claim_runtime};
use matrix_sdk::ruma::api::client::profile::ProfileFieldValue;
use std::collections::HashMap;

/// (Agent, field) → the value this process last published (or is
/// publishing). Only the last value counts: after A → B, naming A again must
/// write A.
static CLAIMED: LazyLock<std::sync::Mutex<HashMap<(String, &'static str), String>>> = LazyLock::new(Default::default);

impl AgentDevice {
    /// Publishes `owner` as this Agent's owner unless it already did, in the
    /// background: the device's work never waits for it.
    pub(in crate::console) fn claim_owner(&self, owner: &str) {
        self.claim(OWNER_FIELD, owner);
    }

    /// Publishes `runtime` ("claude", "codex", "octos") as the runtime this
    /// Agent runs on, like [`Self::claim_owner`].
    pub(in crate::console) fn claim_runtime(&self, runtime: &str) {
        self.claim(RUNTIME_FIELD, runtime);
    }

    fn claim(&self, field: &'static str, value: &str) {
        let Some(agent) = self.client.user_id().map(ToString::to_string) else { return };
        if !start_claim(&agent, field, value) { return; }
        let (client, value) = (self.client.clone(), value.to_owned());
        tokio::spawn(async move {
            if let Err(error) = publish(&client, field, &value).await {
                // Retried on the next use; until then readers fall back to
                // the room roster and the Agent's run cards.
                tracing::warn!(stage = "agent_profile_claim", field, %error, "Agent profile field not published; retrying on next use");
                forget_claim(&agent, field, &value);
            }
        });
    }
}

/// The signed-in owner's console runs this Agent: it publishes that owner,
/// and the runtime this start runs it on.
pub(super) fn claim_profile<T: Transport>(transport: &T, device: &AgentDevice, config: &StartConfig) {
    if let Some(host) = transport.approval_host() {
        device.claim_owner(&host.pin.owner);
        device.claim_runtime(hagency_agent_local::runtime::kind(&config.credential_ref).name());
    }
}

/// Whether to publish `value` for `agent`'s `field` now: not when it is
/// already the last value published there, or not a claim the Agent may make
/// (an owner on another server, an unknown runtime).
fn start_claim(agent: &str, field: &'static str, value: &str) -> bool {
    let allowed = match field {
        OWNER_FIELD => may_claim(agent, value),
        RUNTIME_FIELD => may_claim_runtime(agent, value),
        _ => false,
    };
    if !allowed {
        tracing::warn!(stage = "agent_profile_claim", field, "Agent may not publish this value; not published");
        return false;
    }
    let mut claimed = CLAIMED.lock().unwrap_or_else(|e| e.into_inner());
    let key = (agent.to_owned(), field);
    if claimed.get(&key).is_some_and(|last| last == value) { return false; }
    claimed.insert(key, value.to_owned());
    true
}

/// A failed write of `value`: forgotten so the next use retries, unless a
/// newer value was started since (that write is the one that counts).
fn forget_claim(agent: &str, field: &'static str, value: &str) {
    let mut claimed = CLAIMED.lock().unwrap_or_else(|e| e.into_inner());
    let key = (agent.to_owned(), field);
    if claimed.get(&key).is_some_and(|last| last == value) { claimed.remove(&key); }
}

async fn publish(client: &Client, field: &str, value: &str) -> Result<(), matrix_sdk::Error> {
    let value = ProfileFieldValue::new(field, value.into())?;
    client.account().set_profile_field(value).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_is_published_once_and_again_after_a_failure() {
        let (agent, owner) = ("@_hagency_agt_claim1:beta", "@bird:beta");
        assert!(start_claim(agent, OWNER_FIELD, owner), "first use publishes");
        assert!(!start_claim(agent, OWNER_FIELD, owner), "a second use does not send again");
        forget_claim(agent, OWNER_FIELD, owner);
        assert!(start_claim(agent, OWNER_FIELD, owner), "a failed write is retried");
        assert!(start_claim(agent, OWNER_FIELD, "@eve:beta"), "a new owner is published");
    }

    /// GPT integration review: the cache held every pair ever published, so
    /// after A → B naming A again was skipped and the profile kept saying B.
    #[test]
    fn going_back_to_an_earlier_owner_publishes_it_again() {
        let agent = "@_hagency_agt_claim4:beta";
        assert!(start_claim(agent, OWNER_FIELD, "@bird:beta"));
        assert!(start_claim(agent, OWNER_FIELD, "@eve:beta"));
        assert!(start_claim(agent, OWNER_FIELD, "@bird:beta"), "A → B → A writes A again");
        assert!(!start_claim(agent, OWNER_FIELD, "@bird:beta"), "A is now the last value");
        forget_claim(agent, OWNER_FIELD, "@eve:beta");
        assert!(!start_claim(agent, OWNER_FIELD, "@bird:beta"), "an older failed write does not drop the newer owner");
    }

    #[test]
    fn an_owner_on_another_server_or_an_unknown_runtime_is_never_published() {
        assert!(!start_claim("@_hagency_agt_claim2:beta", OWNER_FIELD, "@anna:alpha"));
        assert!(!start_claim("@bird:beta", OWNER_FIELD, "@bird:beta"), "not an Agent");
        assert!(!start_claim("@_hagency_agt_claim2:beta", RUNTIME_FIELD, "gpt-9"));
        assert!(!start_claim("@bird:beta", RUNTIME_FIELD, "claude"), "not an Agent");
    }

    /// The runtime is its own field: publishing it never stands in for the
    /// owner's write, and a runtime change is published again.
    #[test]
    fn the_runtime_is_claimed_beside_the_owner() {
        let agent = "@_hagency_agt_claim5:beta";
        assert!(start_claim(agent, OWNER_FIELD, "@bird:beta"));
        assert!(start_claim(agent, RUNTIME_FIELD, "claude"), "the owner's claim does not cover the runtime");
        assert!(!start_claim(agent, RUNTIME_FIELD, "claude"));
        assert!(start_claim(agent, RUNTIME_FIELD, "codex"), "a new runtime is published");
        assert!(!start_claim(agent, OWNER_FIELD, "@bird:beta"), "the owner is untouched");
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
        publish(&client, OWNER_FIELD, "@bird:beta").await.unwrap();
        let request = server.await.unwrap();
        let head = request.lines().next().unwrap_or_default();
        assert!(head.starts_with("PUT ") && head.contains("_hagency_agt_claim3") && head.contains("/io.hagency.owner "), "{head}");
        assert!(request.ends_with(r#"{"io.hagency.owner":"@bird:beta"}"#), "{request}");
    }
}
