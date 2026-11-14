//! Recent room context for an owned turn in an end-to-end encrypted room.
//! The management server only routes ciphertext there, so it can attach no
//! history. The Agent's own device, which already decrypted the request,
//! reads the preceding timeline with its own Megolm keys before the turn and
//! the model receives a bounded excerpt instead of having to ask for one.
//!
//! Visibility: the homeserver applies history visibility to the Agent's user,
//! and on top of that nothing older than the Agent's current join is quoted,
//! so a `shared` room never leaks messages from before the Agent was admitted.
//! The boundary is the join membership event's position in the timeline, not
//! `origin_server_ts`, which a federated sender can forge in either direction.
//! Redactions: the server's redacted form (`redacted_because`) is trusted; a
//! timeline redaction event is applied only to its own sender's messages, so an
//! unauthorized redaction of someone else's message cannot hide it.
//! The request itself is never quoted. Output is bounded by message count and
//! bytes; messages this device cannot decrypt are counted, never guessed.
use super::RuntimeError;
use super::e2ee::HistoryBatch;
use hagency_agent_local::inbox;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
mod render;
pub(super) use render::preamble;

pub(super) const MAX_ENCRYPTED_CONTEXT_MESSAGES: usize = 20;
pub(super) const MAX_ENCRYPTED_CONTEXT_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 2 * 1024;
const MAX_PAGES: usize = 4;
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const UNREADABLE: &str = "unreadable";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RecentContext {
    /// `"room"` or `"thread"`.
    pub scope: &'static str,
    /// Oldest first.
    pub messages: Vec<RecentMessage>,
    pub omitted_older: bool,
    /// Set when nothing could be read; the model is told to say so.
    pub unavailable: Option<&'static str>,
    /// Earlier messages this device holds no keys for. Never quoted.
    pub undecryptable: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecentMessage {
    pub event_id: String,
    pub sender: String,
    pub sender_display_name: Option<String>,
    pub body: String,
    pub origin_server_ts: u64,
    pub edited: bool,
}

/// Matrix reads the context needs; `AgentDevice` implements it in e2ee/recent.rs.
pub(super) trait RecentSource {
    /// Event ID of the Agent's current join membership event.
    fn join_event(&self, room: &str) -> impl Future<Output = Result<Option<String>, RuntimeError>> + Send;
    fn display_name(&self, room: &str, user: &str) -> impl Future<Output = Option<String>> + Send;
    fn start(&self, room: &str, anchor: &str) -> impl Future<Output = Result<Option<String>, RuntimeError>> + Send;
    /// One older page cut at `join_event` (see `cut_at_join`); `true` once reached.
    fn page(&self, room: &str, from: &str, join_event: &str) -> impl Future<Output = Result<(HistoryBatch, bool), RuntimeError>> + Send;
}

/// Keeps the records of a newest-first page that come after `join_event` in
/// timeline order, and says whether the join event was in this page.
pub(super) fn cut_at_join<I>(entries: I, join_event: &str) -> (Vec<Value>, bool)
where
    I: IntoIterator<Item = (Option<String>, Option<Value>)>,
{
    let mut records = Vec::new();
    for (id, record) in entries {
        if id.as_deref() == Some(join_event) {
            return (records, true);
        }
        records.extend(record);
    }
    (records, false)
}

/// Never fails the turn: a read failure becomes `unavailable: "unreadable"`,
/// logged here and reported to the model instead of guessed at.
pub(super) async fn read<S: RecentSource>(source: &S, dispatch: &inbox::Dispatch) -> RecentContext {
    let thread = thread_of(dispatch);
    let scope = if thread.is_some() { "thread" } else { "room" };
    match tokio::time::timeout(READ_TIMEOUT, collect(source, dispatch, thread, scope)).await {
        Ok(Ok(context)) => context,
        Ok(Err(error)) => {
            tracing::warn!(%error, room_id = %dispatch.room_id, "Encrypted room context unavailable");
            unreadable(scope)
        }
        Err(_) => {
            tracing::warn!(room_id = %dispatch.room_id, "Encrypted room context read timed out");
            unreadable(scope)
        }
    }
}
fn unreadable(scope: &'static str) -> RecentContext {
    RecentContext { scope, unavailable: Some(UNREADABLE), ..Default::default() }
}
fn thread_of(d: &inbox::Dispatch) -> Option<&str> {
    let root = d.thread_root.as_str();
    (root.starts_with('$') && root != d.event_id && root != d.room_id).then_some(root)
}

async fn collect<S: RecentSource>(
    source: &S,
    d: &inbox::Dispatch,
    thread: Option<&str>,
    scope: &'static str,
) -> Result<RecentContext, RuntimeError> {
    // Without its own join event the Agent cannot place the boundary safely.
    let join_event = source.join_event(&d.room_id).await?.ok_or(RuntimeError::Request)?;
    let (mut records, mut from) = (Vec::new(), source.start(&d.room_id, &d.event_id).await?);
    let mut complete = from.is_none();
    for _ in 0..MAX_PAGES {
        let Some(token) = from.take() else { break };
        let (batch, reached_join) = source.page(&d.room_id, &token, &join_event).await?;
        records.extend(batch.events);
        from = batch.next;
        if reached_join || from.is_none() {
            complete = true;
            break;
        }
    }
    let mut context = select(scope, &Window { records: &records, trigger: &d.event_id, thread, complete });
    name_senders(source, &d.room_id, &mut context.messages).await;
    Ok(context)
}

/// Display names only for the senders actually quoted (at most
/// `MAX_ENCRYPTED_CONTEXT_MESSAGES`); a missing name just omits the label.
async fn name_senders<S: RecentSource>(source: &S, room: &str, messages: &mut [RecentMessage]) {
    let mut names: BTreeMap<String, Option<String>> = BTreeMap::new();
    for message in messages.iter() {
        if !names.contains_key(&message.sender) {
            let name = source.display_name(room, &message.sender).await;
            names.insert(message.sender.clone(), name);
        }
    }
    for message in messages.iter_mut() {
        message.sender_display_name = names.get(&message.sender).cloned().flatten();
    }
}

/// `records` are `history_record` values after the Agent's join, newest first.
pub(super) struct Window<'a> {
    pub records: &'a [Value],
    pub trigger: &'a str,
    pub thread: Option<&'a str>,
    pub complete: bool,
}
pub(super) fn select(scope: &'static str, w: &Window<'_>) -> RecentContext {
    let (edits, redacted) = corrections(w.records);
    let (mut messages, mut seen, mut bytes) = (Vec::new(), HashSet::new(), 0usize);
    let (mut omitted_older, mut undecryptable) = (!w.complete, 0usize);
    for record in w.records {
        let Some(id) = record["eventId"].as_str() else { continue };
        let in_scope = w.thread.is_none_or(|root| id == root || record["relatedEventId"] == root);
        if id == w.trigger || !in_scope || !seen.insert(id) {
            continue;
        }
        if record["status"] == "unavailable_encrypted_message" {
            undecryptable += 1;
            continue;
        }
        let Some(mut message) = quotable(record, &redacted) else { continue };
        if let Some(body) = edits.get(&(message.event_id.clone(), message.sender.clone())) {
            (message.body, message.edited) = (clip(body), true);
        }
        let size = message.body.len() + message.sender.len();
        if messages.len() == MAX_ENCRYPTED_CONTEXT_MESSAGES || bytes + size > MAX_ENCRYPTED_CONTEXT_BYTES {
            omitted_older = true;
            break;
        }
        bytes += size;
        messages.push(message);
    }
    messages.reverse();
    RecentContext { scope, messages, omitted_older, unavailable: None, undecryptable }
}

