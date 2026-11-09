//! Device-pinned configuration transport and original completion custody.
#[cfg(test)]
mod http_tests;
use super::*;
use crate::console::device_execution::DeviceIdentity;
use hagency_agent_local::{ModelProfile, Policy, PolicyVersion, runtime::Kind};
use std::{io::Read, path::PathBuf, time::Duration};
const CAPABILITY: &str = "execution-configuration-v1";
const MAX: usize = 65536;
// Request envelopes may contain both a bounded original input and a bounded result.
const MAX_ENVELOPE: usize = MAX * 2 + 16384;
fn unavailable() -> NativeError {
    failure(503, "configuration_local_state_unavailable")
}
fn invalid() -> NativeError {
    failure(502, "invalid_configuration_response")
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(i64::MAX, |d| d.as_millis().min(i64::MAX as u128) as i64)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Work {
    id: String,
    agent_id: String,
    agent_generation: i64,
    execution_device_id: String,
    device_generation: i64,
    operation: String,
    input: Value,
    state: String,
    created_at_ms: i64,
    expires_at_ms: i64,
    claimed_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    result: Option<Value>,
    error_code: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    pin: DeviceIdentity,
    work: Work,
    completion: Option<Value>,
}
fn operation(command: Command, agent: &str) -> Result<(&'static str, Value), NativeError> {
    match command {
        Command::Local {
            agent_id,
            action,
            input,
            binding_id: None,
            requester: None,
        } if agent_id == agent => {
            let op = match action {
                LocalAction::AgentModel => "agent_model",
                LocalAction::AgentPolicy => "agent_policy",
                LocalAction::SaveAgentModel => "save_agent_model",
                LocalAction::SaveAgentPolicy => "save_agent_policy",
                _ => return Err(failure(400, "configuration_operation_unsupported")),
            };
            let input = input.unwrap_or(json!({}));
            validate_input(op, &input)?;
            Ok((op, input))
        }
        Command::ExecutionEnvironment { agent_id } if agent_id == agent => {
            Ok(("environment", json!({})))
        }
        Command::PrepareAgentWorkspace { agent_id } if agent_id == agent => {
            Ok(("prepare_workspace", json!({})))
        }
        Command::Provider { action } => provider_operation("codex".into(), action),
        Command::RuntimeProvider { runtime, action } => provider_operation(runtime, action),
        _ => Err(failure(400, "configuration_operation_unsupported")),
    }
}
fn provider_operation(
    runtime: String,
    action: ProviderAction,
) -> Result<(&'static str, Value), NativeError> {
    if Kind::parse(&runtime).is_none() {
        return Err(failure(400, "invalid_arguments"));
    }
    let operation = match action {
        ProviderAction::Status => "provider_status",
        ProviderAction::Models => "provider_models",
        ProviderAction::UseLocal => "provider_connect",
        _ => return Err(failure(400, "configuration_operation_unsupported")),
    };
    Ok((operation, json!({"runtime":runtime})))
}
fn exact(v: &Value, keys: &[&str]) -> Result<(), NativeError> {
    let o = v.as_object().ok_or_else(invalid)?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        Err(invalid())
    } else {
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelInput {
    profile: ModelProfile,
    #[serde(rename="expectedProfileDigest")]
    expected_profile_digest:String,
}
fn model_profile_shape(value: &Value) -> Result<(), NativeError> {
    if value.get("web_search").is_some() {
        exact(
            value,
            &[
                "model",
                "credential_ref",
                "workspace_root",
                "reasoning_effort",
                "web_search",
            ],
        )
    } else {
        exact(
            value,
            &[
                "model",
                "credential_ref",
                "workspace_root",
                "reasoning_effort",
            ],
        )
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyInput {
    expected_revision: i64,
    policy: Policy,
}
fn validate_input(op: &str, input: &Value) -> Result<(), NativeError> {
    let valid = match op {
        "environment" | "agent_model" | "agent_policy" | "prepare_workspace" => {
            exact(input, &[]).is_ok()
        }
        "save_agent_model" => {
            (exact(input,&["profile","expectedProfileDigest"]).is_ok()
                && input["expectedProfileDigest"].as_str().is_some_and(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))))
                && model_profile_shape(&input["profile"]).is_ok()
                && serde_json::from_value::<ModelInput>(input.clone()).is_ok()
        }
        "save_agent_policy" => {
            serde_json::from_value::<PolicyInput>(input.clone()).is_ok_and(|v| {
                v.expected_revision >= 0
                    && serde_json::to_value(v.policy).is_ok_and(|p| p == input["policy"])
            })
        }
        "provider_status" | "provider_models" | "provider_connect" => {
            exact(input, &["runtime"]).is_ok()
                && input["runtime"].as_str().and_then(Kind::parse).is_some()
        }
        _ => false,
    };
    if valid && serde_json::to_vec(input).is_ok_and(|v| v.len() <= MAX) {
        Ok(())
    } else {
        Err(failure(400, "invalid_arguments"))
    }
}
pub(super) fn is_wrapper(c: &Command) -> bool {
    matches!(
        c,
        Command::AgentConfigurationStatus { .. }
            | Command::ExecutionEnvironment { .. }
            | Command::AgentConfiguration { .. }
    )
}
pub(super) async fn pin(native: &NativeOwner) -> Result<DeviceIdentity, NativeError> {
    native.sync_matrix().await?;
    native.identity().await?;
    native
        .0
        .host
        .console
        .authorized_device()
        .await
        .map(|d| DeviceIdentity::from(&d))
        .map_err(|_| failure(401, "configuration_authority_changed"))
}
async fn api(
    native: &NativeOwner,
    pin: &DeviceIdentity,
    method: &str,
    path: &str,
    input: Option<Value>,
) -> Result<Value, NativeError> {
    let device = native
        .0
        .host
        .console
        .authorized_device()
        .await
        .map_err(|_| failure(401, "configuration_authority_changed"))?;
    if DeviceIdentity::from(&device) != *pin {
        return Err(failure(401, "configuration_authority_changed"));
    }
    let metadata = crate::server_admission::discover(&pin.origin)
        .await
        .map_err(|e| failure(503, e.code()))?;
    if !metadata.capabilities.iter().any(|c| c == CAPABILITY) {
        return Err(failure(409, "configuration_unsupported"));
    }
    // Discovery awaits the network. Recheck the exact SDK/device authority
    // before sending a configuration effect under the captured credential.
    if self::pin(native).await? != *pin {
        return Err(failure(401, "configuration_authority_changed"));
    }
    let url = reqwest::Url::parse(&pin.origin)
        .map_err(|_| invalid())?
        .join(&format!("/hagency/api/v1/{path}"))
        .map_err(|_| invalid())?;
    let client = server_login::http().map_err(|_| failure(503, "server_unavailable"))?;
    let mut request = client
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| invalid())?,
            url,
        )
        .bearer_auth(
            device
                .bearer()
                .map_err(|_| failure(401, "configuration_authority_changed"))?,
        );
    if let Some(input) = input {
        if serde_json::to_vec(&input).map_err(|_| invalid())?.len() > MAX {
            return Err(failure(400, "invalid_arguments"));
        }
        request = request.json(&input);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| failure(503, "server_unavailable"))?;
    let status = response.status().as_u16();
    let mut raw = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failure(503, "server_unavailable"))?
    {
        if raw.len() + chunk.len() > MAX_ENVELOPE {
            return Err(invalid());
        }
        raw.extend_from_slice(&chunk);
    }
    if pin != &self::pin(native).await? {
        return Err(failure(401, "configuration_authority_changed"));
    }
    let value: Value = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    if !(200..300).contains(&status) {
        let code = value["code"]
            .as_str()
            .filter(|s| {
                s.len() <= 80
                    && !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
            })
            .unwrap_or("configuration_request_failed");
        return Err(failure(status, code));
    }
    Ok(value)
}
fn work(value: &Value) -> Result<Work, NativeError> {
    exact(value, &["request"])?;
    let w: Work = serde_json::from_value(value["request"].clone()).map_err(|_| invalid())?;
    key(&w.id)?;
    key(&w.agent_id)?;
    key(&w.execution_device_id)?;
    validate_input(&w.operation, &w.input)?;
    if w.agent_generation < 1
        || w.device_generation < 1
        || !matches!(
            w.state.as_str(),
            "pending" | "claimed" | "succeeded" | "failed" | "unknown" | "expired"
        )
    {
        return Err(invalid());
    }
    if let Some(code) = &w.error_code
        && ![
            "invalid_arguments",
            "invalid_local_policy",
            "local_revision_conflict",
            "configuration_runtime_busy",
            "configuration_model_unavailable",
            "configuration_provider_unavailable",
            "configuration_local_state_unavailable",
            "configuration_authority_changed",
            "configuration_operation_unsupported",
            "configuration_request_expired",
            "configuration_authority_invalidated",
            "configuration_target_changed",
        ]
        .contains(&code.as_str())
    {
        return Err(invalid());
    }
    Ok(w)
}
pub(super) async fn remote(native: &NativeOwner, command: Command) -> Result<Value, NativeError> {
    remote_pinned(native, command, None).await
}
pub(super) async fn remote_pinned(
    native: &NativeOwner,
    command: Command,
    expected: Option<&DeviceIdentity>,
) -> Result<Value, NativeError> {
    let original = pin(native).await?;
    if expected.is_some_and(|expected| expected != &original) {
        return Err(failure(409, "personal_policy_original_device_required"));
    }
    let (agent, generation, key_value, op, input) = match command {
        Command::AgentConfigurationStatus { agent_id } => {
            key(&agent_id)?;
            let result = api(
                native,
                &original,
                "GET",
                &format!("agents/{agent_id}/configuration"),
                None,
            )
            .await?;
            validate_status(&result, &agent_id)?;
            return Ok(result);
        }
        Command::ExecutionEnvironment { agent_id } => {
            key(&agent_id)?;
            let status = api(
                native,
                &original,
                "GET",
                &format!("agents/{agent_id}/configuration"),
                None,
            )
            .await?;
            validate_status(&status, &agent_id)?;
            let generation = status["configuration"]["agentGeneration"]
                .as_i64()
                .ok_or_else(invalid)?;
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| unavailable())?;
            (
                agent_id,
                generation,
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                "environment",
                json!({}),
            )
        }
        Command::AgentConfiguration {
            agent_id,
            expected_generation,
            idempotency_key,
            command,
        } => {
            key(&agent_id)?;
            key(&idempotency_key)?;
            if expected_generation < 1 {
                return Err(failure(400, "invalid_arguments"));
            }
            let (op, input) = operation(*command, &agent_id)?;
            (agent_id, expected_generation, idempotency_key, op, input)
        }
        _ => return Err(failure(400, "configuration_operation_unsupported")),
    };
    let mut current=work(&api(native,&original,"POST",&format!("agents/{agent}/configuration/requests"),Some(json!({"idempotencyKey":key_value,"expectedGeneration":generation,"operation":op,"input":input}))).await?)?;
    let target = current.execution_device_id.clone();
    let target_generation = current.device_generation;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(62);
    loop {
        if current.agent_id != agent
            || current.agent_generation != generation
            || current.execution_device_id != target
            || current.device_generation != target_generation
            || current.operation != op
            || current.input != input
        {
            return Err(invalid());
        }
        match current.state.as_str() {
            "succeeded" => {
                let result = current.result.ok_or_else(invalid)?;
                validate_result(op, &result, &agent)?;
                if op.starts_with("provider_") && result["runtime"] != input["runtime"] {
                    return Err(invalid());
                }
                return Ok(result);
            }
            "failed" => {
                return Err(failure(
                    409,
                    current
                        .error_code
                        .as_deref()
                        .unwrap_or("configuration_request_failed"),
                ));
            }
            "unknown" => return Err(failure(409, "configuration_outcome_unknown")),
            "expired" => return Err(failure(409, "configuration_request_expired")),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failure(409, "configuration_outcome_unknown"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        current = work(
            &api(
                native,
                &original,
                "GET",
                &format!("agents/{agent}/configuration/requests/{}", current.id),
                None,
            )
            .await?,
        )?;
    }
}
fn validate_status(v: &Value, agent: &str) -> Result<(), NativeError> {
    exact(v, &["configuration"])?;
    let c = &v["configuration"];
    exact(
        c,
        &[
            "agentId",
            "agentGeneration",
            "executionDeviceId",
            "deviceGeneration",
            "online",
            "lastSeenAtMs",
            "observedAtMs",
            "cached",
            "environment",
        ],
    )?;
    if c["agentId"] != agent
        || c["agentGeneration"].as_i64().is_none_or(|v| v < 1)
        || !c["online"].is_boolean()
        || !c["cached"].is_boolean()
    {
        return Err(invalid());
    }
    if !c["environment"].is_null() {
        validate_result("environment", &c["environment"], agent)?;
    }
    Ok(())
}
fn text(v: &Value, max: usize) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() <= max && !s.chars().any(char::is_control))
}
fn validate_provider(v: &Value) -> Result<(), NativeError> {
    exact(
        v,
        &[
            "runtime",
            "installed",
            "version",
            "versionSupported",
            "state",
            "authenticated",
            "accountVerified",
            "credentialRef",
            "credentialSource",
            "shared",
            "strictTokenCap",
            "nativeTools",
            "models",
            "defaultModel",
        ],
    )?;
    if v["runtime"].as_str().and_then(Kind::parse).is_none()
        || !matches!(
            v["state"].as_str(),
            Some("ready" | "disconnected" | "unavailable" | "unsupported")
        )
    {
        return Err(invalid());
    }
    for k in [
        "installed",
        "versionSupported",
        "authenticated",
        "accountVerified",
        "shared",
        "strictTokenCap",
        "nativeTools",
    ] {
        if !v[k].is_boolean() {
            return Err(invalid());
        }
    }
    for (k, max) in [
        ("version", 128),
        ("defaultModel", 256),
        ("credentialRef", 256),
    ] {
        if !v[k].is_null() && !text(&v[k], max) {
            return Err(invalid());
        }
    }
    if !text(&v["credentialSource"], 128) {
        return Err(invalid());
    }
    if !v["models"].is_null() {
        let models = v["models"]
            .as_array()
            .filter(|m| m.len() <= 100)
            .ok_or_else(invalid)?;
        for model in models {
            let mut keys = vec![
                "id",
                "model",
                "displayName",
                "description",
                "isDefault",
                "defaultReasoningEffort",
                "supportedReasoningEfforts",
            ];
            if model.get("credentialRef").is_some() {
                keys.push("credentialRef");
            }
            exact(model, &keys)?;
            for k in [
                "id",
                "model",
                "displayName",
                "description",
                "defaultReasoningEffort",
            ] {
                if !text(&model[k], if k == "description" { 2048 } else { 512 }) {
                    return Err(invalid());
                }
            }
            if !model["isDefault"].is_boolean() {
                return Err(invalid());
            }
            if let Some(reference) = model.get("credentialRef")
                && !text(reference, 512)
            {
                return Err(invalid());
            }
            for effort in model["supportedReasoningEfforts"]
                .as_array()
                .filter(|e| e.len() <= 9)
                .ok_or_else(invalid)?
            {
                exact(effort, &["reasoningEffort", "description"])?;
                if !text(&effort["reasoningEffort"], 32) || !text(&effort["description"], 2048) {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
}
fn validate_result(op: &str, v: &Value, agent: &str) -> Result<(), NativeError> {
    if serde_json::to_vec(v).map_err(|_| invalid())?.len() > MAX {
        return Err(invalid());
    }
    match op {
        "environment" => {
            exact(v, &["providers", "configuration"])?;
            let p = v["providers"]
                .as_array()
                .filter(|p| p.len() == 3)
                .ok_or_else(invalid)?;
            let mut runtimes = std::collections::BTreeSet::new();
            for p in p {
                validate_provider(p)?;
                if !runtimes.insert(p["runtime"].as_str().ok_or_else(invalid)?) {
                    return Err(invalid());
                }
            }
            validate_result("agent_model", &v["configuration"], agent)
        }
        "agent_model" | "agent_policy" | "save_agent_model" | "save_agent_policy" => {
            exact(v, &["agentId", "policy", "usage", "modelProfile"])?;
            if v["agentId"] != agent {
                return Err(invalid());
            }
            exact(&v["policy"], &["revision", "policy"])?;
            let _: PolicyVersion =
                serde_json::from_value(v["policy"].clone()).map_err(|_| invalid())?;
            exact(&v["usage"], &["spent", "held"])?;
            for key in ["spent", "held"] {
                if !v["usage"][key].is_u64() {
                    return Err(invalid());
                }
            }
            let typed: PolicyVersion =
                serde_json::from_value(v["policy"].clone()).map_err(|_| invalid())?;
            if serde_json::to_value(typed).map_err(|_| invalid())? != v["policy"] {
                return Err(invalid());
            }
            if !v["modelProfile"].is_null() {
                model_profile_shape(&v["modelProfile"])?;
                let _: ModelProfile =
                    serde_json::from_value(v["modelProfile"].clone()).map_err(|_| invalid())?;
            }
            Ok(())
        }
        "prepare_workspace" => {
            exact(v, &["agentId", "workspace"])?;
            if v["agentId"] != agent || !text(&v["workspace"], 4096) {
                Err(invalid())
            } else {
                Ok(())
            }
        }
        "provider_status" | "provider_models" | "provider_connect" => validate_provider(v),
        _ => Err(invalid()),
    }
}
fn stable_error(error: &NativeError) -> &'static str {
    match error.code.as_str() {
        "invalid_arguments" => "invalid_arguments",
        "invalid_local_policy" => "invalid_local_policy",
        "local_revision_conflict" | "local_policy_conflict" => "local_revision_conflict",
        "configuration_runtime_busy" => "configuration_runtime_busy",
        "configuration_model_unavailable" => "configuration_model_unavailable",
        "configuration_authority_changed" => "configuration_authority_changed",
        _ => "configuration_provider_unavailable",
    }
}
async fn installation(runtime: &str) -> (bool, Option<String>) {
    use tokio::io::AsyncReadExt;
    let Some(binary) =
        Kind::parse(runtime).and_then(|k| owner_provider::external::executable(k).ok())
    else {
        return (false, None);
    };
    let probe = async {
        let mut child = tokio::process::Command::new(binary)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut raw = Vec::new();
        child
            .stdout
            .take()?
            .take(4097)
            .read_to_end(&mut raw)
            .await
            .ok()?;
        if raw.len() > 4096 || !child.wait().await.ok()?.success() {
            return None;
        }
        let line = std::str::from_utf8(&raw).ok()?.lines().next()?;
        let version = line
            .split_ascii_whitespace()
            .find(|part| part.as_bytes().first().is_some_and(u8::is_ascii_digit))?;
        if version.len() > 64
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        {
            return None;
        }
        Some(version.to_owned())
    };
    (
        true,
        tokio::time::timeout(Duration::from_secs(2), probe)
            .await
            .ok()
            .flatten(),
    )
}
async fn provider(
    native: &NativeOwner,
    runtime: &str,
    action: ProviderAction,
) -> Result<Value, NativeError> {
    let raw = native
        .execute_locked(if runtime == "codex" {
            Command::Provider { action }
        } else {
            Command::RuntimeProvider {
                runtime: runtime.into(),
                action,
            }
        })
        .await?;
    let (installed, version) = installation(runtime).await;
    let state = raw["state"]
        .as_str()
        .map(|state| match state {
            "authenticated" | "ready" => "ready",
            "signed_out" | "signing_in" | "disconnected" => "disconnected",
            "unsupported" => "unsupported",
            _ => "unavailable",
        })
        .unwrap_or("unavailable");
    let mut models = raw["models"].clone();
    if let Some(rows) = models.as_array_mut() {
        for row in rows {
            if let Some(map) = row.as_object_mut() {
                map.retain(|k, _| {
                    [
                        "id",
                        "model",
                        "displayName",
                        "description",
                        "isDefault",
                        "defaultReasoningEffort",
                        "supportedReasoningEfforts",
                        "credentialRef",
                    ]
                    .contains(&k.as_str())
                });
            }
        }
    }
    let v = json!({"runtime":runtime,"installed":installed,"version":version,"versionSupported":version.is_some() && matches!(state,"ready"|"disconnected"),"state":state,"authenticated":raw["authenticated"].as_bool().unwrap_or(false),"accountVerified":runtime!="octos"&&raw["accountVerified"].as_bool().unwrap_or_else(||raw["authenticated"].as_bool().unwrap_or(false)),"credentialRef":raw["credentialRef"].as_str().filter(|s|!s.is_empty()),"credentialSource":raw["credentialSource"].as_str().unwrap_or("unavailable"),"shared":raw["shared"].as_bool().unwrap_or(false),"strictTokenCap":raw["strictTokenCap"].as_bool().unwrap_or(false),"nativeTools":raw["nativeTools"].as_bool().unwrap_or(false),"models":models,"defaultModel":raw["defaultModel"].as_str().filter(|s|!s.is_empty())});
    validate_provider(&v)?;
    Ok(v)
}
async fn apply(
    native: &NativeOwner,
    original: &DeviceIdentity,
    w: &Work,
) -> Result<Value, NativeError> {
    let configuration = || Command::Local {
        agent_id: w.agent_id.clone(),
        action: LocalAction::AgentModel,
        input: None,
        binding_id: None,
        requester: None,
    };
    let value = match w.operation.as_str() {
        "environment" => {
            let mut providers = Vec::new();
            for runtime in ["codex", "claude", "octos"] {
                providers.push(match provider(native,runtime,ProviderAction::Models).await{Ok(p)=>p,Err(_)=>{let (installed,version)=installation(runtime).await;json!({"runtime":runtime,"installed":installed,"version":version,"versionSupported":false,"state":"unavailable","authenticated":false,"accountVerified":false,"credentialRef":null,"credentialSource":"unavailable","shared":false,"strictTokenCap":false,"nativeTools":false,"models":null,"defaultModel":null})}});
            }
            json!({"providers":providers,"configuration":native.execute_locked(configuration()).await?})
        }
        "agent_model" | "agent_policy" => native.execute_locked(configuration()).await?,
        "save_agent_model" => {
            let device = native
                .0
                .host
                .console
                .authorized_device()
                .await
                .map_err(|_| failure(401, "configuration_authority_changed"))?;
            if native
                .0
                .host
                .console
                .0
                .owned_runtime
                .agent_busy(&device, &w.agent_id)
                .await
            {
                return Err(failure(409, "configuration_runtime_busy"));
            }
            let input: ModelInput = serde_json::from_value(w.input.clone())
                .map_err(|_| failure(400, "invalid_arguments"))?;
            if input.expected_profile_digest.len()!=64 {return Err(failure(400,"invalid_arguments"));}
            let runtime = hagency_agent_local::runtime::kind(&input.profile.credential_ref).name();
            let catalog = provider(native, runtime, ProviderAction::Models).await?;
            if catalog["authenticated"] != true
                || !catalog["models"].as_array().is_some_and(|models| {
                    models.iter().any(|m| {
                        m["model"] == input.profile.model
                            && m.get("credentialRef").unwrap_or(&catalog["credentialRef"])
                                == &input.profile.credential_ref
                            && m["supportedReasoningEfforts"]
                                .as_array()
                                .is_some_and(|efforts| {
                                    efforts.iter().any(|e| {
                                        e["reasoningEffort"] == input.profile.reasoning_effort
                                    })
                                })
                    })
                })
            {
                return Err(failure(409, "configuration_model_unavailable"));
            }
            fence(native, original, w, false).await?;
            native
                .execute_locked(Command::Local {
                    agent_id: w.agent_id.clone(),
                    action: LocalAction::SaveAgentModel,
                    input: Some(w.input.clone()),
                    binding_id: None,
                    requester: None,
                })
                .await?
        }
        "save_agent_policy" => {
            native
                .execute_locked(Command::Local {
                    agent_id: w.agent_id.clone(),
                    action: LocalAction::SaveAgentPolicy,
                    input: Some(w.input.clone()),
                    binding_id: None,
                    requester: None,
                })
                .await?
        }
        "prepare_workspace" => {
            let prepared = native
                .execute_locked(Command::PrepareAgentWorkspace {
                    agent_id: w.agent_id.clone(),
                })
                .await?;
            json!({"agentId":w.agent_id,"workspace":prepared["workspace"]})
        }
        "provider_status" | "provider_models" | "provider_connect" => {
            if w.operation == "provider_connect" {
                let device = native
                    .0
                    .host
                    .console
                    .authorized_device()
                    .await
                    .map_err(|_| failure(401, "configuration_authority_changed"))?;
                if native
                    .0
                    .host
                    .console
                    .0
                    .owned_runtime
                    .profile_busy(&device)
                    .await
                {
                    return Err(failure(409, "configuration_runtime_busy"));
                }
            }
            provider(
                native,
                w.input["runtime"].as_str().ok_or_else(invalid)?,
                if w.operation == "provider_connect" {
                    ProviderAction::UseLocal
                } else if w.operation == "provider_models" {
                    ProviderAction::Models
                } else {
                    ProviderAction::Status
                },
            )
            .await?
        }
        _ => return Err(failure(400, "configuration_operation_unsupported")),
    };
    validate_result(&w.operation, &value, &w.agent_id)?;
    Ok(value)
}
fn journal(native: &NativeOwner, pin: &DeviceIdentity) -> Result<PathBuf, NativeError> {
    key(&pin.id)?;
    let root = native
        .0
        .host
        .console
        .0
        .server_login
        .state_directory()
        .ok_or_else(unavailable)?;
    let profile = owned_agents::journal_profile_directory(
        &root,
        &pin.origin,
        &pin.issuer,
        &pin.subject,
        &pin.owner,
    )
    .map_err(|_| unavailable())?;
    let dir = profile.join("device-configuration");
    hagency_store::private::directory(&dir).map_err(|_| unavailable())?;
    Ok(dir.join(format!("{}.json", pin.id)))
}
fn load(path: &Path) -> Result<Option<Receipt>, NativeError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
        _ => {}
    }
    let file = hagency_store::private::open(path, false).map_err(|_| unavailable())?;
    let mut raw = Vec::new();
    file.take((MAX_ENVELOPE + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| unavailable())?;
    if raw.len() > MAX_ENVELOPE {
        return Err(unavailable());
    }
    let r: Receipt = serde_json::from_slice(&raw).map_err(|_| unavailable())?;
    if r.version != 1 {
        return Err(unavailable());
    }
    Ok(Some(r))
}
fn save(path: &Path, r: &Receipt) -> Result<(), NativeError> {
    hagency_store::private::replace(path, &serde_json::to_vec(r).map_err(|_| unavailable())?)
        .map_err(|_| unavailable())
}
async fn fence(
    native: &NativeOwner,
    pin: &DeviceIdentity,
    w: &Work,
    terminal: bool,
) -> Result<(), NativeError> {
    if *pin != self::pin(native).await?
        || pin.id != w.execution_device_id
        || pin.generation != w.device_generation as u64
    {
        return Err(failure(401, "configuration_authority_changed"));
    }
    let current = work(
        &api(
            native,
            pin,
            "GET",
            &format!("agents/{}/configuration/requests/{}", w.agent_id, w.id),
            None,
        )
        .await?,
    )?;
    if !(current.state == "claimed"
        || terminal && matches!(current.state.as_str(), "succeeded" | "failed"))
        || current.agent_id != w.agent_id
        || current.agent_generation != w.agent_generation
        || current.execution_device_id != w.execution_device_id
        || current.device_generation != w.device_generation
        || current.operation != w.operation
        || current.input != w.input
    {
        return Err(failure(409, "configuration_authority_changed"));
    }
    let status = api(
        native,
        pin,
        "GET",
        &format!("agents/{}/configuration", w.agent_id),
        None,
    )
    .await?;
    validate_status(&status, &w.agent_id)?;
    let c = &status["configuration"];
    if c["agentGeneration"] != w.agent_generation
        || c["executionDeviceId"] != w.execution_device_id
        || c["deviceGeneration"] != w.device_generation
        || now() >= w.expires_at_ms
    {
        return Err(failure(409, "configuration_authority_changed"));
    }
    Ok(())
}
async fn confirm(
    native: &NativeOwner,
    pin: &DeviceIdentity,
    original: &Work,
    completion: &Value,
) -> Result<(), NativeError> {
    let result = work(
        &api(
            native,
            pin,
            "POST",
            &format!("device-configuration/requests/{}/complete", original.id),
            Some(completion.clone()),
        )
        .await?,
    )?;
    if result.id != original.id
        || result.agent_id != original.agent_id
        || result.agent_generation != original.agent_generation
        || result.execution_device_id != original.execution_device_id
        || result.device_generation != original.device_generation
        || result.operation != original.operation
        || result.input != original.input
        || result.state != completion["state"].as_str().ok_or_else(invalid)?
        || result.result.as_ref().unwrap_or(&Value::Null) != &completion["result"]
        || result.error_code.as_deref() != completion["errorCode"].as_str()
    {
        return Err(invalid());
    }
    Ok(())
}
async fn tick(native: &NativeOwner) -> Result<(), NativeError> {
    let _guard = native.0.calls.lock().await;
    let pin = self::pin(native).await?;
    let path = journal(native, &pin)?;
    if let Some(receipt) = load(&path)? {
        if receipt.pin != pin || now() >= receipt.work.expires_at_ms {
            std::fs::remove_file(&path).map_err(|_| unavailable())?;
        } else {
            if let Some(completion) = receipt.completion {
                fence(native, &pin, &receipt.work, true).await?;
                confirm(native, &pin, &receipt.work, &completion).await?;
                std::fs::remove_file(&path).map_err(|_| unavailable())?;
            }
            return Ok(());
        }
    }
    let reply = api(
        native,
        &pin,
        "POST",
        "device-configuration/poll",
        Some(json!({"acceptWork":true})),
    )
    .await?;
    exact(&reply, &["request"])?;
    if reply["request"].is_null() {
        return Ok(());
    }
    let w = work(&reply)?;
    if w.state != "claimed" {
        return Err(invalid());
    }
    fence(native, &pin, &w, false).await?;
    let mut receipt = Receipt {
        version: 1,
        pin: pin.clone(),
        work: w.clone(),
        completion: None,
    };
    save(&path, &receipt)?;
    let duration = Duration::from_millis(w.expires_at_ms.saturating_sub(now()).max(0) as u64);
    let result = tokio::time::timeout(duration, apply(native, &pin, &w))
        .await
        .map_err(|_| failure(409, "configuration_outcome_unknown"))?;
    let mutation = matches!(
        w.operation.as_str(),
        "save_agent_model" | "save_agent_policy" | "prepare_workspace" | "provider_connect"
    );
    if mutation
        && result.as_ref().err().is_some_and(|e| {
            matches!(
                e.code.as_str(),
                "invalid_configuration_response" | "configuration_outcome_unknown"
            )
        })
    {
        return Err(failure(409, "configuration_outcome_unknown"));
    }
    let mut completion = match result {
        Ok(v) => json!({"state":"succeeded","result":v,"errorCode":null}),
        Err(e) => json!({"state":"failed","result":null,"errorCode":stable_error(&e)}),
    };
    if serde_json::to_vec(&completion)
        .map_err(|_| invalid())?
        .len()
        > MAX
    {
        if mutation {
            return Err(failure(409, "configuration_outcome_unknown"));
        }
        completion = json!({"state":"failed","result":null,"errorCode":"configuration_provider_unavailable"});
    }
    receipt.completion = Some(completion.clone());
    save(&path, &receipt)?;
    fence(native, &pin, &w, true).await?;
    confirm(native, &pin, &w, &completion).await?;
    std::fs::remove_file(&path).map_err(|_| unavailable())?;
    Ok(())
}
pub(super) fn start(native: &NativeOwner) {
    let weak = Arc::downgrade(&native.0);
    let worker = tokio::spawn(async move {
        loop {
            if let Some(inner) = weak.upgrade() {
                if inner
                    .configuration_stopped
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    break;
                }
                let owner = NativeOwner(inner);
                let _ = tick(&owner).await;
            } else {
                break;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
    let weak = Arc::downgrade(&native.0);
    let heartbeat = tokio::spawn(async move {
        loop {
            if let Some(inner) = weak.upgrade() {
                if inner
                    .configuration_stopped
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    break;
                }
                let owner = NativeOwner(inner);
                if let Ok(pin) = pin(&owner).await {
                    let _ = api(
                        &owner,
                        &pin,
                        "POST",
                        "device-configuration/poll",
                        Some(json!({"acceptWork":false})),
                    )
                    .await;
                }
            } else {
                break;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
    native.0.configuration_tasks.lock().unwrap().extend([
        native.0.configuration_custody.track_cancellable(worker),
        native.0.configuration_custody.track_cancellable(heartbeat),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_intents_reject_nested_runtime_login_and_secret_shapes() {
        for command in [
            Command::Logout,
            Command::Provider {
                action: ProviderAction::Login,
            },
            Command::Local {
                agent_id: "a".into(),
                action: LocalAction::RuntimeStart,
                input: None,
                binding_id: None,
                requester: None,
            },
        ] {
            assert!(operation(command, "a").is_err());
        }
        assert!(validate_input("save_agent_model",&json!({"profile":{"model":"sonnet","credential_ref":"ref","workspace_root":"/target","reasoning_effort":"low","token":"secret"}})).is_err());
        assert!(
            validate_input(
                "provider_models",
                &json!({"runtime":"claude","authUrl":"secret"})
            )
            .is_err()
        );
        let provider = json!({"runtime":"octos","installed":true,"version":null,"versionSupported":false,"state":"disconnected","authenticated":false,"accountVerified":false,"credentialRef":null,"credentialSource":"unavailable","shared":false,"strictTokenCap":false,"nativeTools":false,"models":null,"defaultModel":null});
        assert!(validate_provider(&provider).is_ok());
        let mut secret = provider;
        secret["authUrl"] = "secret".into();
        assert!(validate_provider(&secret).is_err());
    }
    #[test]
    fn personal_model_digest_protocol_requires_current_profile_and_rejects_old_shapes() {
        let old=json!({"profile":{"model":"fixture","credential_ref":"keychain:codex:owner","workspace_root":"/workspace","reasoning_effort":"high","web_search":true}});
        assert!(validate_input("save_agent_model",&old).is_err());
        let mut checked=old.clone();checked["expectedProfileDigest"]=json!("a".repeat(64));
        validate_input("save_agent_model",&checked).unwrap();
        for bad in [Value::Null,json!(1),json!("A".repeat(64)),json!("a".repeat(63))] {
            checked["expectedProfileDigest"]=bad;
            assert!(validate_input("save_agent_model",&checked).is_err());
        }
        let mut wrong=old;wrong["expected_profile_digest"]=json!("a".repeat(64));
        assert!(validate_input("save_agent_model",&wrong).is_err());
    }
    #[test]
    fn applying_receipt_survives_restart_without_manufacturing_completion() {
        let dir = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = dir.path().join("original.json");
        let pin = DeviceIdentity {
            origin: "http://127.0.0.1/".into(),
            issuer: "issuer".into(),
            subject: "subject".into(),
            owner: "@o:test".into(),
            user: "uid".into(),
            id: "dev_a".into(),
            generation: 1,
        };
        let work:Work=serde_json::from_value(json!({"id":"cfg_a","agentId":"agent_a","agentGeneration":1,"executionDeviceId":"dev_a","deviceGeneration":1,"operation":"prepare_workspace","input":{},"state":"claimed","createdAtMs":0,"expiresAtMs":now()+60000,"claimedAtMs":1,"completedAtMs":null,"result":null,"errorCode":null})).unwrap();
        let mut receipt = Receipt {
            version: 1,
            pin,
            work,
            completion: None,
        };
        save(&path, &receipt).unwrap();
        assert!(load(&path).unwrap().unwrap().completion.is_none());
        receipt.completion = Some(
            json!({"state":"succeeded","result":{"agentId":"agent_a","workspace":"/original"},"errorCode":null}),
        );
        save(&path, &receipt).unwrap();
        let reopened = load(&path).unwrap().unwrap();
        assert_eq!(reopened.completion, receipt.completion);
        assert_eq!(reopened.work.id, "cfg_a");
    }
}
