//! Native owner management reuses the authenticated client service in process.
pub use crate::console::native::*;

/// The current request body shared by native callers and the runtime endpoint.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_consent_id: Option<String>,
    #[serde(default)]
    pub binding_id: String,
    #[serde(default)]
    pub takeover: bool,
    #[serde(default)]
    pub host_files: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// Private process-custody entry. The guardian validates its inherited handles;
/// this does not launch a provider or accept owner commands from argv.
pub fn run_guardian_if_requested() -> Option<std::io::Result<()>> {
    #[cfg(unix)]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("guardian")) {
        return Some(hagency_platform::run_guardian());
    }
    None
}
