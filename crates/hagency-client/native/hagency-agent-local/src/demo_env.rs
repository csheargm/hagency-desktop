//! The environment's GitHub token, resolved the same way at Agent creation
//! (the repository check), in the desktop poller and when an Agent's runtime
//! starts: the Agent's own saved token first, else `GH_TOKEN`, else
//! `GITHUB_TOKEN` (the `gh` CLI's order), in every build.
//!
//! An environment token is never written to disk: it is read again each time
//! it is needed, so it lives only where the owner put it (their shell or
//! launcher), and an Agent created against it keeps working after a restart.
//! (The module name predates this: it used to forward only in demo builds.)
use std::ffi::OsString;

/// The variable the runtimes (and `gh`) read.
pub const GITHUB_TOKEN_ENV: &str = "GH_TOKEN";
/// The environment fallback for an Agent without its own token, first match wins.
pub const TOKEN_VARIABLES: [&str; 2] = [GITHUB_TOKEN_ENV, "GITHUB_TOKEN"];
/// Codex drops environment variables whose names look like secrets unless told
/// not to; without this `gh` would see no token inside a Codex command.
pub const CODEX_KEEP_ENV_OVERRIDE: &str = "shell_environment_policy.ignore_default_excludes=true";

/// The one token rule: `saved` (the Agent's own) when set, else the first
/// non-blank of `TOKEN_VARIABLES` from `env`.
pub fn resolve(saved: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    saved.filter(|t| !t.trim().is_empty()).map(str::to_owned).or_else(|| TOKEN_VARIABLES.iter().find_map(|name| env(name).filter(|v| !v.trim().is_empty())))
}

/// This process's environment token, for an Agent with none of its own.
pub fn github_token() -> Option<OsString> {
    resolve(None, |name| std::env::var(name).ok()).map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v).to_owned())
    }
    #[test]
    fn the_saved_token_wins_then_gh_token_then_github_token() {
        assert_eq!(resolve(Some("own"), env(&[("GH_TOKEN", "gh")])).as_deref(), Some("own"));
        assert_eq!(resolve(None, env(&[("GITHUB_TOKEN", "b"), ("GH_TOKEN", "a")])).as_deref(), Some("a"));
        assert_eq!(resolve(None, env(&[("GH_TOKEN", " "), ("GITHUB_TOKEN", "b")])).as_deref(), Some("b"));
        assert_eq!(resolve(Some(""), env(&[])), None);
    }
    /// Regression (GPT review P2): create verified repositories with an
    /// environment token but the runtime saw none outside demo builds.
    #[test]
    fn an_agent_created_with_an_environment_token_gets_it_at_run_time() {
        let at_create = resolve(None, env(&[("GITHUB_TOKEN", "t")]));
        let at_run = crate::agent_env::AgentEnvironment::default().github_token_with(env(&[("GITHUB_TOKEN", "t")]));
        assert_eq!(at_create, at_run);
    }
}
