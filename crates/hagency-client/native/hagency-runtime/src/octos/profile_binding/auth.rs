//! Selected Octos login, kept private to an already authorized process.
use super::{Error, MAX_PROFILE_BYTES};
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, path::{Path, PathBuf}};

/// Mirror Octos's config-context auth-home precedence without opening it.
pub(crate) fn source_from_environment(owner_home: &Path) -> Result<PathBuf, Error> {
    if !owner_home.is_absolute() { return Err(Error::Invalid); }
    let config = std::env::var_os("OCTOS_CONFIG_DIR")
        .filter(|v| !v.to_string_lossy().trim().is_empty());
    if let Some(config) = config {
        let value = PathBuf::from(config);
        let value = if value == Path::new("~") { owner_home.to_path_buf() }
            else if let Ok(rest) = value.strip_prefix("~") { owner_home.join(rest) }
            else { value };
        return if value.is_absolute() { Ok(value) }
            else { std::env::current_dir().map(|cwd| cwd.join(value)).map_err(|_| Error::Unavailable) };
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .filter(|p| p.is_absolute());
    Ok(xdg.unwrap_or_else(|| owner_home.join(".config")).join("octos"))
}

/// Contains only a private digest, never the actual credential.
pub(crate) struct Selection { home: PathBuf, family: String, digest: [u8; 32] }

/// Octos resolves the registry entry first, then indexes the auth store by
/// its canonical name. Keep its supported aliases here so, for example, a
/// `google` profile uses `gemini` credentials just like the CLI.
fn canonical_family(family: &str) -> String {
    let name = family.to_lowercase();
    match name.as_str() {
        "google" => "gemini",
        "qwen" => "dashscope",
        "kimi" => "moonshot",
        "kimi-coding" => "moonshot-coding",
        "minimaxi" => "minimax-cn",
        "nim" => "nvidia",
        "r9s.ai" => "r9s",
        "vertex-ai" | "vertexai" => "vertex",
        "glm" => "zhipu",
        "z.ai" => "zai",
        "z.ai-coding" | "glm-coding" => "zai-coding",
        "llamacpp" | "llama.cpp" | "llama-server" | "llama_server" | "lmstudio" | "lm-studio" | "openai-compatible" => "local",
        _ => return name,
    }.into()
}

/// Mirror the pinned Octos registry's API-key names. Explicit custom names
/// remain exclusive unless they are one of that provider's own aliases.
pub(super) fn env_candidates(family: &str, custom: Option<&str>) -> Vec<String> {
    let family = canonical_family(family);
    let names: Vec<String> = match family.as_str() {
        "openai" => vec!["OPENAI_API_KEY"], "anthropic" => vec!["ANTHROPIC_API_KEY"],
        "gemini" => vec!["GEMINI_API_KEY"], "dashscope" => vec!["DASHSCOPE_API_KEY"],
        "moonshot" => vec!["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "moonshot-coding" => vec!["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"],
        "minimax" => vec!["MINIMAX_API_KEY"], "minimax-cn" => vec!["MINIMAX_CN_API_KEY", "MINIMAX_API_KEY"],
        "zai" => vec!["ZAI_API_KEY"], "zai-coding" => vec!["ZAI_CODING_API_KEY", "ZAI_API_KEY"],
        "zhipu" => vec!["ZHIPU_API_KEY"], "nvidia" => vec!["NVIDIA_API_KEY"],
        "vertex" => vec!["VERTEX_SA_JSON"], "groq" => vec!["GROQ_API_KEY"],
        "deepseek" => vec!["DEEPSEEK_API_KEY"], "openrouter" => vec!["OPENROUTER_API_KEY"],
        "r9s" => vec!["R9S_API_KEY"], "vllm" => vec!["VLLM_API_KEY"],
        _ => return vec![custom.map(str::to_owned).unwrap_or_else(|| format!("{}_API_KEY", family.to_uppercase()))],
    }.into_iter().map(str::to_owned).collect();
    match custom {
        None => names,
        Some(value) if names.iter().any(|name| name == value) => {
            std::iter::once(value.to_owned()).chain(names.into_iter().filter(|name| name != value)).collect()
        }
        Some(value) => vec![value.to_owned()],
    }
}

impl Selection {
    pub(crate) fn capture(home: &Path, family: &str) -> Result<(Self, Option<Vec<u8>>), Error> {
        let family = canonical_family(family);
        let selected = selected_record(home, &family)?;
        let digest = digest(&selected);
        Ok((Self { home: home.to_path_buf(), family, digest }, selected))
    }
    pub(crate) fn check(&self) -> Result<(), Error> {
        if digest(&selected_record(&self.home, &self.family)?) == self.digest { Ok(()) }
        else { Err(Error::Unavailable) }
    }
}

fn digest(selected: &Option<Vec<u8>>) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"hagency-octos-selected-auth-v1\0");
    if let Some(bytes) = selected { hash.update([1]); hash.update(bytes); } else { hash.update([0]); }
    hash.finalize().into()
}

