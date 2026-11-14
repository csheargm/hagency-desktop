//! Which setup step is running or failed last, so a button can show one live
//! progress line and name the step that stopped it.
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every step name, in setup order; the UI names the one that failed.
pub const STEPS: [&str; 14] = ["projects", "create-space", "agents", "create-agent", "use-local-login", "models", "read-model", "save-model", "agent-policy", "save-policy", "start-service", "agent-details", "assign-device", "owner-direct"];
pub(super) static LAST_FAILED_STEP: AtomicUsize = AtomicUsize::new(usize::MAX);
pub(super) static CURRENT_STEP: AtomicUsize = AtomicUsize::new(usize::MAX);
/// The step a setup is on right now (for a live progress line), if one runs.
pub fn current_step() -> Option<&'static str> {
    STEPS.get(CURRENT_STEP.load(Ordering::Relaxed)).copied()
}
/// One short progress line per step, for the owner watching the button.
pub fn progress_text(step: Option<&str>) -> &'static str {
    match step {
        Some("projects" | "create-space") => "Creating…",
        Some("agents" | "create-agent") => "Creating the Agent…",
        Some("use-local-login" | "models") => "Connecting your local login…",
        Some("read-model" | "save-model" | "agent-policy" | "save-policy") => "Saving its settings…",
        Some("start-service" | "agent-details" | "assign-device" | "owner-direct") => "Starting…",
        _ => "Working…",
    }
}
pub(super) fn step_index(label: &str) -> usize {
    STEPS.iter().position(|s| *s == label).unwrap_or(usize::MAX)
}
/// The step whose failure ended the most recent setup, if any.
pub fn last_failed_step() -> Option<&'static str> {
    STEPS.get(LAST_FAILED_STEP.load(Ordering::Relaxed)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_lines_follow_the_steps() {
        assert_eq!(progress_text(Some("create-agent")), "Creating the Agent…");
        assert_eq!(progress_text(Some("use-local-login")), "Connecting your local login…");
        assert_eq!(progress_text(Some("start-service")), "Starting…");
        assert_eq!(progress_text(None), "Working…");
        LAST_FAILED_STEP.store(step_index("save-model"), Ordering::Relaxed);
        assert_eq!(last_failed_step(), Some("save-model"));
        LAST_FAILED_STEP.store(usize::MAX, Ordering::Relaxed);
        assert_eq!(last_failed_step(), None);
    }
}
