//! "N updates" under an Agent's answer opens in place (ADR 0024): the run's
//! progress messages, which the Agent keeps in a thread under the request,
//! are listed inside the card in the main timeline. Nobody has to open the
//! thread to see what the Agent did, and the room timeline stays one answer
//! per request.
use matrix_sdk::{
    deserialized_responses::EncryptionInfo,
    room::{IncludeRelations, RelationsOptions},
    ruma::{OwnedEventId, OwnedRoomId, UInt, events::relation::RelationType},
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Most updates read for one card; the newest are kept.
pub const MAX_UPDATES: usize = 50;
/// Longest line shown per update, in characters.
const LINE_CHARS: usize = 160;

/// A card's updates as they are shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Updates {
    Loading,
    Loaded(Vec<String>),
    Failed,
}

/// Posted when a card's updates were opened, closed or loaded.
#[derive(Clone, Debug)]
pub struct RunUpdatesChanged {
    pub room: String,
    pub thread_root: String,
}

/// Signed-in account (with its session epoch), room, thread root, Agent:
/// decrypted text never carries over to another account (GPT review).
type Key = (String, String, String, String);
#[derive(Default)]
struct Store {
    open: HashMap<Key, Updates>,
    /// The reply count each load answered, so a newer update reloads.
    loaded_for: HashMap<Key, usize>,
    /// The newest load per key; an older answer is dropped.
    latest: HashMap<Key, u64>,
    next: u64,
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);
fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|p| p.into_inner())
}

fn account() -> String {
    let user = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    format!("{user}#{}", crate::account_session::epoch())
}
fn key(room: &str, thread_root: &str, agent: &str) -> Key {
    (account(), room.to_owned(), thread_root.to_owned(), agent.to_owned())
}

/// The updates `agent` posted under a card, or `None` while they are closed.
pub fn shown(room: &str, thread_root: &str, agent: &str) -> Option<Updates> {
    store().open.get(&key(room, thread_root, agent)).cloned()
}

/// Opens or closes a card's updates; `count` is the thread's reply count.
pub fn toggle(room: &str, thread_root: &str, agent: &str, count: usize) {
    let key = key(room, thread_root, agent);
    let opened = {
        let mut store = store();
        if store.open.remove(&key).is_some() {
            store.latest.remove(&key);
            false
        } else {
            store.open.insert(key.clone(), Updates::Loading);
            true
        }
    };
    if opened {
        load(key, count);
    }
    makepad_widgets::Cx::post_action(RunUpdatesChanged { room: room.to_owned(), thread_root: thread_root.to_owned() });
}

/// Reloads open updates whose thread grew since they were read. A zero
/// count is the row's reset before its real count, never a change.
pub fn follow(room: &str, thread_root: &str, agent: &str, count: usize) {
    if count == 0 {
        return;
    }
    let key = key(room, thread_root, agent);
    let stale = {
        let store = store();
        matches!(store.open.get(&key), Some(Updates::Loaded(_))) && store.loaded_for.get(&key) != Some(&count)
    };
    if stale {
        load(key, count);
    }
}

/// The text a card shows for its open updates.
pub fn text(updates: &Updates) -> String {
    match updates {
        Updates::Loading => crate::i18n::tr("Loading updates…").to_owned(),
        Updates::Failed => crate::i18n::tr("Could not load the updates. Close and open them to try again.").to_owned(),
        Updates::Loaded(lines) if lines.is_empty() => crate::i18n::tr("No updates yet.").to_owned(),
        Updates::Loaded(lines) => lines.join("\n"),
    }
}

