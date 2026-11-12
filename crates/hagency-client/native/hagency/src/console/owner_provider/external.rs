//! Owner-scoped binding to provider-managed local Claude/Octos accounts.
//! Reads only public model/profile metadata; never imports account secrets.
use super::{Operation, ProviderError, ProviderPaths, failure, private_directory};
use crate::console::{Console, owned_agents::journal_profile_directory, server_login::OwnerOperation};
use hagency_agent_local::runtime::{Kind, managed_reference};
use serde_json::{Value, json};

use std::path::{Path, PathBuf};

pub(crate) fn executable(kind: Kind) -> Result<PathBuf, ProviderError> {
    if kind == Kind::Codex {
        return super::executable();
    }
    let name = kind.name();
    let variable = format!("HAGENCY_{}_BINARY", name.to_ascii_uppercase());
    let mut candidates = if let Some(path) = std::env::var_os(variable) {
        vec![PathBuf::from(path)]
    } else {
        let mut values = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            for part in [".local/bin", ".cargo/bin"] {
                values.push(PathBuf::from(&home).join(part).join(name));
            }
        }
        for part in ["/opt/homebrew/bin", "/usr/local/bin"] {
            values.push(PathBuf::from(part).join(name));
        }
        if let Some(path) = std::env::var_os("PATH") {
            values.extend(std::env::split_paths(&path).map(|p| p.join(name)));
        }
        values
    };
    for candidate in candidates.drain(..) {
        if candidate.is_absolute()
            && let Ok(canonical) = candidate.canonicalize()
            && canonical.is_file()
        {
            if kind == Kind::Octos {
                return native_octos(&canonical).ok_or(failure(503, "octos_binary_unavailable"));
            }
            return Ok(canonical);
        }
    }
    Err(failure(
        503,
        match kind {
            Kind::Claude => "claude_binary_unavailable",
            Kind::Octos => "octos_binary_unavailable",
            _ => "codex_binary_unavailable",
        },
    ))
}
// Homebrew/npm launchers must resolve to the supervised native process.
fn native_octos(path: &Path) -> Option<PathBuf> {
    fn script(path: &Path) -> Option<bool> {
        use std::io::Read;
        let mut prefix = [0u8; 2];
        std::fs::File::open(path)
            .ok()?
            .read_exact(&mut prefix)
            .ok()?;
        Some(prefix == *b"#!")
    }
    if !script(path)? {
        return Some(path.to_owned());
    }
    let bin = path.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    ["libexec", "vendor"].iter().find_map(|folder| {
        let binary = bin
            .parent()?
            .join(folder)
            .join("octos")
            .canonicalize()
            .ok()?;
        (binary.is_file() && !script(&binary)?).then_some(binary)
    })
}
fn homes(kind: Kind) -> Result<(PathBuf, PathBuf), ProviderError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .ok_or(failure(503, "local_provider_home_unavailable"))?;
    let folder = match kind {
        Kind::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude")),
        Kind::Octos => std::env::var_os("OCTOS_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".octos")),
        _ => return Err(failure(400, "invalid_arguments")),
    };
    let folder = folder
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
        .ok_or(failure(503, "local_provider_home_unavailable"))?;
    Ok((home, folder))
}
fn reference(kind: Kind, home: &Path, profile: Option<&str>) -> Result<String, ProviderError> {
    let primary = if let Some(id) = profile {
        let file = home.join("profiles").join(format!("{id}.json"));
        if !std::fs::symlink_metadata(&file)
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            return Err(failure(409, "octos_profiles_unavailable"));
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(file)
            .map_err(|_| failure(409, "octos_profiles_unavailable"))?
            .take(hagency_runtime::octos::MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure(409, "octos_profiles_unavailable"))?;
        Some(
            hagency_runtime::octos::profile_model(id, &bytes)
                .ok_or(failure(409, "octos_profiles_unavailable"))?,
        )
    } else {
        None
    };
    Ok(hagency_agent_local::runtime::local_reference(
        kind,
        home,
        profile.zip(primary.as_ref()),
    ))
}
fn directory(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<PathBuf, ProviderError> {
    let directory = journal_profile_directory(root, origin, issuer, subject, owner)
        .map_err(|_| failure(503, "provider_directory_unavailable"))?;
    private_directory(&directory)
}
fn consent_path(directory: &Path, kind: Kind) -> PathBuf {
    directory.join(format!("{}-connection.json", kind.name()))
}
fn connected(directory: &Path, kind: Kind, home: &Path) -> Result<bool, ProviderError> {
    let path = consent_path(directory, kind);
    if !path.exists() {
        return Ok(false);
    }
    use std::io::Read;
    let file = hagency_store::private::open(&path, false)
        .map_err(|_| failure(503, "provider_choice_unavailable"))?;
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| failure(503, "provider_choice_unavailable"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| failure(503, "provider_choice_unavailable"))?;
    Ok(value == json!({"version":1,"runtime":kind.name(),"home":home}))
}
pub(crate) fn paths_for_reference(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
    selected: &str,
) -> Result<ProviderPaths, ProviderError> {
    let Some((kind, profile)) = managed_reference(selected) else {
        return super::paths(root, origin, issuer, subject, owner);
    };
    let directory = directory(root, origin, issuer, subject, owner)?;
    let (home, folder) = homes(kind)?;
    if !connected(&directory, kind, &folder)? || reference(kind, &folder, profile)? != selected {
        return Err(failure(409, "provider_profile_mismatch"));
    }
    Ok(ProviderPaths {
        home,
        codex_home: folder,
        credential_ref: selected.into(),
        shared: true,
    })
}
/// What "use my local account" records, for tests of the code that reads it.
#[cfg(test)]
pub(crate) fn record_connection(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
    kind: Kind,
) -> Result<(), ProviderError> {
    let directory = directory(root, origin, issuer, subject, owner)?;
    let (_, folder) = homes(kind)?;
    hagency_store::private::replace(
        &consent_path(&directory, kind),
        &serde_json::to_vec(&json!({"version":1,"runtime":kind.name(),"home":folder}))
            .map_err(|_| failure(503, "provider_choice_unavailable"))?,
    )
    .map_err(|_| failure(503, "provider_choice_unavailable"))
}
/// The original owner-selected local provider, without accepting a path or
/// credential reference from a resource command.
pub(in crate::console) fn connected_paths(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
    kind: Kind,
) -> Result<ProviderPaths, ProviderError> {
    let directory = directory(root, origin, issuer, subject, owner)?;
    let (home, folder) = homes(kind)?;
    if !connected(&directory, kind, &folder)? {
        return Err(failure(409, "provider_profile_mismatch"));
    }
    Ok(ProviderPaths {
        home,
        credential_ref: reference(kind, &folder, None)?,
        codex_home: folder,
        shared: true,
    })
}
pub(crate) fn verify_reference(home: &Path, selected: &str) -> Result<(), ProviderError> {
    let (kind, profile) =
        managed_reference(selected).ok_or(failure(409, "provider_profile_mismatch"))?;
    let (_, current) = homes(kind)?;
    if current != home || reference(kind, home, profile)? != selected {
        return Err(failure(409, "provider_profile_mismatch"));
    }
    Ok(())
}
fn model(
    id: &str,
    name: &str,
    label: &str,
    description: &str,
    reference: &str,
    efforts: &[&str],
) -> Value {
    json!({"id":id,"model":name,"displayName":label,"description":description,"isDefault":false,"credentialRef":reference,
        "defaultReasoningEffort":efforts[0],"supportedReasoningEfforts":efforts.iter().map(|e|json!({"reasoningEffort":e,"description":""})).collect::<Vec<_>>()})
}
pub(crate) fn octos_models(home: &Path) -> Result<Vec<Value>, ProviderError> {
    let entries = std::fs::read_dir(home.join("profiles"))
        .map_err(|_| failure(409, "octos_profiles_unavailable"))?;
    let mut models = Vec::new();
    for entry in entries.take(1024) {
        let entry = entry.map_err(|_| failure(503, "octos_profiles_unavailable"))?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|id| hagency_runtime::octos::profile_id(id))
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())
            .map_err(|_| failure(503, "octos_profiles_unavailable"))?
            .take(hagency_runtime::octos::MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure(503, "octos_profiles_unavailable"))?;
        if let Some(primary) = hagency_runtime::octos::profile_model(id, &bytes) {
            let mut row = model(
                id,
                &primary.model,
                &format!("{id} · {}", primary.model),
                &primary.family,
                &hagency_agent_local::runtime::local_reference(
                    Kind::Octos,
                    home,
                    Some((id, &primary)),
                ),
                &["none"],
            );
            row["profileId"] = id.into();
            models.push(row);
        }
        if models.len() >= 64 {
            break;
        }
    }
    models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    if let Some(first) = models.first_mut() {
        first["isDefault"] = true.into();
    }
    Ok(models)
}
pub(crate) async fn call(
    console: &Console,
    cookie: &str,
    runtime: &str,
    operation: Operation,
) -> Result<Value, ProviderError> {
    let tasks = &console.0.owner_provider.tasks;
    let _activity = tasks
        .enter()
        .map_err(|_| failure(409, "provider_closing"))?;
    let kind = Kind::parse(runtime)
        .filter(|k| *k != Kind::Codex)
        .ok_or(failure(400, "invalid_arguments"))?;
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
        .ok_or(failure(503, "local_state_unavailable"))?;
    let directory = directory(
        &root,
        &reply.origin,
        &reply.issuer,
        &reply.subject,
        &reply.owner,
    )?;
    if matches!(operation, Operation::Disconnect) {
        console
            .0
            .owned_runtime
            .stop_profile(&device)
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let path = consent_path(&directory, kind);
        if path.exists() {
            std::fs::remove_file(path).map_err(|_| failure(503, "provider_choice_unavailable"))?;
        }
        device
            .bearer()
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        return Ok(
            json!({"runtime":runtime,"state":"disconnected","authenticated":false,"credentialRef":"","models":[],"authUrl":null,"shared":true}),
        );
    }
    let (home, folder) = homes(kind)?;
    let binary = executable(kind)?;
    let selected = reference(kind, &folder, None)?;
    let models = match kind {
        Kind::Claude => vec![
            model(
                "sonnet",
                "sonnet",
                "Claude Sonnet",
                "Claude Code local model alias",
                &selected,
                &["medium", "low", "high"],
            ),
            model(
                "opus",
                "opus",
                "Claude Opus",
                "Claude Code local model alias",
                &selected,
                &["medium", "low", "high", "max"],
            ),
            model(
                "haiku",
                "haiku",
                "Claude Haiku",
                "Claude Code local model alias",
                &selected,
                &["low", "medium", "high"],
            ),
        ],
        Kind::Octos => octos_models(&folder)?,
        _ => unreachable!(),
    };
    let signed_in = match kind {
        Kind::Claude => {
            let observation = tasks
                .checked()
                .map_err(|_| failure(409, "provider_closing"))?;
            let result = hagency_agent_local::runtime::claude_account(&binary, &home, &folder)
                .await
                .map_err(|_| failure(503, "claude_account_unavailable"))?;
            observation.complete();
            result
        }
        Kind::Octos => !models.is_empty(),
        _ => false,
    };
    match operation {
        Operation::Status | Operation::Models => {}
        Operation::UseLocal => {
            if !signed_in {
                return Err(failure(409, "local_provider_not_signed_in"));
            }
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            hagency_store::private::replace(
                &consent_path(&directory, kind),
                &serde_json::to_vec(&json!({"version":1,"runtime":runtime,"home":folder}))
                    .map_err(|_| failure(503, "provider_choice_unavailable"))?,
            )
            .map_err(|_| failure(503, "provider_choice_unavailable"))?;
        }
        Operation::Disconnect => {
            console
                .0
                .owned_runtime
                .stop_profile(&device)
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            let path = consent_path(&directory, kind);
            if path.exists() {
                std::fs::remove_file(path)
                    .map_err(|_| failure(503, "provider_choice_unavailable"))?;
            }
        }
        _ => return Err(failure(409, "local_provider_login_managed_locally")),
    }
    device
        .bearer()
        .map_err(|_| failure(401, "owner_authorization_required"))?;
    let authenticated = signed_in && connected(&directory, kind, &folder)?;
    let default = models
        .first()
        .and_then(|m| m["model"].as_str())
        .unwrap_or("");
    Ok(
        json!({"runtime":runtime,"state":if authenticated{"ready"}else{"disconnected"},"authenticated":authenticated,"credentialRef":selected,"credentialSource":format!("local_{runtime}"),"shared":true,"strictTokenCap":false,"nativeTools":true,"models":models,"defaultModel":default,"authUrl":null,"accountVerified":kind==Kind::Claude}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_octos_launchers_resolve_to_native_binary() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        std::fs::create_dir(temp.path().join("bin")).unwrap();
        std::fs::create_dir(temp.path().join("vendor")).unwrap();
        let wrapper = temp.path().join("bin/octos.js");
        let binary = temp.path().join("vendor/octos");
        std::fs::write(&wrapper, b"#!/usr/bin/env node").unwrap();
        assert!(native_octos(&wrapper).is_none());
        std::fs::write(&binary, b"\x7fELF native").unwrap();
        assert_eq!(native_octos(&wrapper), Some(binary.canonicalize().unwrap()));
    }
    #[test]
    fn profile_catalog_keeps_public_metadata_and_distinct_profile_references() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let folder = temp.path().join("profiles");
        std::fs::create_dir(&folder).unwrap();
        for id in ["coding", "review"] {
            std::fs::write(folder.join(format!("{id}.json")),json!({"id":id,"config":{"llm":{"primary":{"family_id":"test","model_id":"same","api_key":"secret-not-exported"}}}}).to_string()).unwrap();
        }
        let models = octos_models(temp.path()).unwrap();
        assert_eq!(models.len(), 2);
        assert_ne!(models[0]["credentialRef"], models[1]["credentialRef"]);
        assert!(
            !serde_json::to_string(&models)
                .unwrap()
                .contains("secret-not-exported")
        );
    }
}
