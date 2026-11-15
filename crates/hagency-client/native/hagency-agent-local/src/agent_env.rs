//! Extra environment one Agent's own CLI runtime (Claude or Codex) starts
//! with, on top of the runtime's fixed allowlist. Today this carries only the
//! Agent's GitHub token as `GH_TOKEN`, so `gh` and `git` in that Agent's
//! sessions act with that Agent's access and no other Agent's.
//!
//! Values are secrets: `Debug` shows the variable names only.
use std::collections::BTreeMap;

/// The variable the GitHub CLI reads first.
pub const GITHUB_TOKEN_VARIABLE: &str = "GH_TOKEN";

/// Codex drops every variable whose name contains KEY, SECRET or TOKEN from
/// the commands it runs unless told not to. The runtime starts from an empty
/// environment, so the only such variables present are the ones set here.
pub const CODEX_KEEP_SECRET_VARIABLES: &str = "shell_environment_policy.ignore_default_excludes=true";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct AgentEnvironment(BTreeMap<String, String>);

impl AgentEnvironment {
    /// The environment that gives the runtime this GitHub token.
    pub fn github_token(token: &str) -> Self {
        Self(BTreeMap::from([(GITHUB_TOKEN_VARIABLE.to_owned(), token.to_owned())]))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
    /// The `-c` overrides Codex needs so its commands see these variables.
    pub fn codex_overrides(&self) -> Vec<String> {
        if self.is_empty() { Vec::new() } else { vec![CODEX_KEEP_SECRET_VARIABLES.to_owned()] }
    }
}

impl std::fmt::Debug for AgentEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_names_never_values() {
        let env = AgentEnvironment::github_token("github_pat_SECRET123");
        let shown = format!("{env:?}");
        assert!(shown.contains("GH_TOKEN"));
        assert!(!shown.contains("SECRET123"));
    }

    #[test]
    fn codex_keeps_secret_variables_only_when_some_are_set() {
        assert!(AgentEnvironment::default().codex_overrides().is_empty());
        let env = AgentEnvironment::github_token("t");
        assert_eq!(env.codex_overrides(), vec![CODEX_KEEP_SECRET_VARIABLES.to_owned()]);
    }

    fn profile(environment: AgentEnvironment) -> crate::codex::Profile {
        crate::codex::Profile {
            executable: "/usr/bin/true".into(),
            home: std::env::temp_dir(),
            codex_home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            model: "m".into(),
            effort: "low".into(),
            shared_auth: false,
            web_search: false,
            environment,
        }
    }

    #[test]
    fn codex_launch_gets_only_its_own_agents_token() {
        let with = profile(AgentEnvironment::github_token("tok-a")).launch(&[]).unwrap();
        assert_eq!(with.environment.get(std::ffi::OsStr::new("GH_TOKEN")).map(|v| v.to_str().unwrap()), Some("tok-a"));
        let keep = std::ffi::OsString::from(CODEX_KEEP_SECRET_VARIABLES);
        assert!(with.arguments.contains(&keep), "Codex must not filter GH_TOKEN out");
        let without = profile(AgentEnvironment::default()).launch(&[]).unwrap();
        assert!(!without.environment.contains_key(std::ffi::OsStr::new("GH_TOKEN")));
        assert!(!without.arguments.contains(&keep));
    }
}
