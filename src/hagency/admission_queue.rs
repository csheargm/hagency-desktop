//! A shared limit on loaded run card admissions (GPT review P2). At sign-in
//! every room whose latest event is a run card, and every opened timeline,
//! started its own admission task, each making up to two event fetches with
//! no limit across them; a failed fetch was asked again at once. Admissions
//! now take one of `MAX_ADMISSIONS` slots (the rest wait their turn), and a
//! failed try waits before the next one, without holding a slot meanwhile.

use std::future::Future;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Admission tries (each up to two fetches) running at once, process-wide.
pub(super) const MAX_ADMISSIONS: usize = 4;
static ADMISSIONS: Semaphore = Semaphore::const_new(MAX_ADMISSIONS);

/// Runs `admission` once a slot is free, queued behind the ones running.
pub(super) async fn limited<F: Future>(admission: F) -> F::Output {
    // The semaphore is a static that is never closed, so acquiring cannot
    // fail; if it ever did, running unthrottled beats never admitting.
    let _slot = ADMISSIONS.acquire().await.ok();
    admission.await
}

/// Runs `admit` for each of `items` in turn, each in its own slot (so queued
/// admissions get turns between items); whether any returned true.
pub(super) async fn each_limited<'a, T, F, Fut>(items: &'a [T], mut admit: F) -> bool
where
    F: FnMut(&'a T) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut changed = false;
    for item in items {
        changed |= limited(admit(item)).await;
    }
    changed
}

/// Tries `attempt` after each pause in `pauses` until it yields a value, each
/// try in a slot (`limited`), the pauses outside one. None after the last try.
pub(super) async fn with_backoff<T, F, Fut>(pauses: &[Duration], mut attempt: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    for pause in pauses {
        tokio::time::sleep(*pause).await;
        if let Some(value) = limited(attempt()).await {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
#[path = "admission_queue_tests.rs"]
mod tests;
