//! Pasion provisions a new Matrix user through an async job. A token minted
//! before that job runs is refused by Palpo ("User not found (not yet
//! provisioned?)"), and the SDK cannot retry an exchanged authorization code,
//! so wait for the account to exist before authorizing.

use anyhow::{bail, Result};
use std::time::Duration;

/// How long a fresh account may take to appear on the homeserver.
pub const PROVISION_WAIT: Duration = Duration::from_secs(15);
/// Pause between checks while waiting for provisioning.
pub const PROVISION_POLL: Duration = Duration::from_millis(300);
const NOT_READY: &str = "Your account was created, but the server is still setting it up. Wait a moment, then sign in.";

/// Whether the homeserver already knows `@username:server`. `None` means the
/// server cannot tell us without a token, so we stop waiting.
async fn account_exists(http: &matrix_sdk::reqwest::Client, origin: &str, user_id: &str) -> Result<Option<bool>> {
    let mut url = url::Url::parse(origin)?;
    url.path_segments_mut().map_err(|_| anyhow::anyhow!("Invalid homeserver URL"))?
        .pop_if_empty().extend(["_matrix", "client", "v3", "profile", user_id]);
    let status = http.get(url).send().await?.status();
    Ok(match status.as_u16() {
        200 => Some(true),
        404 => Some(false),
        _ => None,
    })
}

async fn server_name(http: &matrix_sdk::reqwest::Client, origin: &str) -> Result<String> {
    let raw = http.get(format!("{origin}/hagency/api/v1/discovery")).send().await?.bytes().await?;
    let body: serde_json::Value = serde_json::from_slice(&raw)?;
    match body["serverName"].as_str() {
        Some(name) if !name.is_empty() => Ok(name.to_owned()),
        _ => bail!("This server did not advertise its Matrix server name"),
    }
}

/// Wait until a just-registered `username` exists on the homeserver at `origin`.
pub async fn wait_for_new_account(origin: &str, username: &str, wait: Duration, poll: Duration) -> Result<()> {
    let origin = origin.trim_end_matches('/');
    let http = crate::http::client_builder().timeout(Duration::from_secs(8)).build()?;
    let user_id = format!("@{}:{}", username.trim_start_matches('@'), server_name(&http, origin).await?);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match account_exists(&http, origin, &user_id).await? {
            Some(true) | None => return Ok(()),
            Some(false) if tokio::time::Instant::now() + poll < deadline => tokio::time::sleep(poll).await,
            Some(false) => bail!(NOT_READY),
        }
    }
}
