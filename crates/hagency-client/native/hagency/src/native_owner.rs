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
    /// The owner's opt-in to let this Agent change files in its saved project
    /// directory. Left out of the body when not selected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared_workspace: bool,
    /// The project directory the owner was shown when selecting the opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}
impl RuntimeStartRequest {
    /// A shared-workspace start names the directory the owner confirmed. A
    /// recovery start repeats an earlier confirmed one instead.
    pub(crate) fn accepts_shared_workspace(&self, workspace: &str) -> bool {
        !self.shared_workspace
            || self.recovery_consent_id.is_some()
            || self.expected_workspace.as_deref() == Some(workspace)
    }
}

/// The body of a local model save as the endpoint reads it: the new profile
/// and the digest of the profile the caller last read. `current` is the
/// `modelProfile` of that read (null when none is saved yet); the save is
/// refused when the stored profile is no longer that one. `None` when
/// `current` is not a profile as this build reads it.
pub fn model_save_request(
    current: &serde_json::Value,
    profile: serde_json::Value,
) -> Option<serde_json::Value> {
    let current: Option<hagency_agent_local::ModelProfile> =
        serde_json::from_value(current.clone()).ok()?;
    let digest = hagency_agent_local::model_profile_digest(current.as_ref()).ok()?;
    Some(serde_json::json!({"profile":profile,"expectedProfileDigest":digest}))
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
