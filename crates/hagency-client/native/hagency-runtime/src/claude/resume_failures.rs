//! Consecutive failed starts of resumed native sessions, counted in memory for
//! this host process only. A failed start that does not report the saved
//! context missing is no evidence about that context, so the saved session is
//! kept. One that keeps failing before any input is replaced by a fresh
//! session once this count reaches `LIMIT`; a host restart begins it again.
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard, PoisonError},
};

/// The consecutive failed resumed start, before any input, on which a fresh
/// session replaces the saved one: the third. Earlier ones only keep it.
pub const LIMIT: u32 = 3;
/// Saved sessions counted at once. The oldest count is forgotten first, which
/// can only delay that session's fresh start.
const MAX_TRACKED: usize = 256;
static FAILURES: Mutex<VecDeque<(String, u32)>> = Mutex::new(VecDeque::new());

fn failures() -> MutexGuard<'static, VecDeque<(String, u32)>> {
    FAILURES.lock().unwrap_or_else(PoisonError::into_inner)
}
/// Count one more failed resumed start of this saved session and return its
/// consecutive total. The host names the saved session; an unsupported
/// installation is never counted, because a fresh start meets the same thing.
pub fn record(saved: &str) -> u32 {
    let mut failures = failures();
    if let Some((_, count)) = failures.iter_mut().find(|(id, _)| id == saved) {
        *count = count.saturating_add(1);
        return *count;
    }
    if failures.len() >= MAX_TRACKED {
        failures.pop_front();
    }
    failures.push_back((saved.to_owned(), 1));
    1
}
/// A resume that initialized, or the discard of the saved session, ends the run.
pub fn clear(saved: &str) {
    failures().retain(|(id, _)| id != saved);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_failures_are_consecutive_per_saved_session_and_bounded() {
        let key = |n: usize| format!("resume-failures-unit-{n}");
        assert_eq!(record(&key(0)), 1);
        assert_eq!(record(&key(0)), 2);
        assert_eq!(record(&key(1)), 1, "another saved session has its own count");
        assert_eq!(record(&key(0)), LIMIT);
        clear(&key(0));
        assert_eq!(record(&key(0)), 1, "a cleared count starts again");
        clear(&key(0));
        clear(&key(1));
        // More sessions than the bound forget the oldest count; none is lost
        // in a way that brings a fresh start forward.
        assert_eq!(record(&key(2)), 1);
        for n in 3..3 + MAX_TRACKED {
            assert_eq!(record(&key(n)), 1);
        }
        assert!(failures().len() <= MAX_TRACKED);
        assert_eq!(record(&key(2)), 1, "the forgotten session counts from one");
        for n in 2..3 + MAX_TRACKED {
            clear(&key(n));
        }
    }
}
