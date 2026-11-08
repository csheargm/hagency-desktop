//! Bounded runtime protocol and host-only owned child IO foundation. It grants
//! no permissions and cannot mutate canonical dispatch, task or Matrix state.
pub mod claude;
pub mod codex;
pub mod deadline;
mod json;
pub mod octos;
pub mod owned;
pub mod questions;
pub mod task_mcp;
pub mod room_tools;
