//! Read-only account identity from the provider's authenticated public API.
//! Credential locators, email, model, runtime and local paths are not capacity.
use super::{
    Process, ProviderError, ProviderPaths, executable, failure, paths, selected_reference,
};
use crate::console::{Console, server_login::OwnerOperation};
use hagency_store::owner_resources::VerifiedProviderAccount;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

// Account metadata is only a switch detector. It never becomes the quota key
// and never leaves this module, even when the provider has no stable identity.
#[derive(PartialEq, Eq)]
struct AccountSnapshot {
    email: Option<String>,
    plan: String,
}
fn unavailable() -> ProviderError {
    failure(409, "provider_identity_unavailable")
}
pub(super) fn configuration(value: &Value) -> Result<(), ProviderError> {
    if value["model_provider"] != "openai"
        || value["chatgpt_base_url"] != "https://chatgpt.com/backend-api"
        || value["model_providers"]
            .get("openai")
            .is_some_and(|v| !v.is_null())
    {
        return Err(unavailable());
    }
    Ok(())
}
fn snapshot(value: &Value) -> Result<AccountSnapshot, ProviderError> {
    if value["requiresOpenaiAuth"] != true || value["account"]["type"] != "chatgpt" {
        return Err(unavailable());
    }
    let email = match value["account"].get("email") {
        Some(Value::Null) => None,
        Some(Value::String(s))
            if !s.is_empty() && s.len() <= 320 && !s.chars().any(char::is_control) =>
        {
            Some(s.clone())
        }
        _ => return Err(unavailable()),
    };
    let plan = value["account"]["planType"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .ok_or_else(unavailable)?;
    Ok(AccountSnapshot {
        email,
        plan: plan.into(),
    })
}
struct QuotaIdentity(String);
impl QuotaIdentity {
    fn read(value: &Value) -> Result<Self, ProviderError> {
        // Codex account/rateLimits/read's public accountId identifies the
        // backend account for this usage snapshot. An absent/unknown field is
        // unsupported, not a reason to hash email or the credential home.
        if !value["rateLimits"].is_object() {
            return Err(unavailable());
        }
        let id = value["accountId"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            })
            .ok_or_else(unavailable)?;
        Ok(Self(id.into()))
    }
    fn proof(self, deadline: Instant) -> Result<VerifiedProviderAccount, ProviderError> {
        VerifiedProviderAccount::new("codex", "openai.chatgpt", &self.0, deadline)
            .map_err(|_| unavailable())
    }
}
/// Sanitized, native-only provider report. It is never a credential or an
/// authority proof. Missing public account identity stays resource-scoped.
pub(in crate::console) struct ObservedQuota {
    pub(in crate::console) subject_digest: Option<String>,
    pub(in crate::console) plan: String,
    pub(in crate::console) limits: hagency_metering::dashboard::quota::CodexRateLimitsRead,
}
fn quota_identity(value: &Value) -> Result<Option<QuotaIdentity>, ProviderError> {
    if value.get("accountId").is_none_or(Value::is_null) {
        return Ok(None);
    }
    QuotaIdentity::read(value).map(Some)
}
fn observed_quota(account: AccountSnapshot, value: &Value) -> Result<ObservedQuota, ProviderError> {
    if !value["rateLimits"].is_object() { return Err(unavailable()); }
    let subject_digest = quota_identity(value)?.map(|id| {
        // Identical to the execution driver's RateLimitTracker digest; idle
        // and running Terra/Astra resources must group under the same account.
        hagency_runtime::codex::session::quota_account_digest(&id.0).ok_or_else(unavailable)
    }).transpose()?;
    let limits: hagency_metering::dashboard::quota::CodexRateLimitsRead =
        serde_json::from_value(value.clone()).map_err(|_| unavailable())?;
    if limits.rate_limits_by_limit_id.as_ref().is_some_and(|m|m.len()>32)
        || serde_json::to_vec(&limits).map_err(|_|unavailable())?.len()>16*1024 {
        return Err(unavailable());
    }
    Ok(ObservedQuota {subject_digest, plan:account.plan, limits})
}
/// Five read-only RPCs bracket the quota with unchanged account metadata.
/// No credential data or private account metadata escapes this function.
#[cfg(test)]
pub(super) async fn observe_quota(
    process: &mut Process,
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<ObservedQuota, ProviderError> {
    observe_quota_with_identity(process, paths, deadline).await.map(|(report, _)| report)
}

/// A canonical resource must match the public quota identity recorded at
/// enrollment. The report and proof come from the same bracketed observation.
pub(super) async fn observe_bound_quota(
    process: &mut Process,
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<(ObservedQuota, VerifiedProviderAccount), ProviderError> {
    let (report, proof) = observe_quota_with_identity(process, paths, deadline).await?;
    Ok((report, proof.ok_or_else(unavailable)?))
}

async fn observe_quota_with_identity(
    process: &mut Process,
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<(ObservedQuota, Option<VerifiedProviderAccount>), ProviderError> {
    selected_reference(&paths.codex_home, paths.shared)?;
    let first = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    let quota = process.rpc("account/rateLimits/read", json!({})).await?;
    let identity = quota_identity(&quota)?;
    let middle = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    let repeated = process.rpc("account/rateLimits/read", json!({})).await?;
    let repeated_identity = quota_identity(&repeated)?;
    let last = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    selected_reference(&paths.codex_home, paths.shared)?;
    if first != middle || first != last
        || identity.as_ref().map(|id|&id.0) != repeated_identity.as_ref().map(|id|&id.0) {
        return Err(failure(409, "provider_account_changed"));
    }
    if Instant::now() >= deadline { return Err(unavailable()); }
    let proof = repeated_identity.map(|identity| identity.proof(deadline)).transpose()?;
    Ok((observed_quota(last, &repeated)?, proof))
}
async fn observe(
    process: &mut Process,
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<VerifiedProviderAccount, ProviderError> {
    // Keep proof issuance stricter than telemetry: quota reports without a
    // public stable subject can be displayed but never enroll capacity.
    selected_reference(&paths.codex_home, paths.shared)?;
    let first = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    let quota = QuotaIdentity::read(&process.rpc("account/rateLimits/read", json!({})).await?)?;
    let middle = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    let repeated = QuotaIdentity::read(&process.rpc("account/rateLimits/read", json!({})).await?)?;
    let last = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    selected_reference(&paths.codex_home, paths.shared)?;
    if first != middle || first != last || quota.0 != repeated.0 {
        return Err(failure(409, "provider_account_changed"));
    }
    quota.proof(deadline)
}

/// Native host only: no JSON command can obtain or submit an identity proof.
/// Existing provider login is reused; this never initiates login or inference.
pub(in crate::console) async fn verify(
    console: &Console,
    cookie: &str,
) -> Result<(VerifiedProviderAccount, Instant), ProviderError> {
    verify_runtime(console, cookie, hagency_agent_local::runtime::Kind::Codex).await
}

pub(in crate::console) async fn verify_runtime(
    console: &Console,
    cookie: &str,
    runtime: hagency_agent_local::runtime::Kind,
) -> Result<(VerifiedProviderAccount, Instant), ProviderError> {
    use hagency_agent_local::runtime::Kind;
    if !matches!(runtime, Kind::Codex | Kind::Claude) { return Err(unavailable()); }
    let tasks = &console.0.owner_provider.tasks;
    let _activity = tasks.enter().map_err(|_| failure(409, "provider_closing"))?;
    let reply = console
        .owner_api(cookie, OwnerOperation::Agents)
        .await
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    let device = console
        .authorized_device()
        .await
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    if device.origin() != reply.origin
        || device.issuer() != reply.issuer
        || device.subject() != reply.subject
        || device.owner_mxid() != reply.owner
    {
        return Err(failure(401, "owner_authorization_required"));
    }
    device
        .bearer()
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    let root = console
        .0
        .server_login
        .state_directory()
        .ok_or(failure(503, "provider_directory_unavailable"))?;
    let select = || match runtime {
        Kind::Codex => paths(&root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner),
        Kind::Claude => super::external::connected_paths(&root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner, runtime),
        _ => Err(unavailable()),
    };
    let paths = select()?;
    let deadline = device
        .valid_until()
        .min(Instant::now() + Duration::from_secs(30));
    // A short-lived child prevents cached identity from another profile/session
    // being adopted. Shared homes use the existing sealed backend-origin and
    // no-tools configuration. Dedicated homes reject ambient configuration.
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        if runtime == Kind::Claude {
            let binding = super::claude_identity::observe(&paths, deadline).await?;
            let proof = VerifiedProviderAccount::from_claude_binding(binding).map_err(|_| unavailable())?;
            let current = select()?;
            if current.home != paths.home || current.codex_home != paths.codex_home
                || current.credential_ref != paths.credential_ref || current.shared != paths.shared
            { return Err(failure(409, "provider_account_changed")); }
            return Ok(proof);
        }
        let mut process = Process::spawn_inner(&paths, &executable()?, true, tasks).await?;
        let result = observe(&mut process, &paths, deadline).await;
        let _ = process.child.kill().await;
        result
    })
    .await
    .map_err(|_| failure(504, "provider_identity_timeout"))?;
    device
        .bearer()
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    let fresh = console
        .owner_api(cookie, OwnerOperation::Agents)
        .await
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    if fresh.origin != reply.origin
        || fresh.issuer != reply.issuer
        || fresh.subject != reply.subject
        || fresh.owner != reply.owner
    {
        return Err(failure(401, "owner_authorization_required"));
    }
    result
        .map(|proof| (proof, deadline))
        .map_err(|error| match error.code {
            "provider_account_changed" | "provider_identity_unavailable" => error,
            _ => unavailable(),
        })
}

/// Catalog metadata does not establish a quota identity or grant enrollment.
/// Compare private account observations around the model read, never expose them.
pub(super) async fn inspect_models(process:&mut Process,paths:&ProviderPaths)->Result<super::ProviderStatus,ProviderError>{
    let value=process.rpc("account/read",json!({"refreshToken":false})).await?;
    if value["account"].is_null() {return Err(failure(409,"provider_connection_required"));}
    let before=snapshot(&value)?;
    let catalog=process.models(paths).await?;
    let after=snapshot(&process.rpc("account/read",json!({"refreshToken":false})).await?)?;
    if before!=after || !catalog.authenticated {return Err(failure(409,"provider_account_changed"));}
    Ok(catalog)
}

/// A model catalog and quota identity observed from one unchanged account.
/// The proof stays native; email, credential locators and catalog IDs are never
/// substituted for the provider's public quota-account identity.
pub(super) async fn inspect_resource(
    process: &mut Process,
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<(VerifiedProviderAccount, super::ProviderStatus), ProviderError> {
    selected_reference(&paths.codex_home, paths.shared)?;
    let value = process.rpc("account/read", json!({"refreshToken":false})).await?;
    if value["account"].is_null() { return Err(failure(409, "provider_connection_required")); }
    let before = snapshot(&value)?;
    let quota = QuotaIdentity::read(&process.rpc("account/rateLimits/read", json!({})).await?)?;
    let catalog = inspect_models(process, paths).await?;
    let repeated = QuotaIdentity::read(&process.rpc("account/rateLimits/read", json!({})).await?)?;
    let after = snapshot(&process.rpc("account/read", json!({"refreshToken":false})).await?)?;
    selected_reference(&paths.codex_home, paths.shared)?;
    if before != after || quota.0 != repeated.0 {
        return Err(failure(409, "provider_account_changed"));
    }
    Ok((quota.proof(deadline)?, catalog))
}