fn selected_record(home: &Path, family: &str) -> Result<Option<Vec<u8>>, Error> {
    use rustix::fs::{Mode, OFlags};
    if !home.is_absolute() || family.is_empty() || family.len() > 128 || family.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    let directory = match home.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !home.exists() => return Ok(None),
        Err(_) => return Err(Error::Unavailable),
    };
    let dir = rustix::fs::open(&directory, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
        .map_err(|_| Error::Unavailable)?;
    let file = match rustix::fs::openat(&dir, "auth.json", OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::empty()) {
        Ok(file) => std::fs::File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(Error::Unavailable),
    };
    if !file.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_PROFILE_BYTES) { return Err(Error::Unavailable); }
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 > MAX_PROFILE_BYTES { return Err(Error::Unavailable); }
    let document: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)?;
    let credentials = document.get("credentials").and_then(serde_json::Value::as_object).ok_or(Error::Unavailable)?;
    let Some(record) = credentials.get(family) else { return Ok(None); };
    let record = record.as_object().ok_or(Error::Unavailable)?;
    let mut copied = serde_json::Map::new();
    for key in ["access_token", "provider", "auth_method"] {
        let value = record.get(key).and_then(serde_json::Value::as_str).ok_or(Error::Unavailable)?;
        if value.len() > 64 * 1024 { return Err(Error::Unavailable); }
        copied.insert(key.into(), value.into());
    }
    if let Some(expires) = record.get("expires_at") {
        if !expires.is_null() && expires.as_str().is_none() { return Err(Error::Unavailable); }
        copied.insert("expires_at".into(), expires.clone());
    }
    let mut credentials = serde_json::Map::new();
    credentials.insert(family.into(), copied.into());
    serde_json::to_vec(&serde_json::json!({"credentials": credentials})).map(Some).map_err(|_| Error::Unavailable)
}

/// Write only the selected provider, never the whole auth store or refresh
/// tokens. `home` is the host-owned empty HOME for the disposable process.
pub(crate) fn snapshot_into(home: &Path, selected: &[u8]) -> Result<(), Error> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = rustix::fs::open(home, flags, Mode::empty()).map_err(|_| Error::Unavailable)?;
    rustix::fs::mkdirat(&root, "octos", Mode::RUSR | Mode::WUSR | Mode::XUSR).map_err(|_| Error::Unavailable)?;
    let directory = rustix::fs::openat(&root, "octos", flags, Mode::empty()).map_err(|_| Error::Unavailable)?;
    let file = rustix::fs::openat(&directory, "auth.json", OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::RUSR | Mode::WUSR).map_err(|_| Error::Unavailable)?;
    let mut file = std::fs::File::from(file);
    file.write_all(selected).and_then(|_| file.sync_all()).map_err(|_| Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn only_selected_login_is_copied_and_rotation_revokes_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");
        fs::create_dir(&source).unwrap(); fs::create_dir(&dest).unwrap();
        let credential = |token: &str| json!({"access_token":token,"refresh_token":"do-not-copy", "provider":"first", "auth_method":"oauth"});
        let file = source.join("auth.json");
        fs::write(&file, json!({"credentials":{"first":credential("synthetic-a"),"other":credential("synthetic-b")}}).to_string()).unwrap();
        let (guard, selected) = Selection::capture(&source, "first").unwrap();
        let selected = selected.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&selected).unwrap();
        assert_eq!(value["credentials"]["first"]["access_token"], "synthetic-a");
        assert!(value["credentials"].get("other").is_none());
        assert!(value["credentials"]["first"].get("refresh_token").is_none());
        snapshot_into(&dest, &selected).unwrap();
        assert_eq!(fs::read(dest.join("octos/auth.json")).unwrap(), selected);
        fs::write(&file, json!({"credentials":{"first":credential("synthetic-a"),"other":credential("changed")}}).to_string()).unwrap();
        guard.check().unwrap();
        fs::write(&file, json!({"credentials":{"first":credential("rotated")}}).to_string()).unwrap();
        assert!(guard.check().is_err());
        fs::remove_file(&file).unwrap();
        let (absent, record) = Selection::capture(&source, "first").unwrap();
        assert!(record.is_none());
        fs::write(&file, json!({"credentials":{"first":credential("new")}}).to_string()).unwrap();
        assert!(absent.check().is_err());
    }

    #[test]
    fn profile_aliases_select_octos_registry_canonical_auth_keys() {
        for (alias, canonical) in [("GOOGLE", "gemini"), ("kimi", "moonshot"), ("glm-coding", "zai-coding"), ("lm-studio", "local"), ("openai", "openai")] {
            assert_eq!(canonical_family(alias), canonical);
        }
    }
}
