//! Pure rules of the Memory list and proposal cards: what a change writes and
//! who may write it. No Matrix and no widgets here; the storage format is the
//! backend's (`room_memory`, `room_memory_timeline`), so the list, the cards
//! and the Agents always agree.
use hagency::room_memory::{self, AcceptError, Entry, MEMORY_PROPOSAL_KEY, MEMORY_REASON_BYTES, MEMORY_TEXT_BYTES, PowerLevels, Storage};
use hagency::room_memory_timeline::revision_content;
use serde_json::{Value, json};

/// Where a write goes: a state event keyed by the entry id, or a custom
/// timeline event (encrypted rooms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    State { state_key: String },
    Timeline,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Write {
    pub delivery: Delivery,
    pub content: Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    PowerLevel,
    Empty,
    TooLong,
    Missing,
}
impl WriteError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::PowerLevel => "You do not have permission to change this room's memory.",
            Self::Empty => "Write something to remember.",
            Self::TooLong => "That note is too long. Keep it under 500 characters.",
            Self::Missing => "That entry is no longer in the room's memory.",
        }
    }
}
impl From<AcceptError> for WriteError {
    fn from(_: AcceptError) -> Self {
        Self::PowerLevel
    }
}

/// One change a person makes to the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Add(String),
    Edit(String),
    Pin(bool),
    Delete,
}

/// Collapses whitespace and control characters, like the reader does.
pub fn tidy(raw: &str) -> String {
    let flat: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    flat.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn checked_text(raw: &str) -> Result<String, WriteError> {
    let text = tidy(raw);
    match text.len() {
        0 => Err(WriteError::Empty),
        n if n > MEMORY_TEXT_BYTES => Err(WriteError::TooLong),
        _ => Ok(text),
    }
}

fn delivery(storage: Storage, id: &str) -> Delivery {
    match storage {
        Storage::State => Delivery::State { state_key: id.into() },
        Storage::Timeline => Delivery::Timeline,
    }
}

/// The write for `change` by `me`. `current` is the entry being changed (none
/// for Add), `id` names a new entry. Edit, pin and delete bump `rev`; a delete
/// is empty state content, or a `deleted` revision in an encrypted room.
pub fn plan_write(change: &Change, current: Option<&Entry>, id: &str, me: &str, levels: &PowerLevels, storage: Storage) -> Result<Write, WriteError> {
    let mut entry = match (change, current) {
        (Change::Add(text), _) => Entry { id: id.into(), text: checked_text(text)?, author: me.into(), source_event: None, pinned: false, rev: 0 },
        (_, Some(entry)) => entry.clone(),
        (_, None) => return Err(WriteError::Missing),
    };
    let rev = if matches!(change, Change::Add(_)) { 1 } else { entry.rev + 1 };
    match change {
        Change::Edit(text) => entry.text = checked_text(text)?,
        Change::Pin(pinned) => entry.pinned = *pinned,
        Change::Add(_) | Change::Delete => {}
    }
    let content = room_memory::accept(&entry, me, levels, storage, rev)?;
    entry.rev = rev;
    let content = match (change, storage) {
        (Change::Delete, Storage::State) => json!({}),
        (Change::Delete, Storage::Timeline) => revision_content(&entry, true),
        _ => content,
    };
    Ok(Write { delivery: delivery(storage, &entry.id), content })
}

/// A card an Agent posted: the proposed entry and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub entry: Entry,
    pub reason: String,
}
/// Parses a message's raw content. `None` means a plain notice.
pub fn parse_proposal(content: &Value) -> Option<Proposal> {
    let entry = room_memory::proposal_from_content(content)?;
    let reason = tidy(content.get(MEMORY_PROPOSAL_KEY)?["reason"].as_str().unwrap_or_default());
    Some(Proposal { entry, reason: reason.chars().take(MEMORY_REASON_BYTES).collect() })
}

/// The write Accept sends. A proposal already in memory (same id) is replaced
/// at the next revision, so accepting twice cannot lose an edit.
pub fn accept_plan(proposal: &Proposal, existing: Option<&Entry>, me: &str, levels: &PowerLevels, storage: Storage) -> Result<Write, WriteError> {
    let rev = existing.map_or(1, |e| e.rev + 1);
    let content = room_memory::accept(&proposal.entry, me, levels, storage, rev)?;
    Ok(Write { delivery: delivery(storage, &proposal.entry.id), content })
}

/// What a proposal card shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardState {
    Pending,
    Adding,
    Added,
    Dismissed,
    Failed(String),
}
/// Whether Accept is offered. Everyone sees the card; only members who may
/// write memory can accept it.
pub fn can_accept(state: &CardState, can_write: bool) -> bool {
    can_write && matches!(state, CardState::Pending | CardState::Failed(_))
}
pub fn can_dismiss(state: &CardState) -> bool {
    matches!(state, CardState::Pending | CardState::Failed(_))
}
/// A card for a proposal whose id is already in memory is done; a dismissed
/// one stays hidden.
pub fn card_state(in_memory: bool, dismissed: bool, working: bool, failure: Option<&str>) -> CardState {
    match (in_memory, dismissed, working, failure) {
        (true, ..) => CardState::Added,
        (_, true, ..) => CardState::Dismissed,
        (_, _, true, _) => CardState::Adding,
        (_, _, _, Some(message)) => CardState::Failed(message.into()),
        _ => CardState::Pending,
    }
}

/// A new entry id: short, unique, within the 64-byte id limit.
pub fn new_entry_id() -> String {
    format!("m-{}", uuid::Uuid::new_v4().simple())
}
