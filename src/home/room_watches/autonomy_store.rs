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
    // Allocated and registered under one lock, so tickets register in order (GPT review).
    let mut reads = READS.lock().unwrap_or_else(|p| p.into_inner());
    let ticket = NEXT_READ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    reads.insert(key.clone(), ticket);
    drop(reads);
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_insert(Shown::Loading);
    ticket
}
/// Shows what the read `ticket` found, unless a newer read of `room` started since.
/// The check and the write happen under the read tickets' lock, so a newer
/// read cannot start and show its state in between (GPT review).
fn finish(account: String, room: &RoomId, ticket: u64, shown: Shown) {
    let key = (account, room.to_owned());
    let reads = READS.lock().unwrap_or_else(|p| p.into_inner());
    if reads.get(&key) != Some(&ticket) {
        return;
    }
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).insert(key, shown);
    drop(reads);
    Cx::post_action(AutonomyChanged { room_id: room.to_owned() });
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


#[cfg(test)]
mod tests {
    use super::*;

    /// GPT review (P2): while an older read's reply is being shown, no newer
    /// read can start (and finish first): the ticket check and the write are one step.
    #[test]
    fn showing_a_read_holds_the_tickets_until_written() {
        let room = matrix_sdk::ruma::owned_room_id!("!atomic:x");
        let ticket = begin("@atomic:x", &room);
        let shown = SHOWN.lock().unwrap_or_else(|p| p.into_inner());
        let writer = { let room = room.clone(); std::thread::spawn(move || finish("@atomic:x".to_owned(), &room, ticket, Shown::Failed("old".into()))) };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(READS.try_lock().is_err(), "a newer read must wait until the older one is written");
        drop(shown);
        writer.join().unwrap();
        let newer = begin("@atomic:x", &room);
        finish("@atomic:x".to_owned(), &room, ticket, Shown::Failed("stale".into()));
        finish("@atomic:x".to_owned(), &room, newer, Shown::Loading);
        let key = ("@atomic:x".to_owned(), room.clone());
        assert!(matches!(SHOWN.lock().unwrap().get(&key), Some(Shown::Loading)), "the older reply never overwrites the newer");
    }

    /// GPT review (P2): a ticket is allocated only once the tickets' lock is
    /// held, so two reads starting together register in ticket order.
    #[test]
    fn a_ticket_is_allocated_under_the_tickets_lock() {
        let room = matrix_sdk::ruma::owned_room_id!("!order:x");
        let reads = READS.lock().unwrap_or_else(|p| p.into_inner());
        let before = NEXT_READ.load(std::sync::atomic::Ordering::Relaxed);
        let starter = std::thread::spawn(move || begin("@order:x", &room));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(NEXT_READ.load(std::sync::atomic::Ordering::Relaxed), before, "no ticket is taken before the lock");
        drop(reads);
        assert!(starter.join().unwrap() >= before);
    }
}