/// Latest edit per (target, original sender) and self-redactions per
/// (target, sender). Records are newest first, so the first edit seen for a
/// target is the latest. A redaction only counts against its own sender's
/// message: that is always authorized, while redacting others' messages needs
/// power the client cannot check here; the server marks those it applied.
type Keyed = HashSet<(String, String)>;
fn corrections(records: &[Value]) -> (HashMap<(String, String), String>, Keyed) {
    let (mut edits, mut redacted) = (HashMap::new(), HashSet::new());
    for r in records {
        let (Some(target), Some(sender)) = (r["target"].as_str(), r["sender"].as_str()) else { continue };
        match (r["status"].as_str(), r["body"].as_str()) {
            (Some("edit"), Some(body)) => {
                edits.entry((target.to_owned(), sender.to_owned())).or_insert_with(|| body.to_owned());
            }
            (Some("redaction"), _) => {
                redacted.insert((target.to_owned(), sender.to_owned()));
            }
            _ => {}
        }
    }
    (edits, redacted)
}
fn quotable(record: &Value, redacted: &Keyed) -> Option<RecentMessage> {
    let id = record["eventId"].as_str()?;
    let sender = record["sender"].as_str()?;
    let text_type = matches!(record["messageType"].as_str(), Some("m.text" | "m.notice" | "m.emote"));
    if record.get("status").is_some() || redacted.contains(&(id.to_owned(), sender.to_owned())) || !text_type {
        return None;
    }
    let body = record["body"].as_str().filter(|b| !b.trim().is_empty())?;
    Some(RecentMessage {
        event_id: id.to_owned(),
        sender: sender.to_owned(),
        sender_display_name: None,
        body: clip(body),
        origin_server_ts: record["timestamp"].as_u64()?,
        edited: false,
    })
}
fn clip(body: &str) -> String {
    if body.len() <= MAX_MESSAGE_BYTES {
        return body.to_owned();
    }
    let mut end = MAX_MESSAGE_BYTES - '…'.len_utf8();
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &body[..end])
}

#[cfg(test)]
mod tests;
