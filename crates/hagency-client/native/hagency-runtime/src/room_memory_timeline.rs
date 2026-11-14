//! Room memory in end-to-end encrypted rooms. Entries (and every edit, pin and
//! delete) are custom timeline events of type `io.hagency.memory`, sent through
//! the room's Megolm session, so the homeserver never sees the text. Timeline
//! events can be sent by any member, so [`reduce_timeline`] keeps a revision
//! only if its sender had the room's required power level when it is read.
//! The Agent device only reads; a person's client writes on Accept.
use crate::room_memory::{Entry, MEMORY_STATE_TYPE, MEMORY_TEXT_BYTES, PowerLevels, clean};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Newest-first pages of this many events are requested while scanning.
pub const SCAN_PAGE_EVENTS: u32 = 50;
/// At most this many pages are read per reconstruction.
pub const SCAN_MAX_PAGES: usize = 8;
/// At most this many timeline events are looked at, memory or not.
pub const SCAN_MAX_EVENTS: usize = 400;
/// At most this many bytes of memory events are kept.
pub const SCAN_MAX_BYTES: usize = 256 * 1024;
const MAX_ID_BYTES: usize = 64;

/// What one backward scan of the timeline found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineScan {
    /// Decrypted memory events, oldest first, as raw JSON with `sender`.
    pub events: Vec<Value>,
    /// Events this device could not decrypt or authenticate. Any of them may
    /// have been a memory entry; they are counted, never guessed.
    pub unreadable: usize,
    /// A page, event or byte cap stopped the scan before the room's start.
    pub truncated: bool,
    /// Id of the newest event seen; a cache key for this scan.
    pub newest: Option<String>,
}

/// Current entries plus what the reader should be told about the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineView {
    pub entries: Vec<Entry>,
    pub unreadable: usize,
    pub truncated: bool,
    /// Revisions ignored because their sender lacked the power level.
    pub ignored: usize,
}

/// Content of a timeline revision a person's client sends on Accept, edit,
/// pin or delete. A tombstone keeps the id and carries `deleted: true`.
pub fn revision_content(entry: &Entry, deleted: bool) -> Value {
    json!({"id":entry.id,"text":entry.text,"author":entry.author,
        "source_event":entry.source_event,"pinned":entry.pinned,"rev":entry.rev,"deleted":deleted})
}

fn revision(event: &Value) -> Option<(Entry, bool)> {
    if event["type"] != MEMORY_STATE_TYPE {
        return None;
    }
    let content = &event["content"];
    let id = content["id"].as_str().filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)?;
    let deleted = content["deleted"].as_bool().unwrap_or(false);
    let text = content["text"].as_str().map(clean).filter(|t| !t.is_empty() && t.len() <= MEMORY_TEXT_BYTES);
    if text.is_none() && !deleted {
        return None;
    }
    let entry = Entry {
        id: id.into(),
        text: text.unwrap_or_default(),
        author: content["author"].as_str().unwrap_or_default().into(),
        source_event: content["source_event"].as_str().map(Into::into),
        pinned: content["pinned"].as_bool().unwrap_or(false),
        rev: content["rev"].as_u64().unwrap_or(0),
    };
    Some((entry, deleted))
}

/// Reduces a scan with the room's power levels. Latest `rev` per id wins (a
/// later event wins a tie); a tombstone removes the entry.
pub fn reduce_timeline(scan: &TimelineScan, levels: &PowerLevels) -> TimelineView {
    let mut latest: BTreeMap<String, (Entry, bool)> = BTreeMap::new();
    let mut ignored = 0;
    for event in &scan.events {
        let Some((entry, deleted)) = revision(event) else { continue };
        if !event["sender"].as_str().is_some_and(|sender| levels.can_write(sender)) {
            ignored += 1;
            continue;
        }
        if latest.get(&entry.id).is_none_or(|(old, _)| entry.rev >= old.rev) {
            latest.insert(entry.id.clone(), (entry, deleted));
        }
    }
    let live: Vec<Value> = latest
        .into_values()
        .filter(|(_, deleted)| !deleted)
        .map(|(e, _)| json!({"state_key":e.id,"content":{"text":e.text,"author":e.author,
            "source_event":e.source_event,"pinned":e.pinned,"rev":e.rev}}))
        .collect();
    TimelineView {
        entries: crate::room_memory::reduce(&live),
        unreadable: scan.unreadable,
        truncated: scan.truncated,
        ignored,
    }
}

/// One timeline event as the scanner sees it.
#[derive(Clone, Debug)]
pub enum ScanKind {
    /// A decrypted, authenticated `io.hagency.memory` event (raw JSON).
    Memory(Value),
    /// Could not be decrypted or authenticated by this device.
    Unreadable,
    Other,
}
pub struct ScanPage {
    /// Newest first, as the server returns a backward page.
    pub events: Vec<(String, ScanKind)>,
    /// Token for the next older page; `None` at the start of the room.
    pub next: Option<String>,
}
pub enum ScanStep {
    Next(String),
    Done,
}

/// Folds backward pages into a [`TimelineScan`] under the page, event and
/// byte caps. The caller fetches; this holds the rules.
#[derive(Default)]
pub struct ScanState {
    scan: TimelineScan,
    newest: Option<String>,
    pages: usize,
    seen: usize,
    bytes: usize,
    last_token: Option<String>,
}
impl ScanState {
    /// Id of the newest event seen, once the first page is in.
    pub fn newest(&self) -> Option<&str> {
        self.newest.as_deref()
    }
    pub fn push(&mut self, page: ScanPage) -> ScanStep {
        self.pages += 1;
        if self.newest.is_none() {
            self.newest = page.events.first().map(|(id, _)| id.clone());
        }
        let empty = page.events.is_empty();
        for (_, kind) in page.events {
            if self.seen >= SCAN_MAX_EVENTS {
                return self.stop();
            }
            self.seen += 1;
            if !self.keep(kind) {
                return self.stop();
            }
        }
        match page.next {
            Some(token) if !empty && self.last_token.as_deref() != Some(&token) => {
                if self.pages >= SCAN_MAX_PAGES {
                    return self.stop();
                }
                self.last_token = Some(token.clone());
                ScanStep::Next(token)
            }
            Some(_) => self.stop(),
            None => ScanStep::Done,
        }
    }
    fn keep(&mut self, kind: ScanKind) -> bool {
        match kind {
            ScanKind::Memory(event) => {
                self.bytes += event.to_string().len();
                if self.bytes > SCAN_MAX_BYTES {
                    return false;
                }
                self.scan.events.push(event);
            }
            ScanKind::Unreadable => self.scan.unreadable += 1,
            ScanKind::Other => {}
        }
        true
    }
    fn stop(&mut self) -> ScanStep {
        self.scan.truncated = true;
        ScanStep::Done
    }
    /// Oldest first, newest id recorded for the caller's cache.
    pub fn finish(mut self) -> TimelineScan {
        self.scan.events.reverse();
        self.scan.newest = self.newest;
        self.scan
    }
}

#[cfg(test)]
mod tests;
