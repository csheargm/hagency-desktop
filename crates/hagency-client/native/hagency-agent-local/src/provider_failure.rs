//! Why a provider failed a turn, when the cause is the provider's own
//! availability: a usage limit, overload or outage. Only these causes may
//! hand the same turn to a configured backup model; a tool denial, an owner
//! stop or any failure after a tool ran keeps the plain failure.
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailability {
    UsageLimit,
    Overloaded,
    Outage,
}

impl Unavailability {
    /// The English reason shown in the reply; hosts translate it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::UsageLimit => "usage limit",
            Self::Overloaded => "overloaded",
            Self::Outage => "outage",
        }
    }
}

/// Codex app-server `turn.error`: its structured `codexErrorInfo` first,
/// then the message text.
pub fn classify_codex(error: &Value) -> Option<Unavailability> {
    let info = &error["codexErrorInfo"];
    let code = info.as_str().or_else(|| info.as_object().and_then(|o| o.keys().next().map(String::as_str)));
    match code {
        Some("usageLimitExceeded") => Some(Unavailability::UsageLimit),
        Some("serverOverloaded") => Some(Unavailability::Overloaded),
        Some(
            "internalServerError" | "httpConnectionFailed" | "responseStreamConnectionFailed"
            | "responseStreamDisconnected" | "responseTooManyFailedAttempts",
        ) => Some(Unavailability::Outage),
        _ => classify_text(error["message"].as_str().unwrap_or("")),
    }
}

const LIMIT_MARKERS: [&str; 6] = ["usage limit", "rate limit", "limit reached", "quota", "api error: 429", "too many requests"];
const OVERLOAD_MARKERS: [&str; 2] = ["overloaded", "api error: 529"];
const OUTAGE_MARKERS: [&str; 6] = ["api error: 500", "api error: 502", "api error: 503", "api error: 504", "service unavailable", "internal server error"];

/// A failed turn's provider text (Claude's error result, or a Codex message).
pub fn classify_text(text: &str) -> Option<Unavailability> {
    let text = text.to_ascii_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|m| text.contains(m));
    if has(&LIMIT_MARKERS) {
        Some(Unavailability::UsageLimit)
    } else if has(&OVERLOAD_MARKERS) {
        Some(Unavailability::Overloaded)
    } else if has(&OUTAGE_MARKERS) {
        Some(Unavailability::Outage)
    } else {
        None
    }
}

/// The error a failed turn returns: `Unavailable` only when the cause is the
/// provider's availability AND no tool was requested, so a retry cannot
/// repeat a side effect.
pub fn failed_turn(cause: Option<Unavailability>, tool_requests: usize) -> crate::codex::Error {
    match cause {
        Some(cause) if tool_requests == 0 => crate::codex::Error::Unavailable(cause),
        _ => crate::codex::Error::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_structured_codes_classify() {
        assert_eq!(classify_codex(&json!({"codexErrorInfo":"usageLimitExceeded"})), Some(Unavailability::UsageLimit));
        assert_eq!(classify_codex(&json!({"codexErrorInfo":"serverOverloaded"})), Some(Unavailability::Overloaded));
        assert_eq!(classify_codex(&json!({"codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":502}}})), Some(Unavailability::Outage));
        assert_eq!(classify_codex(&json!({"message":"You've hit your usage limit. Try again at 3:08 PM."})), Some(Unavailability::UsageLimit));
    }
    #[test]
    fn other_failures_do_not_classify() {
        assert_eq!(classify_codex(&json!({"codexErrorInfo":"contextWindowExceeded","message":"context too long"})), None);
        assert_eq!(classify_codex(&json!({"codexErrorInfo":"unauthorized"})), None);
        assert_eq!(classify_codex(&Value::Null), None);
        assert_eq!(classify_text("Permission to use Bash was denied"), None);
        assert_eq!(classify_text("Request interrupted by user"), None);
    }
    #[test]
    fn claude_texts_classify() {
        assert_eq!(classify_text("Claude AI usage limit reached|1763240000"), Some(Unavailability::UsageLimit));
        assert_eq!(classify_text("API Error: 529 {\"type\":\"overloaded_error\"}"), Some(Unavailability::Overloaded));
        assert_eq!(classify_text("API Error: 503 Service Unavailable"), Some(Unavailability::Outage));
    }
    #[test]
    fn only_tool_free_unavailability_may_retry() {
        assert!(matches!(failed_turn(Some(Unavailability::UsageLimit), 0), crate::codex::Error::Unavailable(Unavailability::UsageLimit)));
        assert!(matches!(failed_turn(Some(Unavailability::UsageLimit), 1), crate::codex::Error::Failed));
        assert!(matches!(failed_turn(None, 0), crate::codex::Error::Failed));
    }
}
