//! Demo-only: hand the launcher's GitHub token to the Agents' runtimes so `gh`
//! works for them. The demo launcher sets `GH_TOKEN` from `gh auth token`; a
//! product build (no `demo` feature) never forwards it, and Agent runtimes
//! otherwise start from a cleared environment.
use std::ffi::OsString;

/// The variable the launcher sets and the runtimes (and `gh`) read.
pub const GITHUB_TOKEN_ENV: &str = "GH_TOKEN";
/// Codex drops environment variables whose names look like secrets unless told
/// not to; without this `gh` would see no token inside a Codex command.
pub const CODEX_KEEP_ENV_OVERRIDE: &str = "shell_environment_policy.ignore_default_excludes=true";

/// The token to forward, when this is a demo build and the launcher set one.
pub fn github_token() -> Option<OsString> {
    forwarded(std::env::var_os(GITHUB_TOKEN_ENV))
}

#[cfg(feature = "demo")]
fn forwarded(value: Option<OsString>) -> Option<OsString> {
    value.filter(|token| !token.is_empty())
}
#[cfg(not(feature = "demo"))]
fn forwarded(_value: Option<OsString>) -> Option<OsString> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "demo")]
    #[test]
    fn demo_build_forwards_a_non_empty_token_only() {
        assert_eq!(forwarded(Some("t".into())), Some("t".into()));
        assert_eq!(forwarded(Some("".into())), None);
        assert_eq!(forwarded(None), None);
    }
    /// Regression guard: a product build must never hand the owner's GitHub token to an Agent.
    #[cfg(not(feature = "demo"))]
    #[test]
    fn product_build_never_forwards_the_token() {
        assert_eq!(forwarded(Some("t".into())), None);
    }
}
