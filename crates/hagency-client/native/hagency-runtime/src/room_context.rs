//! The room context every runtime's turn starts with: room memory, then the
//! team skills. One composer, so Claude, Codex and any later runtime get the
//! same text and nothing depends on a tool call succeeding.
use crate::{room_memory, room_skills};

/// Memory block (if the room keeps any) and the inlined skills, or `None`
/// when neither applies (no skills support for this room).
pub fn compose(memory: &[room_memory::Entry], skills: Option<&[room_skills::Skill]>) -> Option<String> {
    join(room_memory::render_block(memory), skills.map(room_skills::render_inline))
}

/// Joins an already rendered memory block and skills block.
pub fn join(memory: Option<String>, skills: Option<String>) -> Option<String> {
    match (memory, skills) {
        (Some(memory), Some(skills)) => Some(format!("{memory}\n\n{skills}")),
        (memory, skills) => memory.or(skills),
    }
}

#[cfg(test)]
mod conformance;
