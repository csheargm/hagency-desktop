//! A room's level writes on this desktop, one at a time per room (GPT
//! review P2): the Status tab's choices, Resume and the migration's level all
//! take the same per-room lock and read the room again under it, so a level
//! one of them saved (an Off) is never overwritten from an earlier read. A
//! choice a newer one superseded while it waited is dropped, so the latest
//! choice is written last.
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

/// The newest level choice per room.
static CHOICES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<OwnedRoomId, u64>>> = std::sync::LazyLock::new(Default::default);
static WRITES: crate::home::call_power_levels::RoomLocks = crate::home::call_power_levels::RoomLocks::new();

/// A new level choice in `room`: its ticket, the room's latest from now.
pub fn choose(room: &RoomId) -> u64 {
    let mut choices = CHOICES.lock().unwrap_or_else(|p| p.into_inner());
    let ticket = choices.get(room).copied().unwrap_or_default() + 1;
    choices.insert(room.to_owned(), ticket);
    ticket
}

/// `write` once `room`'s earlier level writes finished; it must read the
/// room itself, so it reads under the lock.
pub async fn serialized<T>(room: &RoomId, write: impl std::future::Future<Output = T>) -> T {
    let lock = WRITES.of(room);
    let _one = lock.lock().await;
    write.await
}

/// [`serialized`] for choice `ticket`, unless a newer choice was made
/// meanwhile (`None`: superseded, not written).
pub async fn latest_only<T>(room: &RoomId, ticket: u64, write: impl std::future::Future<Output = T>) -> Option<T> {
    serialized(room, async {
        let latest = CHOICES.lock().unwrap_or_else(|p| p.into_inner()).get(room).copied();
        if latest != Some(ticket) {
            return None;
        }
        Some(write.await)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hagency::issue_watch::safeguards::room_autonomy::Level;
    use std::sync::Mutex;

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    /// GPT review (P2): Assist → Autonomous → Off while the Autonomous write is
    /// pending: Off is written last, and a choice superseded while it waited is dropped.
    #[test]
    fn the_latest_level_choice_is_written_last() {
        let room = matrix_sdk::ruma::owned_room_id!("!latest:x");
        let log = Mutex::new(Vec::new());
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let autonomous = choose(&room);
        let first = latest_only(&room, autonomous, async { held.await.unwrap(); log.lock().unwrap().push(Level::Autonomous) });
        let rest = async {
            let stale = choose(&room);
            let off = choose(&room);
            let stale_write = latest_only(&room, stale, async { log.lock().unwrap().push(Level::Assist) });
            let off_write = latest_only(&room, off, async { log.lock().unwrap().push(Level::Off) });
            release.send(()).unwrap();
            tokio::join!(stale_write, off_write)
        };
        let (first, (stale, off)) = run(async { tokio::join!(first, rest) });
        assert_eq!((first, stale, off), (Some(()), None, Some(())), "the superseded choice is never written");
        assert_eq!(*log.lock().unwrap(), vec![Level::Autonomous, Level::Off], "the latest choice lands last");
    }

    /// GPT review (P2): an Off chosen while Resume waits for the lock is
    /// written after it; and Resume, re-reading under the lock, keeps an Off
    /// written before it (it never writes the level it read earlier).
    #[test]
    fn an_off_landing_during_resume_is_never_overwritten() {
        let room = matrix_sdk::ruma::owned_room_id!("!resume:x");
        let level = Mutex::new(Level::Autonomous);
        // Resume: reads the level under the lock, then writes it back with the pauses cleared.
        let resume = || serialized(&room, async {
            let read = *level.lock().unwrap();
            tokio::task::yield_now().await;
            *level.lock().unwrap() = read;
        });
        let off = choose(&room);
        let off_write = latest_only(&room, off, async { *level.lock().unwrap() = Level::Off });
        run(async { tokio::join!(off_write, resume()) });
        assert_eq!(*level.lock().unwrap(), Level::Off, "Resume ran after the Off and read it");
        *level.lock().unwrap() = Level::Autonomous;
        let off = choose(&room);
        let off_write = latest_only(&room, off, async { *level.lock().unwrap() = Level::Off });
        run(async { tokio::join!(resume(), off_write) });
        assert_eq!(*level.lock().unwrap(), Level::Off, "an Off chosen during Resume lands after it");
    }
}
