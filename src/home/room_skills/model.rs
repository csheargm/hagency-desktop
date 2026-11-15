//! Pure rules of the Skills list and skill proposal cards: what a change
//! writes and who may write it. No Matrix and no widgets here.
use crate::home::room_memory::model::{Delivery, Write};
use hagency::room_memory::{PowerLevels, Storage};
use hagency::room_skills::{self as skills, SKILL_BODY_BYTES, SKILL_DESCRIPTION_BYTES, SKILL_STATE_TYPE, Skill, proposal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    PowerLevel,
    Name,
    Description,
    Body,
    Missing,
}
impl WriteError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::PowerLevel => "You do not have permission to change this room's skills.",
            Self::Name => "Use a short name of lowercase letters, digits, - or _.",
            Self::Description => "Add a one-line description (under 200 characters).",
            Self::Body => "Add the skill's steps in Markdown (under 16 KiB).",
            Self::Missing => "That skill is no longer in this room.",
        }
    }
}

/// One change a person makes in the Skills list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Save { name: String, description: String, body: String },
    Delete,
}

fn checked(name: &str, description: &str, body: &str) -> Result<(String, String, String), WriteError> {
    let name = name.trim().to_lowercase();
    if !skills::valid_name(&name) {
        return Err(WriteError::Name);
    }
    let description = crate::home::room_memory::model::tidy(description);
    if description.is_empty() || description.len() > SKILL_DESCRIPTION_BYTES {
        return Err(WriteError::Description);
    }
    if body.trim().is_empty() || body.len() > SKILL_BODY_BYTES {
        return Err(WriteError::Body);
    }
    Ok((name, description, body.to_owned()))
}

fn write(skill: &Skill, deleted: bool, storage: Storage) -> Write {
    let timeline = storage == Storage::Timeline;
    let delivery = if timeline { Delivery::Timeline } else { Delivery::State { state_key: skill.name.clone() } };
    Write { delivery, content: skills::content(skill, deleted, timeline) }
}

/// The write for `change` by `me`, at the revision after `current` (the skill
/// of that name now in the room). Delete needs `current`.
pub fn plan_write(change: &Change, current: Option<&Skill>, me: &str, levels: &PowerLevels, storage: Storage) -> Result<Write, WriteError> {
    if !levels.can_write_type(me, SKILL_STATE_TYPE) {
        return Err(WriteError::PowerLevel);
    }
    let rev = current.map_or(1, |s| s.rev + 1);
    match change {
        Change::Save { name, description, body } => {
            let (name, description, body) = checked(name, description, body)?;
            Ok(write(&Skill { name, description, body, author: me.into(), rev }, false, storage))
        }
        Change::Delete => {
            let mut gone = current.cloned().ok_or(WriteError::Missing)?;
            gone.rev = rev;
            Ok(write(&gone, true, storage))
        }
    }
}

/// A card an Agent posted: the proposed skill (author = the real sender).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub id: String,
    pub skill: Skill,
    pub reason: String,
}

/// Only a Hagency Agent on the room's own homeserver may propose; the
/// payload's `author` is attacker-controlled and ignored.
pub fn trusted_proposal(content: &serde_json::Value, sender: &str, room: &str) -> Option<Proposal> {
    let server = hagency::agent_questions::server_name(room)?;
    if !hagency::agent_questions::is_agent_on(sender, server) {
        return None;
    }
    let (id, mut skill, reason) = proposal::proposal_from_content(content)?;
    skill.author = sender.to_owned();
    Some(Proposal { id, skill, reason })
}

/// What Accept writes: the proposed skill at the next revision of its name.
pub fn accept_plan(p: &Proposal, existing: Option<&Skill>, me: &str, levels: &PowerLevels, storage: Storage) -> Result<Write, WriteError> {
    let timeline = storage == Storage::Timeline;
    let content = proposal::accept(&p.skill, &p.skill.author, me, levels, existing.map(|s| s.rev), timeline).map_err(|_| WriteError::PowerLevel)?;
    let delivery = if timeline { Delivery::Timeline } else { Delivery::State { state_key: p.skill.name.clone() } };
    Ok(Write { delivery, content })
}

/// A card is done once its exact body is the skill of that name.
pub fn is_applied(p: &Proposal, current: &[Skill]) -> bool {
    current.iter().any(|s| s.name == p.skill.name && s.body == p.skill.body && s.description == p.skill.description)
}
