//! Skill proposals: so team skills grow from work Agents actually did. An
//! Agent posts a card (an ordinary message, never a skill event) with a new
//! skill or an edit of one by name; a person with the room's power level for
//! `io.hagency.skill` accepts it from their own client, at the next revision.
use super::{SKILL_BODY_BYTES, SKILL_DESCRIPTION_BYTES, SKILL_STATE_TYPE, Skill, SkillOperation, content, skill_from, valid_name};
use crate::room_memory::{AcceptError, PowerLevels, clean};
use crate::room_tools::Error;
use serde_json::{Value, json};

pub const SKILL_PROPOSE_TOOL: &str = "skill_propose";
/// Key of the proposal payload inside a proposal card's message content.
pub const SKILL_PROPOSAL_KEY: &str = "io.hagency.skill_proposal";
pub const SKILL_REASON_BYTES: usize = 300;

pub(super) fn definition() -> Value {
    json!({"name":SKILL_PROPOSE_TOOL,
        "description":"Propose a reusable team skill for this room, or an edit of an existing one (use its exact name). It only posts a card: a person with enough power level must accept it. Propose after you finished a multi-step task you had to figure out and that the team will repeat (build and test commands, a review checklist, release steps), written as a short Markdown procedure. Propose an edit when a skill you followed was wrong or incomplete, sending the full corrected body. Never propose one-off facts or preferences (use memory_propose) and never secrets.",
        "inputSchema":{"type":"object","properties":{
            "name":{"type":"string","minLength":1,"maxLength":super::SKILL_NAME_BYTES,"description":"lowercase letters, digits, - or _"},
            "description":{"type":"string","minLength":1,"maxLength":SKILL_DESCRIPTION_BYTES},
            "body_md":{"type":"string","minLength":1,"maxLength":SKILL_BODY_BYTES},
            "reason":{"type":"string","minLength":1,"maxLength":SKILL_REASON_BYTES}},
            "required":["name","description","body_md","reason"],"additionalProperties":false}})
}

fn text(args: &serde_json::Map<String, Value>, key: &str, max: usize) -> Result<String, Error> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() && s.len() <= max => Ok(s.to_owned()),
        _ => Err(Error::Malformed),
    }
}

pub(super) fn parse(arguments: &Value) -> Result<SkillOperation, Error> {
    let args = arguments.as_object().ok_or(Error::Malformed)?;
    if args.keys().any(|k| !matches!(k.as_str(), "name" | "description" | "body_md" | "reason")) {
        return Err(Error::Malformed);
    }
    let name = text(args, "name", super::SKILL_NAME_BYTES)?;
    if !valid_name(&name) {
        return Err(Error::Malformed);
    }
    let skill = Skill {
        name,
        description: clean(&text(args, "description", SKILL_DESCRIPTION_BYTES)?),
        body: text(args, "body_md", SKILL_BODY_BYTES)?,
        author: String::new(),
        rev: 0,
    };
    Ok(SkillOperation::Propose { skill, reason: clean(&text(args, "reason", SKILL_REASON_BYTES)?) })
}

/// The card an Agent posts. `id` is stable per dispatch and content, so a
/// replay is the same card; `edit` says the name already exists.
pub fn proposal_content(id: &str, skill: &Skill, reason: &str, source_event: &str, agent: &str, edit: bool) -> Value {
    let mut content = json!({
        "msgtype": "m.notice",
        SKILL_PROPOSAL_KEY: {"id":id,"name":skill.name,"description":skill.description,"body_md":skill.body,
            "reason":reason,"source_event":source_event,"author":agent,"edit":edit},
    });
    name_proposal(&mut content, "Agent");
    content
}

/// The fallback body other clients show names the Agent, never its raw id.
pub fn name_proposal(content: &mut Value, agent_name: &str) {
    let p = &content[SKILL_PROPOSAL_KEY];
    let verb = if p["edit"] == true { "an edit of team skill" } else { "a new team skill" };
    let field = |key: &str| p[key].as_str().unwrap_or_default().to_owned();
    let body = format!("{agent_name} proposes {verb} \"{}\": {}\nReason: {}", field("name"), field("description"), field("reason"));
    content["body"] = json!(body);
}

/// A card's proposal: (id, skill, reason). The caller sets the author from the
/// event's real sender; the payload's author is never trusted.
pub fn proposal_from_content(content: &Value) -> Option<(String, Skill, String)> {
    let p = content.get(SKILL_PROPOSAL_KEY)?;
    let id = p["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64)?;
    let (skill, deleted) = skill_from(p["name"].as_str()?, &json!({"description":p["description"],"body_md":p["body_md"],"rev":0}))?;
    if deleted || skill.body.trim().is_empty() {
        return None;
    }
    let reason: String = clean(p["reason"].as_str().unwrap_or_default()).chars().take(SKILL_REASON_BYTES).collect();
    Some((id.into(), skill, reason))
}

/// What Accept writes under the acceptor's own user: the proposed skill at the
/// revision after `existing`, as state or as an encrypted timeline event.
pub fn accept(proposed: &Skill, author: &str, acceptor: &str, levels: &PowerLevels, existing: Option<u64>, timeline: bool) -> Result<Value, AcceptError> {
    if !levels.can_write_type(acceptor, SKILL_STATE_TYPE) {
        return Err(AcceptError::PowerLevel);
    }
    let mut skill = proposed.clone();
    skill.author = author.into();
    skill.rev = existing.map_or(1, |rev| rev + 1);
    Ok(content(&skill, false, timeline))
}
