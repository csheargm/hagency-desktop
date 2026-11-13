//! A scoped logout fence survives a missing socket and process interruption.
use super::enrollment::Enrollment;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
const FILE: &str = "logout-request.json";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub owner: String,
    pub origin: String,
    pub root: PathBuf,
}
pub fn load(home: &Path) -> Result<Option<Request>, &'static str> {
    super::read_private(&home.join(FILE), 16 * 1024)
        .map_err(|_| "daemon_logout_storage_unavailable")?
        .map(|raw| serde_json::from_slice(&raw).map_err(|_| "daemon_logout_storage_unavailable"))
        .transpose()
}
pub fn request(home: &Path, origin: &str, owner: &str) -> Result<(), &'static str> {
    let Some(enrollment) = Enrollment::load(home).map_err(|e| e.code())? else {
        return Ok(());
    };
    if enrollment.owner != owner || !super::same_origin(&enrollment.origin, origin) {
        return Err("daemon_owner_scope_mismatch");
    }
    let value = Request {
        owner: owner.into(),
        origin: enrollment.origin,
        root: enrollment.owner_root,
    };
    if let Some(previous) = load(home)? {
        return if previous == value {
            Ok(())
        } else {
            Err("daemon_owner_scope_mismatch")
        };
    }
    let raw = serde_json::to_vec(&value).map_err(|_| "daemon_logout_storage_unavailable")?;
    hagency_store::private::replace(&home.join(FILE), &raw)
        .map_err(|_| "daemon_logout_storage_unavailable")
}
#[cfg(unix)]
pub(super) fn finish(home: &Path) -> Result<(), &'static str> {
    match std::fs::remove_file(home.join(FILE)) {
        Ok(()) => std::fs::File::open(home)
            .and_then(|f| f.sync_all())
            .map_err(|_| "daemon_logout_storage_unavailable"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("daemon_logout_storage_unavailable"),
    }
}
