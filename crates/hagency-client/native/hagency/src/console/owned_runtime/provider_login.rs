//! A personal Agent whose owner's Claude Code or Codex is signed out. The
//! login check runs before any model input, so the request did not start: the
//! room gets a plain answer saying so, and the owner's status names the fix.
//! The answer carries only display names: no account, path or credential.
use hagency_agent_local::codex::{CODEX_LOGIN_REQUIRED, Error};
use hagency_agent_local::runtime::CLAUDE_LOGIN_REQUIRED;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoginNeeded {
    Claude,
    Codex,
}

/// Fallback when the server never told this device the Agent's name.
const UNNAMED_AGENT: &str = "This Agent";

impl LoginNeeded {
    /// The provider sign-in an error reports as missing, if that is all it is.
    pub(super) fn of(error: &Error) -> Option<Self> {
        match error {
            Error::Profile(CLAUDE_LOGIN_REQUIRED) => Some(Self::Claude),
            Error::Profile(CODEX_LOGIN_REQUIRED) => Some(Self::Codex),
            _ => None,
        }
    }
    /// The status code the desktop words as "<provider> sign-in needed".
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Claude => "claude_login_required",
            Self::Codex => "codex_login_required",
        }
    }
    /// The room answer, English then Chinese like the server's own notices.
    pub(super) fn room_reply(self, agent: Option<&str>, owner: &str) -> String {
        let agent = agent.map(str::trim).filter(|a| !a.is_empty()).unwrap_or(UNNAMED_AGENT);
        let (provider, command) = match self {
            Self::Claude => ("Claude", "claude login"),
            Self::Codex => ("Codex", "codex login"),
        };
        format!(
            "{agent} can't run right now: its {provider} login on its owner's computer is signed out. \
             {owner} needs to run `{command}`, then ask again.\n\
             {agent} 暂时无法运行：它在主人电脑上的 {provider} 登录已退出。请 {owner} 运行 `{command}` 后再提问。"
        )
    }
}

/// The owner as the room shows them: their display name, else the Matrix
/// ID's localpart ("@chris:server" → "chris").
pub(super) fn owner_label(display_name: Option<String>, owner_mxid: &str) -> String {
    display_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let local = owner_mxid.strip_prefix('@').unwrap_or(owner_mxid);
            local.split(':').next().unwrap_or(local).to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_login_is_a_not_started_request() {
        assert_eq!(LoginNeeded::of(&Error::Profile(CLAUDE_LOGIN_REQUIRED)), Some(LoginNeeded::Claude));
        assert_eq!(LoginNeeded::of(&Error::Profile(CODEX_LOGIN_REQUIRED)), Some(LoginNeeded::Codex));
        for other in [
            Error::Profile("local model provider requires an account"),
            Error::Profile("room tools already bound"),
            Error::Protocol("Claude initialize"),
            Error::Timeout,
            Error::Failed,
        ] {
            assert_eq!(LoginNeeded::of(&other), None, "{other:?} keeps its own handling");
        }
    }

    #[test]
    fn the_room_is_told_who_must_sign_in_and_how() {
        let reply = LoginNeeded::Claude.room_reply(Some("Ada"), "Chris");
        let (english, chinese) = reply.split_once('\n').unwrap();
        assert_eq!(
            english,
            "Ada can't run right now: its Claude login on its owner's computer is signed out. \
             Chris needs to run `claude login`, then ask again."
        );
        assert!(chinese.contains("Ada") && chinese.contains("Chris") && chinese.contains("`claude login`"));
        let codex = LoginNeeded::Codex.room_reply(None, "Chris");
        assert!(codex.starts_with("This Agent can't run right now: its Codex login"));
        assert!(codex.contains("`codex login`") && !codex.contains("claude"));
        for text in [&reply, &codex] {
            assert!(!text.contains("interrupted") && !text.contains("cannot be confirmed"));
        }
    }

    #[test]
    fn the_owner_status_names_the_sign_in_and_other_failures_keep_theirs() {
        use super::super::{provider_failure, startup_check_failure, RuntimeError};
        let claude = Error::Profile(CLAUDE_LOGIN_REQUIRED);
        let codex = Error::Profile(CODEX_LOGIN_REQUIRED);
        assert!(matches!(startup_check_failure(&claude), RuntimeError::Execution("claude_login_required")));
        assert!(matches!(startup_check_failure(&codex), RuntimeError::Execution("codex_login_required")));
        assert!(matches!(provider_failure("turn", &claude), RuntimeError::Execution("claude_login_required")));
        assert!(matches!(provider_failure("turn", &codex), RuntimeError::Execution("codex_login_required")));
        assert!(matches!(startup_check_failure(&Error::Protocol("Claude initialize")), RuntimeError::Provider));
        let unverified = Error::Profile(hagency_agent_local::codex::CODEX_ACCOUNT_UNVERIFIED);
        assert!(matches!(startup_check_failure(&unverified), RuntimeError::Provider));
        assert!(matches!(provider_failure("turn", &unverified), RuntimeError::Provider), "an unusable account keeps its old mapping");
        assert!(matches!(provider_failure("turn", &Error::Profile("room tools already bound")), RuntimeError::Profile));
        assert!(matches!(provider_failure("turn", &Error::Timeout), RuntimeError::Execution("codex_provider_timeout")));
    }

    #[test]
    fn the_owner_is_named_by_display_name_or_localpart() {
        assert_eq!(owner_label(Some(" Chris ".into()), "@chris:server"), "Chris");
        assert_eq!(owner_label(Some("  ".into()), "@chris:server"), "chris");
        assert_eq!(owner_label(None, "@chris:server"), "chris");
    }
}