/// One line per text message, oldest first, from a backward page of thread
/// relations (newest first, as the server returns them): the run Agent's own
/// progress as "• step", and what people wrote in that thread as
/// "• name: text", since the room shows no other way into the thread
/// (integration review). Other Agents' messages are not this run's.
pub fn lines(events: &[Value], agent: &str, is_agent: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "m.room.message"
            && e["content"]["m.relates_to"]["rel_type"] != "m.replace"
            && e["content"]["msgtype"].as_str().is_some_and(|t| t == "m.text" || t == "m.notice"))
        .filter_map(|e| {
            let sender = e["sender"].as_str()?;
            let person = sender != agent && !is_agent(sender);
            if sender != agent && !person {
                return None;
            }
            let line = e["content"]["body"].as_str()?.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("> "))?;
            let mut short: String = line.chars().take(LINE_CHARS).collect();
            if line.chars().count() > LINE_CHARS {
                short.push('…');
            }
            Some(if person { format!("• {}: {short}", person_name(sender)) } else { format!("• {short}") })
        })
        .take(MAX_UPDATES)
        .collect();
    out.reverse();
    out
}

/// A person's label: the Matrix ID's localpart.
fn person_name(sender: &str) -> &str {
    sender.strip_prefix('@').and_then(|s| s.split(':').next()).unwrap_or(sender)
}

/// Whether a thread event may be shown as an update: in a room known to be
/// unencrypted, a plaintext one; otherwise only an event the SDK decrypted
/// from its claimed sender's device, the rule run cards are admitted by
/// (`agent_questions::authentic`). A plaintext event claiming the Agent's ID
/// in an encrypted room (or one whose state is unknown) showed fabricated
/// progress (integration GPT review, Nov 19).
pub fn authentic_update(event: &Value, info: Option<&EncryptionInfo>, plaintext_room: bool) -> bool {
    let Some(sender) = event["sender"].as_str() else { return false };
    match info {
        None => plaintext_room && event["type"] != "m.room.encrypted",
        Some(info) => ::hagency::agent_questions::authentic(info, sender),
    }
}

fn is_agent_id(sender: &str) -> bool {
    matrix_sdk::ruma::UserId::parse(sender).is_ok_and(|u| crate::profile::profile_route::is_agent(&u))
}

fn load(key: Key, count: usize) {
    let ticket = {
        let mut store = store();
        store.next += 1;
        let ticket = store.next;
        store.latest.insert(key.clone(), ticket);
        ticket
    };
    let Some(client) = crate::sliding_sync::get_client() else {
        finish(&key, ticket, count, Updates::Failed);
        return;
    };
    crate::sliding_sync::spawn_async_task(async move {
        let read = async {
            let room = client.get_room(&OwnedRoomId::try_from(key.1.as_str()).ok()?)?;
            let root = OwnedEventId::try_from(key.2.as_str()).ok()?;
            let options = RelationsOptions {
                include_relations: IncludeRelations::RelationsOfType(RelationType::Thread),
                limit: UInt::new(MAX_UPDATES as u64),
                ..Default::default()
            };
            // Unknown encryption state: only decrypted, authentic events count.
            let state = room.latest_encryption_state().await.ok()?;
            let plaintext = !state.is_unknown() && !state.is_encrypted();
            let page = room.relations(root, options).await.ok()?;
            // Decrypted events carry their clear content; undecryptable ones are left out.
            let events: Vec<Value> = page.chunk.iter().filter_map(|e| {
                let value: Value = serde_json::from_str(e.kind.raw().json().get()).ok()?;
                authentic_update(&value, e.encryption_info().map(|info| &**info), plaintext).then_some(value)
            }).collect();
            Some(lines(&events, &key.3, is_agent_id))
        };
        let updates = read.await.map_or(Updates::Failed, Updates::Loaded);
        finish(&key, ticket, count, updates);
    });
}

fn finish(key: &Key, ticket: u64, count: usize, updates: Updates) {
    {
        let mut store = store();
        // Superseded (a newer load, a reopen) or another account: dropped.
        if store.latest.get(key) != Some(&ticket) || key.0 != account() {
            return;
        }
        // Closed meanwhile: the answer is dropped, the card stays closed.
        let Some(open) = store.open.get_mut(key) else { return };
        *open = updates;
        store.loaded_for.insert(key.clone(), count);
    }
    makepad_widgets::Cx::post_action(RunUpdatesChanged { room: key.1.clone(), thread_root: key.2.clone() });
}

#[cfg(test)]
#[path = "agent_run_updates_tests.rs"]
mod tests;
