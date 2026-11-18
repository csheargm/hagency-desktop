//! "N updates" under an Agent's answer opens in place (ADR 0023): the run's
//! progress messages, which the Agent keeps in a thread under the request,
//! are listed inside the card in the main timeline. Nobody has to open the
//! thread to see what the Agent did, and the room timeline stays one answer
//! per request.
use matrix_sdk::{
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

type Key = (String, String);
#[derive(Default)]
struct Store {
    open: HashMap<Key, Updates>,
    /// The reply count each load answered, so a newer update reloads.
    loaded_for: HashMap<Key, usize>,
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);
fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|p| p.into_inner())
}

/// The updates shown for a card, or `None` while they are closed.
pub fn shown(room: &str, thread_root: &str) -> Option<Updates> {
    store().open.get(&(room.to_owned(), thread_root.to_owned())).cloned()
}

/// Opens or closes a card's updates; `count` is the thread's reply count.
pub fn toggle(room: &str, thread_root: &str, count: usize) {
    let key = (room.to_owned(), thread_root.to_owned());
    let opened = {
        let mut store = store();
        if store.open.remove(&key).is_some() {
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

/// Reloads open updates whose thread grew since they were read.
pub fn follow(room: &str, thread_root: &str, count: usize) {
    let key = (room.to_owned(), thread_root.to_owned());
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

/// One line per text message, oldest first, from a backward page of
/// thread relations (newest first, as the server returns them).
pub fn lines(events: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "m.room.message" && e["content"]["m.relates_to"]["rel_type"] != "m.replace")
        .filter_map(|e| e["content"]["body"].as_str())
        .filter_map(|body| body.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|line| {
            let mut short: String = line.chars().take(LINE_CHARS).collect();
            if line.chars().count() > LINE_CHARS {
                short.push('…');
            }
            format!("• {short}")
        })
        .take(MAX_UPDATES)
        .collect();
    out.reverse();
    out
}

fn load(key: Key, count: usize) {
    let Some(client) = crate::sliding_sync::get_client() else {
        finish(&key, count, Updates::Failed);
        return;
    };
    crate::sliding_sync::spawn_async_task(async move {
        let read = async {
            let room = client.get_room(&OwnedRoomId::try_from(key.0.as_str()).ok()?)?;
            let root = OwnedEventId::try_from(key.1.as_str()).ok()?;
            let options = RelationsOptions {
                include_relations: IncludeRelations::RelationsOfType(RelationType::Thread),
                limit: UInt::new(MAX_UPDATES as u64),
                ..Default::default()
            };
            let page = room.relations(root, options).await.ok()?;
            // Decrypted events carry their clear content; undecryptable ones are left out.
            let events: Vec<Value> = page.chunk.iter().filter_map(|e| serde_json::from_str(e.kind.raw().json().get()).ok()).collect();
            Some(lines(&events))
        };
        let updates = read.await.map_or(Updates::Failed, Updates::Loaded);
        finish(&key, count, updates);
    });
}

fn finish(key: &Key, count: usize, updates: Updates) {
    {
        let mut store = store();
        // Closed meanwhile: the answer is dropped, the card stays closed.
        let Some(open) = store.open.get_mut(key) else { return };
        *open = updates;
        store.loaded_for.insert(key.clone(), count);
    }
    makepad_widgets::Cx::post_action(RunUpdatesChanged { room: key.0.clone(), thread_root: key.1.clone() });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(body: &str) -> Value {
        json!({"type":"m.room.message","content":{"msgtype":"m.text","body":body}})
    }

    #[test]
    fn updates_read_oldest_first_one_short_line_each() {
        let newest_first = vec![
            message("running cargo test\nmore output"),
            json!({"type":"m.room.message","content":{"body":"* edited","m.relates_to":{"rel_type":"m.replace","event_id":"$a"}}}),
            json!({"type":"m.reaction","content":{}}),
            message(&"x".repeat(LINE_CHARS + 3)),
            message("reading the issue list"),
        ];
        let shown = lines(&newest_first);
        assert_eq!(shown.len(), 3);
        assert_eq!(shown[0], "• reading the issue list");
        assert!(shown[1].ends_with('…') && shown[1].chars().count() == LINE_CHARS + 3);
        assert_eq!(shown[2], "• running cargo test");
    }

    #[test]
    fn the_open_state_has_words_for_every_case() {
        assert_eq!(text(&Updates::Loaded(vec!["• a".into(), "• b".into()])), "• a\n• b");
        for key in ["Loading updates…", "Could not load the updates. Close and open them to try again.", "No updates yet.", "Hide updates"] {
            assert_ne!(crate::i18n::text_for(crate::i18n::Language::Chinese, key), key, "{key} has no Chinese text");
        }
    }

    /// A card closed while its updates load stays closed; a reopened one
    /// shows what was read.
    #[test]
    fn closing_while_loading_drops_the_answer() {
        let key = ("!r:test".to_owned(), "$closing".to_owned());
        store().open.insert(key.clone(), Updates::Loading);
        store().open.remove(&key);
        finish(&key, 2, Updates::Loaded(vec!["• late".into()]));
        assert_eq!(shown(&key.0, &key.1), None);
        store().open.insert(key.clone(), Updates::Loading);
        finish(&key, 2, Updates::Loaded(vec!["• now".into()]));
        assert_eq!(shown(&key.0, &key.1), Some(Updates::Loaded(vec!["• now".into()])));
    }
}
