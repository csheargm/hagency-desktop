//! Codex 0.153.4 App Server JSONL. Protocol observations are untrusted data.
pub mod approval;
mod connection;
mod launch;
mod rollout_usage;
use crate::json;
pub mod session;
pub mod transport;
pub mod tree;
pub mod version;
mod wire;

pub use connection::{Connection, Event, Phase, TurnScope};
pub use launch::{native_arguments, validate_native_configuration};
pub use rollout_usage::Cursor as NativeUsageCursor;
pub use wire::{Decoder, Message, RequestId, RpcError, encode};

/// Includes every byte preceding LF (including CR for CRLF input).
/// Codex sends each command's full output in `item/completed`, and
/// `turn/completed` carries every item of the turn. A research turn with tens
/// of commands and web searches exceeded the former 1 MiB, and the decoder
/// ended the run just as its answer was ready. Still a hard bound.
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_DEPTH: usize = crate::json::MAX_DEPTH;
pub const MAX_PENDING: usize = 32;
pub const MAX_SERVER_IDS: usize = 1024;
pub const PARTIAL_FRAME_MS: u64 = 10_000;
pub const MAX_REQUEST_MS: u64 = 1_200_000;
/// The acknowledgement budget for a startup RPC (`initialize`, `thread/start`,
/// `thread/resume`) in a session that carries a REQUIRED MCP server. TS 1:1
/// (`router/src/runner.ts:431` `acknowledgementTimeoutMs ?? 60_000`, used for
/// `Codex initialize` at `:807` and `Codex thread start` at `:828`).
///
/// Distinct from the generic per-request `response_ms` (2 s,
/// `hagency-execution/src/host.rs:58`). Codex withholds the `thread/start`
/// response until every required MCP server in the thread config has completed
/// its handshake (`session/task_mcp.rs:50-51`), and bounds that itself by
/// `startup_timeout_sec` (5 s). The product `response_ms` ceiling sits *under*
/// Codex's own ceiling, so a cold helper lost the race and the live rig died
/// `Transport(Timeout)` at `stage="thread_start"`, settling the attempt
/// `outcome_unknown` (board #105). Never overrides the operation ceiling: the
/// driver takes `min` of this and the transport lifetime.
pub const ACKNOWLEDGEMENT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Codex protocol connection is closed")]
    Closed,
    #[error("invalid Codex protocol state")]
    State,
    #[error("invalid Codex protocol envelope")]
    Envelope,
    #[error("Codex protocol capacity exceeded")]
    Capacity,
    #[error("Codex protocol request identity mismatch")]
    Identity,
    #[error("Codex protocol deadline exceeded")]
    Timeout,
    #[error("Codex protocol clock moved backwards")]
    Clock,
    #[error("Codex protocol ended with incomplete data or pending work")]
    UnexpectedEof,
    #[error("Codex protocol transport failed")]
    Transport,
    #[error("Codex is older than the oldest supported version")]
    Version,
}

fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

impl From<crate::json::Error> for Error {
    fn from(error: crate::json::Error) -> Self {
        match error {
            crate::json::Error::Envelope => Self::Envelope,
            crate::json::Error::Capacity => Self::Capacity,
        }
    }
}
