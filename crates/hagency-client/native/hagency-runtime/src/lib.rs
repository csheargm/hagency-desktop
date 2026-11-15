//! Bounded runtime protocol and host-only owned child IO foundation. It grants
//! no permissions and cannot mutate canonical dispatch, task or Matrix state.
#[macro_use]
mod merge_note;
pub use merge_note::AGENT_MERGE_NOTE;
pub mod claude;
pub mod codex;
pub mod deadline;
mod json;
pub mod octos;
pub mod owned;
pub mod questions;
pub mod task_mcp;
pub mod room_memory;
pub mod room_memory_timeline;
pub mod room_skills;
pub mod room_tools;
pub mod live_text;
