//! Background work for the Memory list and the proposal cards: reload a room's
//! memory, and run one write. A write always re-reads the room first, so the
//! revision, the storage and the power level are current when it is planned.
//! Every failure is kept for the panel and shown as a popup.
use super::io::{self, Loaded};
use super::model::{Write, WriteError};
use super::store::{self, MemoryChanged, Progress};
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use crate::sliding_sync::{current_user_id, get_client, spawn_async_task};
use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

/// Builds the write from the freshly loaded room, or says why it cannot.
pub type Planner = Box<dyn FnOnce(&Loaded) -> Result<Write, WriteError> + Send>;

fn changed(room_id: &RoomId) {
    Cx::post_action(MemoryChanged { room_id: room_id.to_owned() });
}
fn fail(room_id: &RoomId, subject: Option<&str>, message: String) {
    {
        let mut store = store::lock();
        store.set_error(room_id, message.clone());
        if let Some(subject) = subject {
            store.set_progress(room_id, subject, Some(Progress::Failed(message.clone())));
        }
    }
    enqueue_popup_notification(message, PopupKind::Error, None);
    changed(room_id);
}
fn signed_out() -> String {
    crate::i18n::tr("Sign in to use room memory.").to_owned()
}

/// Reloads the room's memory into the store.
pub fn refresh(room_id: &RoomId) {
    let (Some(client), Some(owner)) = (get_client(), current_user_id()) else { return };
    let room_id = room_id.to_owned();
    spawn_async_task(async move {
        let result = io::load(&client, &room_id).await;
        if current_user_id().as_ref() != Some(&owner) {
            return;
        }
        match result {
            Ok(loaded) => store::lock().set_loaded(loaded),
            Err(message) => {
                store::lock().forget_requests(&room_id);
                store::lock().set_error(&room_id, message);
            }
        }
        changed(&room_id);
    });
}

/// Loads a room's memory the first time anything asks for it.
pub fn ensure_loaded(room_id: &RoomId) {
    if store::lock().first_request(room_id) {
        refresh(room_id);
    }
}

/// Runs one write for `subject` (an entry id, or a proposal id for a card).
pub fn submit(room_id: &RoomId, subject: &str, plan: Planner) {
    let (Some(client), Some(owner)) = (get_client(), current_user_id()) else {
        return fail(room_id, Some(subject), signed_out());
    };
    {
        let mut store = store::lock();
        store.clear_error(room_id);
        store.set_progress(room_id, subject, Some(Progress::Working));
    }
    changed(room_id);
    let (room_id, subject) = (room_id.to_owned(), subject.to_owned());
    spawn_async_task(async move {
        let outcome = run(&client, &room_id, plan).await;
        if current_user_id().as_ref() != Some(&owner) {
            return;
        }
        match outcome {
            Ok(reloaded) => finish(&room_id, &subject, reloaded),
            Err(message) => fail(&room_id, Some(&subject), message),
        }
    });
}

fn finish(room_id: &OwnedRoomId, subject: &str, reloaded: Option<Loaded>) {
    {
        let mut store = store::lock();
        store.set_progress(room_id, subject, None);
        match reloaded {
            Some(loaded) => store.set_loaded(loaded),
            None => store.set_error(room_id, crate::i18n::tr("Saved. The list updates when the room syncs.").to_owned()),
        }
    }
    changed(room_id);
}

/// `None` means the write went through but the reload failed.
async fn run(client: &matrix_sdk::Client, room_id: &RoomId, plan: Planner) -> Result<Option<Loaded>, String> {
    let loaded = io::load(client, room_id).await?;
    let write = plan(&loaded).map_err(|error| crate::i18n::tr(error.message()).to_owned())?;
    io::send(client, room_id, loaded.storage, &write).await?;
    Ok(io::load(client, room_id).await.ok())
}

/// Hides a proposal card on this device.
pub fn dismiss(room_id: &RoomId, proposal: &str) {
    let saved = store::lock().dismiss(room_id, proposal);
    if let Err(error) = saved {
        let message = crate::i18n::format("Could not remember the dismissal: {reason}", &[("reason", error.to_string())]);
        enqueue_popup_notification(message, PopupKind::Error, None);
    }
    changed(room_id);
}
