//! The short exclusive lock around a runtime journal file (intents, service
//! options). Writers hold it only for one read-modify-replace, so a writer
//! that finds it taken waits briefly instead of failing: startup recovery
//! starts one Agent service while the previous one's Room children are still
//! saving their own intents, and an immediate failure there surfaced as
//! `runtime_profile_unavailable` for the second Agent. A lock still held
//! after the bound is a stuck or foreign writer and still fails closed.
use super::RuntimeError;
use std::{fs::File, path::Path, time::{Duration, Instant}};

/// Longest wait for another writer's single journal update.
const WAIT: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(5);

/// Open `lock_path` (creating it owner-only) and hold it exclusively.
pub(super) fn acquire(lock_path: &Path) -> Result<File, RuntimeError> {
    let lock = hagency_store::private::open(lock_path, true)
        .or_else(|_| hagency_store::private::open(lock_path, false))
        .map_err(|_| RuntimeError::Profile)?;
    let deadline = Instant::now() + WAIT;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            // WouldBlock: another writer's update is in flight; wait for it.
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(POLL),
            // Held past the bound, or the lock itself failed: fail closed.
            Err(_) => return Err(RuntimeError::Profile),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_writer_waits_for_a_short_update_and_fails_closed_on_a_stuck_one() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.lock");
        let held = acquire(&path).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        assert!(acquire(&path).is_ok(), "a brief concurrent update must not fail the start");
        releaser.join().unwrap();
        let stuck = acquire(&path).unwrap();
        let started = Instant::now();
        assert!(matches!(acquire(&path), Err(RuntimeError::Profile)));
        assert!(started.elapsed() >= WAIT);
        drop(stuck);
    }
}
