//! WeChat-style local chat visibility. These preferences never leave a room or redact events.
use std::{cell::RefCell, collections::BTreeMap, sync::Mutex};
use makepad_widgets::*;
use ruma::{OwnedRoomId, OwnedUserId, RoomId};
use serde::{Deserialize, Serialize};
use crate::{
    app::ConfirmDeleteAction,
    shared::{
        confirmation_modal::ConfirmationModalContent,
        popup_list::{enqueue_popup_notification, PopupKind},
    },
    sliding_sync::current_user_id,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ChatState {
    hidden_through: Option<u64>,
    cleared_through: Option<u64>,
}
impl ChatState {
    fn observe(&mut self, timestamp: u64) -> bool {
        if self.hidden_through.is_some_and(|t| timestamp > t) {
            self.hidden_through = None;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct Store {
    owner: Option<OwnedUserId>,
    rooms: BTreeMap<OwnedRoomId, ChatState>,
}
static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let owner = current_user_id();
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(Store::default);
    if store.owner != owner {
        store.rooms = owner
            .as_ref()
            .and_then(|id| {
                std::fs::read(
                    crate::persistence::persistent_state_dir(id).join("chat_visibility.json"),
                )
                .ok()
            })
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        store.owner = owner;
    }
    f(store)
}

impl Store {
    fn save(&self) -> Result<(), String> {
        use std::io::Write;
        let owner = self.owner.as_ref().ok_or(crate::i18n::tr("Sign in first."))?;
        let dir = crate::persistence::persistent_state_dir(owner);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("chat_visibility.json");
        let temporary = dir.join("chat_visibility.json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&self.rooms).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(temporary, path).map_err(|e| e.to_string())
    }
}

pub fn is_hidden(room: &RoomId) -> bool {
    with_store(|s| {
        s.rooms
            .get(room)
            .is_some_and(|r| r.hidden_through.is_some())
    })
}
pub fn cleared_through(room: &RoomId) -> Option<u64> {
    with_store(|s| s.rooms.get(room).and_then(|r| r.cleared_through))
}

/// New activity restores a hidden chat; the history cutoff remains in effect.
pub fn observe(room: &RoomId, timestamp: u64) -> bool {
    with_store(|s| {
        let Some(state) = s.rooms.get_mut(room) else {
            return false;
        };
        if state.observe(timestamp) {
            if let Err(e) = s.save() {
                warning!("Could not persist restored chat visibility: {e}");
            }
            true
        } else {
            false
        }
    })
}

#[derive(Clone, Debug)]
pub struct ChatVisibilityChanged;
#[derive(Clone, Debug)]
pub struct ChatSwipeOpened(pub OwnedRoomId);

pub fn restore(room: &RoomId) -> bool {
    with_store(|s| {
        let Some(state) = s.rooms.get_mut(room) else {
            return false;
        };
        if state.hidden_through.take().is_none() {
            return false;
        }
        if let Err(e) = s.save() {
            warning!("Could not save chat visibility: {e}");
        }
        true
    })
}

pub fn hide(cx: &mut Cx, room: OwnedRoomId, timestamp: u64, clear: bool) {
    let result = with_store(|s| {
        let previous = s.rooms.clone();
        let state = s.rooms.entry(room).or_default();
        state.hidden_through = Some(timestamp);
        if clear {
            state.cleared_through = Some(state.cleared_through.unwrap_or(0).max(timestamp));
        }
        if let Err(error) = s.save() {
            s.rooms = previous;
            return Err(error);
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            cx.action(ChatVisibilityChanged);
            cx.redraw_all();
            enqueue_popup_notification(
                if clear {
                    crate::i18n::tr("Chat removed on this device. You are still a member.")
                } else {
                    crate::i18n::tr("Chat hidden. Find it in Contacts or search; new activity brings it back.")
                },
                PopupKind::Success,
                Some(4.0),
            );
        }
        Err(error) => enqueue_popup_notification(
            crate::i18n::format("Could not save chat visibility: {error}", &[("error", (error).to_string())]),
            PopupKind::Error,
            Some(5.0),
        ),
    }
}

pub fn confirm_delete(cx: &mut Cx, room: OwnedRoomId, timestamp: u64) {
    let owner = current_user_id();
    let body = if super::chat_menu_routes::own_agent_dm(&room).is_some() {
        super::chat_menu_routes::agent_chat_delete_body()
    } else {
        crate::i18n::tr("Remove this chat and clear its existing history from Rinx on this device? You stay in the conversation. Messages remain on the server and other devices.")
    };
    cx.action(ConfirmDeleteAction::Show(RefCell::new(Some(ConfirmationModalContent {
        title_text: crate::i18n::tr("Delete Chat").into(),
        body_text: body.into(),
        accept_button_text: Some(crate::i18n::tr("Delete Chat").into()),
        on_accept_clicked: Some(Box::new(move |cx| {
            if current_user_id() == owner { hide(cx, room.clone(), timestamp, true); }
        })),
        ..Default::default()
    }))));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_sync_cannot_restore_a_hidden_chat_but_new_activity_can() {
        let mut state = ChatState {
            hidden_through: Some(100),
            cleared_through: Some(100),
        };
        for t in [0, 99, 100] {
            assert!(!state.observe(t));
        }
        assert!(state.observe(101));
        assert_eq!(state.hidden_through, None);
        assert_eq!(state.cleared_through, Some(100));
        assert!(!state.observe(102));
    }
    #[test]
    fn persistence_preserves_hide_and_delete_as_distinct_states() {
        let original = BTreeMap::from([
            (
                ruma::room_id!("!hidden:example.org").to_owned(),
                ChatState {
                    hidden_through: Some(100),
                    cleared_through: None,
                },
            ),
            (
                ruma::room_id!("!deleted:example.org").to_owned(),
                ChatState {
                    hidden_through: Some(200),
                    cleared_through: Some(200),
                },
            ),
        ]);
        let copy: BTreeMap<OwnedRoomId, ChatState> =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(
            copy[ruma::room_id!("!hidden:example.org")].cleared_through,
            None
        );
        assert_eq!(
            copy[ruma::room_id!("!deleted:example.org")].cleared_through,
            Some(200)
        );
    }
}
