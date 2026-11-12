//! Host-only bridges from retained process custody to bounded runner sessions.
//! No server route constructs this type; sandbox/dispatch qualification is open.
use hagency_platform::SupervisedReport;
use std::io;

mod managed;
mod shared_text;
pub use shared_text::{
    SHARED_WEB_INSTRUCTIONS, SHARED_WEB_INSTRUCTIONS_FILE, shared_text_policy,
    SHARED_TEXT_INSTRUCTIONS, SHARED_TEXT_INSTRUCTIONS_FILE, SharedTextActivityControl,
    SharedTextEvent, SharedTextProcessObservation, SharedTextProcessSource,
};
mod session;
mod room_workspace;
pub use managed::{
    MANAGED_DISABLED_FEATURES, MANAGED_INSTRUCTIONS, MANAGED_INSTRUCTIONS_FILE,
    ManagedPolicyOverrides, ManagedProcessObservation, ManagedProcessSource, ManagedProcessState,
};
pub use session::OwnedSession;
mod octos;
pub use octos::OwnedOctosSession;
mod claude;
mod claude_shared;
pub use claude_shared::SharedClaudePolicy;
pub use claude::OwnedClaudeSession;

/// Exact platform observations, deliberately distinct from upstream completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    Pending,
    Observed(SupervisedReport),
    Unknown { kind: io::ErrorKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    #[error("invalid host runner settings")]
    Settings,
    #[error("native runner piped platform is unavailable")]
    Unsupported,
    /// The existing supervisor may have lost startup acknowledgement after spawn.
    /// Absence of a returned owner is never proof that no child briefly executed.
    #[error("native runner startup or IO handoff failed; cleanup must be inspected")]
    Uncertain {
        kind: io::ErrorKind,
        cleanup: Cleanup,
    },
}

pub use shared_text::{
    SHARED_TOOLS_INSTRUCTIONS, SHARED_TOOLS_INSTRUCTIONS_FILE, shared_execution_policy,
};
