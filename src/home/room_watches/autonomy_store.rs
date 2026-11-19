//! What a room's agent work section and header badge show (ADR 0030), per
//! signed-in account and room: the newest read from the homeserver and the
//! note of the last action; split from `autonomy.rs` for R4.
use super::autonomy::Shown;
use crate::hagency::room_autonomy::{lines, server, state::RoomState};
use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Posted when a room's agent work was read or changed.
#[derive(Clone, Debug)]
pub struct AutonomyChanged {
    pub room_id: OwnedRoomId,
}

type Key = (String, OwnedRoomId);
static SHOWN: LazyLock<Mutex<HashMap<Key, Shown>>> = LazyLock::new(Default::default);
static NOTES: LazyLock<Mutex<HashMap<Key, String>>> = LazyLock::new(Default::default);

use super::account;
pub(super) fn set(account: String, room: &RoomId, shown: Shown) {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), shown);
    Cx::post_action(AutonomyChanged { room_id: room.to_owned() });
}
/// What the section (and the room header's badge) shows for `room`.
pub fn current(room: &RoomId) -> Shown {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned().unwrap_or(Shown::Loading)
}
/// The room header's badge for `room`, once its agent work was read.
pub fn badge_of(room: &RoomId) -> Option<&'static str> {
    match current(room) {
        Shown::Ready(state) => lines::badge(&state),
        Shown::Loading | Shown::Failed(_) => None,
    }
}
pub(super) fn note(room: &RoomId) -> Option<String> {
    NOTES.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned()
}
pub(super) fn set_note(account: String, room: &RoomId, text: String) {
    NOTES.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), text);
    Cx::post_action(AutonomyChanged { room_id: room.to_owned() });
}

/// Whether `room`'s agent work is read on this desktop (shown in its header
/// or Status tab), counted from the start of its first read.
pub(super) fn was_read(room: &RoomId) -> bool {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&(account(), room.to_owned()))
}

/// The newest read per room: a reply from an older one is dropped, so replies
/// finishing out of order never show an older state (GPT review).
static READS: LazyLock<Mutex<HashMap<Key, u64>>> = LazyLock::new(Default::default);
static NEXT_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A ticket for a read of `room` starting now; the room counts as read from here.
fn begin(account: &str, room: &RoomId) -> u64 {
    let key = (account.to_owned(), room.to_owned());
    let ticket = NEXT_READ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    READS.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), ticket);
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_insert(Shown::Loading);
    ticket
}
/// Shows what the read `ticket` found, unless a newer read of `room` started since.
fn finish(account: String, room: &RoomId, ticket: u64, shown: Shown) {
    if READS.lock().unwrap_or_else(|p| p.into_inner()).get(&(account.clone(), room.to_owned())) == Some(&ticket) {
        set(account, room, shown);
    }
}

/// A fresh read of `room` another part of the desktop starts (the loop's
/// check, the Safeguards card): its ticket, when `room` is shown here.
pub fn observe(room: &RoomId) -> Option<(String, u64)> {
    let account = account();
    was_read(room).then(|| (account.clone(), begin(&account, room)))
}
/// What that read found, shown unless a newer read started since.
pub fn observed(ticket: Option<(String, u64)>, room: &RoomId, read: &Result<RoomState, String>) {
    if let Some((account, ticket)) = ticket {
        finish(account, room, ticket, read.clone().map_or_else(Shown::Failed, Shown::Ready));
    }
}

/// Read `room`'s agent work from the homeserver; from then on also on each
/// change another member makes ([`super::autonomy_watch`]).
pub fn refresh(room: &RoomId) {
    let account = account();
    let client = crate::sliding_sync::get_client();
    if let Some(client) = &client {
        super::autonomy_watch::attach(client);
    }
    let Some(joined) = client.and_then(|c| c.get_room(room)) else {
        return set(account, room, Shown::Failed(crate::i18n::tr("Room not found").to_owned()));
    };
    crate::sliding_sync::spawn_async_task(async move { read_into(account, &joined).await });
}

pub(super) async fn read_into(account: String, joined: &matrix_sdk::Room) {
    let ticket = begin(&account, joined.room_id());
    let shown = server::read_events(joined).await.map(|(state, _)| state).map_or_else(Shown::Failed, Shown::Ready);
    finish(account, joined.room_id(), ticket, shown);
}

