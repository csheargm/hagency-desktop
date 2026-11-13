//! A local Octos profile is a billing configuration, not a verified provider
//! account. This witness never exports the profile contents or credentials.
use super::ProfileModel;
#[cfg(unix)]
use super::{MAX_PROFILE_BYTES, profile_id, profile_model, profile_models};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::io::{Read, Write};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
pub(crate) mod auth;

/// Resolve the local owner's Octos auth directory using the same environment
/// precedence as their personal Octos process. This function does not open it.
pub fn auth_home_for_owner(home: &Path) -> Result<PathBuf, Error> {
    #[cfg(target_os = "macos")]
    { auth::source_from_environment(home) }
    #[cfg(not(target_os = "macos"))]
    { let _ = home; Err(Error::Unavailable) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Octos profile is unavailable or has changed")]
    Unavailable,
    #[error("invalid Octos profile binding")]
    Invalid,
}

/// Enumerate only valid profile file names through no-follow directory handles.
/// The bound rejects an oversized directory rather than returning a partial
/// inventory that could hide a separately budgeted configuration.
#[cfg(unix)]
pub fn profile_ids(home: &Path) -> Result<Vec<String>, Error> {
    use rustix::fs::{Dir, Mode, OFlags};
    if !home.is_absolute() || home.canonicalize().ok().as_deref() != Some(home) {
        return Err(Error::Invalid);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let home_fd = rustix::fs::open(home, flags, Mode::empty()).map_err(|_| Error::Unavailable)?;
    let profiles_fd = rustix::fs::openat(&home_fd, "profiles", flags, Mode::empty())
        .map_err(|_| Error::Unavailable)?;
    let directory = Dir::read_from(&profiles_fd).map_err(|_| Error::Unavailable)?;
    let mut ids = Vec::new();
    for (index, entry) in directory.enumerate() {
        if index >= 1024 {
            return Err(Error::Unavailable);
        }
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .ok()
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if profile_id(id) {
            ids.push(id.to_owned());
            if ids.len() > 64 {
                return Err(Error::Unavailable);
            }
        }
    }
    ids.sort();
    Ok(ids)
}

#[cfg(not(unix))]
pub fn profile_ids(_home: &Path) -> Result<Vec<String>, Error> {
    Err(Error::Unavailable)
}

/// Private, short-lived observation. Neither path, provider secret, profile
/// contents nor the configuration checksum is serializable or sent remotely.
pub struct ProfileBinding {
    home: PathBuf,
    id: String,
    model: ProfileModel,
    checksum: [u8; 32],
    expires: Instant,
}

/// Change detector for an already admitted process. This guard cannot create
/// a process or authorize new work; those operations still require a fresh
/// short-lived ProfileBinding and the current shared activity lease.
pub struct RunningProfileGuard {
    home: PathBuf,
    id: String,
    model: ProfileModel,
    checksum: [u8; 32],
    #[cfg(target_os = "macos")]
    auth: Option<auth::Selection>,
}
impl RunningProfileGuard {
    pub fn check(&self) -> Result<(), Error> {
        let (model, checksum) = read(&self.home, &self.id, Some(&self.model.model))?;
        if model != self.model || checksum != self.checksum {
            return Err(Error::Unavailable);
        }
        #[cfg(target_os = "macos")]
        if let Some(auth) = &self.auth {
            auth.check()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn bounded_file(file: std::fs::File) -> Result<Vec<u8>, Error> {
    let metadata = file.metadata().map_err(|_| Error::Unavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES {
        return Err(Error::Unavailable);
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 > MAX_PROFILE_BYTES {
        return Err(Error::Unavailable);
    }
    Ok(bytes)
}

#[cfg(unix)]
struct Captured {
    model: ProfileModel,
    checksum: [u8; 32],
    profile: Vec<u8>,
    defaults: Option<Vec<u8>>,
}

/// Resolve only markers actually present in this profile's env map. The
/// result remains in the private ephemeral process snapshot; no secret is
/// returned in an inventory, process argument, log, or server request.
#[cfg(target_os = "macos")]
fn resolved_snapshot(
    bytes: &[u8],
    nested: bool,
    mut lookup: impl FnMut(&str) -> Result<String, Error>,
) -> Result<Vec<u8>, Error> {
    let mut document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)?;
    let owner = if nested { document.get_mut("config") } else { Some(&mut document) };
    let Some(vars) = owner.and_then(|v| v.get_mut("env_vars")).and_then(serde_json::Value::as_object_mut) else {
        return Ok(bytes.to_vec());
    };
    let mut changed = false;
    for (name, value) in vars {
        let Some(marker) = value.as_str().and_then(|v| v.strip_prefix("keychain:")) else { continue; };
        let account = if marker.is_empty() { name.as_str() } else { marker };
        if account.is_empty() || account.starts_with('-') || account.len() > 255
            || account.chars().any(char::is_control) { return Err(Error::Unavailable); }
        // Octos treats a missing/locked Keychain entry as unavailable and
        // continues to the next credential candidate. Keep an empty profile
        // override so a missing entry does not reveal an inherited value.
        let secret = lookup(account).unwrap_or_default();
        if secret.len() > 64 * 1024 { return Err(Error::Unavailable); }
        *value = secret.into();
        changed = true;
    }
    if changed { serde_json::to_vec(&document).map_err(|_| Error::Unavailable) } else { Ok(bytes.to_vec()) }
}

/// Personal Octos falls back to its launch environment after trying profile
/// and inherited env vars. Copy only the selected model's first usable key,
/// into the ephemeral profile rather than the process or shell environment.
#[cfg(target_os = "macos")]
fn with_ambient_credential(
    profile: &[u8], defaults: Option<&[u8]>, family: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<Vec<u8>, Error> {
    let mut value: serde_json::Value = serde_json::from_slice(profile).map_err(|_| Error::Unavailable)?;
    let defaults: serde_json::Value = defaults.map(serde_json::from_slice).transpose().map_err(|_| Error::Unavailable)?.unwrap_or(serde_json::Value::Null);
    let custom = value.pointer("/config/llm/primary/route/api_key_env").and_then(serde_json::Value::as_str);
    let candidates = auth::env_candidates(family, custom);
    if candidates.iter().any(|name| name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) || name.contains('=')) {
        return Err(Error::Unavailable);
    }
    let profile_vars = value.pointer("/config/env_vars");
    let default_vars = defaults.get("env_vars");
    // Octos checks every configured candidate before any process variable.
    if candidates.iter().any(|name| {
        let current = profile_vars.and_then(|vars| vars.get(name)).or_else(|| default_vars.and_then(|vars| vars.get(name)));
        current.and_then(serde_json::Value::as_str).is_some_and(|v| !v.is_empty())
    }) { return Ok(profile.to_vec()); }
    for name in candidates {
        let Some(secret) = lookup(&name).filter(|v| !v.is_empty()) else { continue; };
        if secret.len() > 64 * 1024 { return Err(Error::Unavailable); }
        let config = value.get_mut("config").and_then(serde_json::Value::as_object_mut).ok_or(Error::Unavailable)?;
        let vars = config.entry("env_vars").or_insert_with(|| serde_json::json!({})).as_object_mut().ok_or(Error::Unavailable)?;
        vars.insert(name, secret.into());
        return serde_json::to_vec(&value).map_err(|_| Error::Unavailable);
    }
    Ok(profile.to_vec())
}

#[cfg(target_os = "macos")]
fn keychain_secret(account: &str) -> Result<String, Error> {
    use std::{process::{Command, Stdio}, thread};
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "octos", "-a", account, "-w"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|_| Error::Unavailable)?;
    let mut stdout = child.stdout.take().ok_or(Error::Unavailable)?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut too_large = false;
        let result = (|| {
            loop {
                let count = stdout.read(&mut chunk)?;
                if count == 0 { break; }
                let keep = count.min((64 * 1024usize).saturating_sub(output.len()));
                output.extend_from_slice(&chunk[..keep]);
                too_large |= keep != count;
            }
            Ok::<_, std::io::Error>((output, too_large))
        })();
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => { let _ = child.kill(); let _ = child.wait(); break None; }
        }
    };
    let (bytes, too_large) = receiver.recv_timeout(Duration::from_secs(1))
        .map_err(|_| Error::Unavailable)?.map_err(|_| Error::Unavailable)?;
    if !status.is_some_and(|s| s.success()) || too_large { return Err(Error::Unavailable); }
    let value = String::from_utf8(bytes).map_err(|_| Error::Unavailable)?.trim().to_owned();
    // Octos's security CLI uses a hexadecimal form for multi-line secrets.
    if value.len() >= 2 && value.len() % 2 == 0 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        let decoded = (0..value.len()).step_by(2).map(|i| u8::from_str_radix(&value[i..i+2], 16)).collect::<Result<Vec<_>, _>>();
        if let Ok(decoded) = decoded && let Ok(text) = String::from_utf8(decoded) && text.contains('\n') {
            return Ok(text);
        }
    }
    if value.is_empty() { Err(Error::Unavailable) } else { Ok(value) }
}

#[cfg(unix)]
fn capture(home: &Path, id: &str) -> Result<Captured, Error> {
    use rustix::fs::{Mode, OFlags};
    if !home.is_absolute() || home.canonicalize().ok().as_deref() != Some(home) || !profile_id(id) {
        return Err(Error::Invalid);
    }
    let profiles = home.join("profiles");
    if profiles.canonicalize().ok().as_ref() != Some(&profiles)
        || !std::fs::symlink_metadata(&profiles)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    {
        return Err(Error::Unavailable);
    }
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let home_fd =
        rustix::fs::open(home, directory_flags, Mode::empty()).map_err(|_| Error::Unavailable)?;
    let profiles_fd = rustix::fs::openat(&home_fd, "profiles", directory_flags, Mode::empty())
        .map_err(|_| Error::Unavailable)?;
    let file = rustix::fs::openat(
        &profiles_fd,
        format!("{id}.json"),
        file_flags,
        Mode::empty(),
    )
    .map_err(|_| Error::Unavailable)?;
    let bytes = bounded_file(std::fs::File::from(file))?;
    let model = profile_model(id, &bytes).ok_or(Error::Unavailable)?;
    // Octos merges this global layer into every profile, including inherited
    // credentials and runtime policy. Its creation, deletion or modification
    // invalidates a witness too; the stable billing identity stays unchanged.
    let defaults =
        match rustix::fs::openat(&home_fd, "profile-defaults.json", file_flags, Mode::empty()) {
            Ok(fd) => Some(bounded_file(std::fs::File::from(fd))?),
            Err(rustix::io::Errno::NOENT) => None,
            Err(_) => return Err(Error::Unavailable),
        };
    let mut hash = Sha256::new();
    hash.update(b"hagency-octos-profile-and-defaults-v1\0");
    hash.update(Sha256::digest(&bytes));
    if let Some(defaults) = &defaults {
        hash.update([1]);
        hash.update(Sha256::digest(defaults));
    } else {
        hash.update([0]);
    }
    Ok(Captured {
        model,
        checksum: hash.finalize().into(),
        profile: bytes,
        defaults,
    })
}

#[cfg(unix)]
fn read(home: &Path, id: &str, selected: Option<&str>) -> Result<(ProfileModel, [u8; 32]), Error> {
    let captured = capture(home, id)?;
    let model = match selected {
        None => captured.model,
        Some(name) => profile_models(id, &captured.profile).ok_or(Error::Unavailable)?
            .into_iter().find(|model| model.model == name).ok_or(Error::Unavailable)?,
    };
    Ok((model, captured.checksum))
}

// Do not read through a path that can be replaced between inspection and
// opening. Other platforms need an equivalent no-follow handle first.
#[cfg(not(unix))]
fn read(_home: &Path, _id: &str, _selected: Option<&str>) -> Result<(ProfileModel, [u8; 32]), Error> {
    Err(Error::Unavailable)
}

/// Change only the ephemeral copy. Disabling automatic fallback makes the
/// published model an exact execution promise even if the original profile
/// has alternatives on other providers or endpoints.
#[cfg(unix)]
fn selected_snapshot(bytes: &[u8], id: &str, selected: &ProfileModel) -> Result<Vec<u8>, Error> {
    let choices = profile_models(id, bytes).ok_or(Error::Unavailable)?;
    if !choices.contains(selected) { return Err(Error::Unavailable); }
    let mut document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)?;
    let llm = document.pointer_mut("/config/llm").and_then(serde_json::Value::as_object_mut).ok_or(Error::Unavailable)?;
    let is_primary = choices.first() == Some(selected);
    let has_fallbacks = llm.get("fallbacks").and_then(serde_json::Value::as_array).is_some_and(|v| !v.is_empty());
    if is_primary && !has_fallbacks { return Ok(bytes.to_vec()); }
    if !is_primary {
        let fallbacks = llm.get_mut("fallbacks").and_then(serde_json::Value::as_array_mut).ok_or(Error::Unavailable)?;
        let index = fallbacks.iter().position(|entry|
            entry.get("family_id").and_then(serde_json::Value::as_str) == Some(selected.family.as_str())
                && entry.get("model_id").and_then(serde_json::Value::as_str) == Some(selected.model.as_str()))
            .ok_or(Error::Unavailable)?;
        let chosen = fallbacks.remove(index);
        llm.insert("primary".into(), chosen);
    }
    llm.insert("fallbacks".into(), serde_json::json!([]));
    serde_json::to_vec(&document).map_err(|_| Error::Unavailable)
}

impl ProfileBinding {
    pub fn observe(home: &Path, id: &str, expires: Instant) -> Result<Self, Error> {
        Self::observe_selection(home, id, None, expires)
    }
    /// Bind one explicitly configured, unambiguous model to the same local
    /// profile budget. No path, credential or remote model claim is accepted.
    pub fn observe_model(home: &Path, id: &str, model: &str, expires: Instant) -> Result<Self, Error> {
        Self::observe_selection(home, id, Some(model), expires)
    }
    fn observe_selection(home: &Path, id: &str, selected: Option<&str>, expires: Instant) -> Result<Self, Error> {
        let now = Instant::now();
        if expires <= now || expires > now + Duration::from_secs(60) {
            return Err(Error::Invalid);
        }
        let (model, checksum) = read(home, id, selected)?;
        Ok(Self {
            home: home.into(),
            id: id.into(),
            model,
            checksum,
            expires,
        })
    }
    pub fn profile_id(&self) -> &str {
        &self.id
    }
    pub fn model(&self) -> &ProfileModel {
        &self.model
    }
    pub fn is_primary(&self) -> bool {
        self.check().is_ok() && read(&self.home, &self.id, None)
            .is_ok_and(|(model, checksum)| model == self.model && checksum == self.checksum)
    }
    /// Only models configured on this exact profile. The check is repeated
    /// after the read so a concurrent edit cannot authorize a stale choice.
    #[cfg(unix)]
    pub fn models(&self) -> Result<Vec<ProfileModel>, Error> {
        self.check()?;
        let captured = capture(&self.home, &self.id)?;
        if captured.checksum != self.checksum { return Err(Error::Unavailable); }
        let models = profile_models(&self.id, &captured.profile).ok_or(Error::Unavailable)?;
        self.check()?;
        Ok(models)
    }
    #[cfg(not(unix))]
    pub fn models(&self) -> Result<Vec<ProfileModel>, Error> { Err(Error::Unavailable) }
    pub fn expires(&self) -> Instant {
        self.expires
    }

    pub fn running_guard(&self) -> Result<RunningProfileGuard, Error> {
        self.check()?;
        Ok(RunningProfileGuard {
            home: self.home.clone(),
            id: self.id.clone(),
            model: self.model.clone(),
            checksum: self.checksum,
            #[cfg(target_os = "macos")]
            auth: None,
        })
    }

    /// Bind an already selected provider login to this process. The auth
    /// record is never part of the resource identity, but a change revokes the
    /// running guard so an old login cannot remain in use after rotation.
    #[cfg(target_os = "macos")]
    pub(crate) fn running_guard_with_auth(
        &self,
        selection: Option<auth::Selection>,
    ) -> Result<RunningProfileGuard, Error> {
        let mut guard = self.running_guard()?;
        guard.auth = selection;
        guard.check()?;
        Ok(guard)
    }

    /// Copy just this bound profile and its inherited defaults into an empty,
    /// host-owned ephemeral directory. The returned files contain provider
    /// credentials: the caller must confine the process and destroy this
    /// directory instead of including it in a persistent session archive.
    /// No sibling profile, symbolic link, or caller-specified source is read.
    #[cfg(unix)]
    pub fn snapshot_into(&self, destination: &Path) -> Result<(), Error> {
        self.snapshot_into_with_ambient(destination, false)
    }

    #[cfg(unix)]
    pub(crate) fn snapshot_into_with_ambient(&self, destination: &Path, ambient: bool) -> Result<(), Error> {
        use rustix::fs::{AtFlags, Dir, Mode, OFlags};
        if Instant::now() >= self.expires
            || !destination.is_absolute()
            || destination.canonicalize().ok().as_deref() != Some(destination)
            || destination == self.home
        {
            return Err(Error::Invalid);
        }
        let captured = capture(&self.home, &self.id)?;
        if captured.checksum != self.checksum || !profile_models(&self.id, &captured.profile)
            .is_some_and(|models| models.contains(&self.model)) {
            return Err(Error::Unavailable);
        }
        #[cfg(target_os = "macos")]
        let mut profile_bytes = resolved_snapshot(&captured.profile, true, keychain_secret)?;
        #[cfg(not(target_os = "macos"))]
        let mut profile_bytes = captured.profile.clone();
        #[cfg(target_os = "macos")]
        let defaults_bytes = captured.defaults.as_deref().map(|v| resolved_snapshot(v, false, keychain_secret)).transpose()?;
        #[cfg(not(target_os = "macos"))]
        let defaults_bytes = captured.defaults.clone();
        profile_bytes = selected_snapshot(&profile_bytes, &self.id, &self.model)?;
        #[cfg(target_os = "macos")]
        if ambient {
            profile_bytes = with_ambient_credential(&profile_bytes, defaults_bytes.as_deref(), &self.model.family,
                |name| std::env::var(name).ok())?;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = ambient;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let root =
            rustix::fs::open(destination, flags, Mode::empty()).map_err(|_| Error::Unavailable)?;
        for entry in Dir::read_from(&root).map_err(|_| Error::Unavailable)? {
            let entry = entry.map_err(|_| Error::Unavailable)?;
            if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
                return Err(Error::Unavailable);
            }
        }
        // The destination belongs to the already-exclusive workspace. Refuse
        // a stale or attacker-created entry instead of overwriting it.
        rustix::fs::mkdirat(&root, "profiles", Mode::RUSR | Mode::WUSR | Mode::XUSR)
            .map_err(|_| Error::Unavailable)?;
        let profiles = rustix::fs::openat(&root, "profiles", flags, Mode::empty())
            .map_err(|_| Error::Unavailable)?;
        let write_new = |parent: &std::os::fd::OwnedFd,
                         name: &str,
                         bytes: &[u8],
                         created: &mut bool|
         -> Result<(), Error> {
            let file = rustix::fs::openat(
                parent,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| Error::Unavailable)?;
            *created = true;
            let mut file = std::fs::File::from(file);
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| Error::Unavailable)
        };
        let profile_name = format!("{}.json", self.id);
        let (mut defaults_created, mut profile_created) = (false, false);
        let result = (|| {
            if let Some(defaults) = &defaults_bytes {
                write_new(
                    &root,
                    "profile-defaults.json",
                    defaults,
                    &mut defaults_created,
                )?;
            }
            write_new(
                &profiles,
                &profile_name,
                &profile_bytes,
                &mut profile_created,
            )?;
            // A concurrent owner edit cannot authorize an already-stale copy.
            self.check()
        })();
        if result.is_err() {
            if profile_created {
                let _ = rustix::fs::unlinkat(&profiles, &profile_name, AtFlags::empty());
            }
            if defaults_created {
                let _ = rustix::fs::unlinkat(&root, "profile-defaults.json", AtFlags::empty());
            }
            let _ = rustix::fs::unlinkat(&root, "profiles", AtFlags::REMOVEDIR);
        }
        result
    }

    /// Stable per installation/profile; model or credential changes must not
    /// silently create a fresh budget. The installation key remains local.
    pub fn accounting_key(&self, installation_key: &[u8; 32]) -> Result<String, Error> {
        self.check()?;
        if installation_key.iter().all(|byte| *byte == 0) {
            return Err(Error::Invalid);
        }
        let mut hash = Sha256::new();
        hash.update(b"hagency-octos-profile-account-v1\0");
        hash.update(installation_key);
        let home = self.home.to_str().ok_or(Error::Invalid)?;
        hash.update((home.len() as u64).to_be_bytes());
        hash.update(home.as_bytes());
        hash.update(self.id.as_bytes());
        Ok(format!("{:x}", hash.finalize()))
    }
    /// A private version commitment: it changes if any configuration byte
    /// changes, without publishing the configuration or an unkeyed checksum.
    pub fn configuration_key(&self, installation_key: &[u8; 32]) -> Result<String, Error> {
        self.check()?;
        if installation_key.iter().all(|byte| *byte == 0) {
            return Err(Error::Invalid);
        }
        let mut hash = Sha256::new();
        hash.update(b"hagency-octos-profile-version-v1\0");
        hash.update(installation_key);
        hash.update(self.checksum);
        Ok(format!("{:x}", hash.finalize()))
    }
    /// Bind the process to the chosen model as well as the profile revision;
    /// the accounting key intentionally remains shared by all its models.
    pub fn selected_model_key(&self, installation_key: &[u8; 32]) -> Result<String, Error> {
        self.check()?;
        #[cfg(not(unix))]
        { let _ = installation_key; return Err(Error::Unavailable); }
        #[cfg(unix)]
        {
        let account = self.accounting_key(installation_key)?;
        let captured = capture(&self.home, &self.id)?;
        if captured.checksum != self.checksum { return Err(Error::Unavailable); }
        let selected = selected_snapshot(&captured.profile, &self.id, &self.model)?;
        let mut document: serde_json::Value = serde_json::from_slice(&selected).map_err(|_| Error::Unavailable)?;
        // Octos touches updated_at when another model is configured. That and
        // other fallback selections do not change this model's runtime.
        document.as_object_mut().ok_or(Error::Unavailable)?.remove("updated_at");
        document.pointer_mut("/config/llm").and_then(serde_json::Value::as_object_mut)
            .ok_or(Error::Unavailable)?.insert("fallbacks".into(), serde_json::json!([]));
        let selected = serde_json::to_vec(&document).map_err(|_| Error::Unavailable)?;
        let mut hash = Sha256::new();
        hash.update(b"hagency-octos-selected-model-v1\0");
        hash.update(account.as_bytes());
        hash.update(Sha256::digest(selected));
        if let Some(defaults) = captured.defaults { hash.update([1]); hash.update(Sha256::digest(defaults)); }
        else { hash.update([0]); }
        hash.update((self.model.family.len() as u64).to_be_bytes());
        hash.update(self.model.family.as_bytes());
        hash.update((self.model.model.len() as u64).to_be_bytes());
        hash.update(self.model.model.as_bytes());
        self.check()?;
        Ok(format!("{:x}", hash.finalize()))
        }
    }
    pub fn check(&self) -> Result<(), Error> {
        if Instant::now() >= self.expires {
            return Err(Error::Unavailable);
        }
        self.running_guard_unchecked().check()
    }

    fn running_guard_unchecked(&self) -> RunningProfileGuard {
        RunningProfileGuard {
            home: self.home.clone(),
            id: self.id.clone(),
            model: self.model.clone(),
            checksum: self.checksum,
            #[cfg(target_os = "macos")]
            auth: None,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod keychain_tests {
    use super::*;
    #[test]
    fn selected_profile_resolves_only_declared_keychain_markers() {
        let profile = br#"{"config":{"env_vars":{"FIRST":"keychain:","SECOND":"keychain:scoped::coding","PLAIN":"kept"}},"name":"fixture"}"#;
        let mut requested = Vec::new();
        let bytes = resolved_snapshot(profile, true, |name| {
            requested.push(name.to_owned());
            Ok(format!("synthetic-{name}"))
        }).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        requested.sort();
        assert_eq!(requested, ["FIRST", "scoped::coding"]);
        assert_eq!(result["config"]["env_vars"]["FIRST"], "synthetic-FIRST");
        assert_eq!(result["config"]["env_vars"]["SECOND"], "synthetic-scoped::coding");
        assert_eq!(result["config"]["env_vars"]["PLAIN"], "kept");
        let bad = br#"{"env_vars":{"KEY":"keychain:-invalid"}}"#;
        assert!(resolved_snapshot(bad, false, |_| panic!("invalid marker must not be looked up")).is_err());
        let missing = br#"{"config":{"env_vars":{"UNRELATED":"keychain:missing"}}}"#;
        let result = resolved_snapshot(missing, true, |_| Err(Error::Unavailable)).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(result["config"]["env_vars"]["UNRELATED"], "");
    }

    #[test]
    fn ambient_fallback_preserves_profile_and_defaults_precedence() {
        let profile = br#"{"config":{"llm":{"primary":{"family_id":"moonshot","model_id":"fixture"}}}}"#;
        let defaults = br#"{"env_vars":{"KIMI_API_KEY":"synthetic-inherited"}}"#;
        let result = with_ambient_credential(profile, Some(defaults), "moonshot", |_| panic!("configured key wins")).unwrap();
        assert_eq!(result, profile);
        let mut requested = Vec::new();
        let result = with_ambient_credential(profile, None, "moonshot", |name| {
            requested.push(name.to_owned());
            (name == "KIMI_API_KEY").then(|| "synthetic-environment".into())
        }).unwrap();
        assert_eq!(requested, ["MOONSHOT_API_KEY", "KIMI_API_KEY"]);
        let result: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(result["config"]["env_vars"]["KIMI_API_KEY"], "synthetic-environment");
        let custom = br#"{"config":{"llm":{"primary":{"route":{"api_key_env":"CUSTOM_KEY"}}}}}"#;
        let mut requested = Vec::new();
        let result = with_ambient_credential(custom, None, "openai", |name| {
            requested.push(name.to_owned()); Some("synthetic-custom".into())
        }).unwrap();
        assert_eq!(requested, ["CUSTOM_KEY"]);
        let result: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(result["config"]["env_vars"]["CUSTOM_KEY"], "synthetic-custom");
    }
}
