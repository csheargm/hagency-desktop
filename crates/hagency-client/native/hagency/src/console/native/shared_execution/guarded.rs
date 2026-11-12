//! A retained phase boundary for the original, successor and budget drivers.
//! Constructing and polling large SDK/approval futures inline in each driver
//! stacks their debug frames until an ordinary Tokio worker can overflow.
use super::{NativeError, failure};
use std::{future::Future, pin::Pin, time::Instant};
use tokio::sync::watch;

type Phase<'a, T> = Pin<Box<dyn Future<Output = Result<T, NativeError>> + Send + 'a>>;

/// Defer construction until the cancellation/deadline gate has been checked.
/// This is the same awaited phase, never a detached task: dropping its caller
/// drops the active child and leaves the retained process custody to drain.
#[inline(never)]
pub(super) fn guarded<'a, T: 'a, M, F>(
    cancel: &'a mut watch::Receiver<bool>,
    until: Instant,
    make: M,
) -> Phase<'a, T>
where
    M: FnOnce() -> F + Send + 'a,
    F: Future<Output = Result<T, NativeError>> + Send + 'a,
{
    Box::pin(async move {
        if *cancel.borrow() || Instant::now() >= until {
            return Err(failure(409, "shared_execution_closed"));
        }
        // Creation is inside this heap-owned phase rather than the parent
        // driver's state machine; cancellation still wins the biased select.
        let future = make();
        tokio::select! {biased;
            _ = cancel.changed() => Err(failure(409, "shared_execution_closed")),
            _ = tokio::time::sleep_until(until.into()) => Err(failure(409, "shared_execution_expired")),
            result = future => result,
        }
    })
}

/// Runs a lease renewal while the pending provider operation stays polled.
///
/// A provider call (`thread/resume`, `turn/start`) carries its own wall-clock
/// bound. A renewal usually takes a fraction of a second, but the one that
/// performs the daemon's token refresh takes several: awaited on its own it
/// left the call unpolled for that long, and the call timed out the moment it
/// was polled again, which ended the run and left its message unanswered.
///
/// The renewal always runs to its end, never dropped half-way: its epoch may
/// already be applied on the server. If the operation finished meanwhile its
/// result is handed back; it is not polled again after that.
pub(super) async fn renew_beside<T, R, O>(
    renewal: R,
    mut operation: Pin<&mut O>,
) -> (Result<(), NativeError>, Option<T>)
where
    R: Future<Output = Result<(), NativeError>>,
    O: Future<Output = T>,
{
    tokio::pin!(renewal);
    let mut finished = None;
    loop {
        tokio::select! {biased;
            renewed = &mut renewal => return (renewed, finished),
            result = &mut operation, if finished.is_none() => finished = Some(result),
        }
    }
}

/// Whether a driver's wait for the central reply is over.
///
/// `reported` is `None` while the report itself is still being retried, and
/// `Some(reply state)` once the server holds the terminal receipt. The server
/// makes a reply only for completed work, so a recorded receipt of any other
/// state (a stopped or failed turn) has nothing left to deliver: waiting on it
/// would hold the process, and every later message, for ever.
pub(super) fn reply_settled(reported: Option<Option<&str>>, terminal_state: &str) -> bool {
    match reported {
        Some(Some("sent" | "blocked")) => true,
        Some(None) => terminal_state != "completed",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use std::time::Duration;

    #[test]
    fn reply_wait_ends_on_delivery_or_when_no_reply_can_exist() {
        assert!(reply_settled(Some(Some("sent")), "completed"));
        assert!(reply_settled(Some(Some("blocked")), "completed"));
        // Still being delivered, or the report is still being retried.
        assert!(!reply_settled(Some(Some("pending")), "completed"));
        assert!(!reply_settled(Some(None), "completed"));
        assert!(!reply_settled(None, "completed"));
        assert!(!reply_settled(None, "failed"));
        // A recorded stop or failure has no reply to wait for.
        assert!(reply_settled(Some(None), "failed"));
        assert!(reply_settled(Some(None), "unknown"));
    }

    /// The call's own bound (80 ms) is shorter than the renewal (300 ms) and
    /// its answer arrives at 20 ms: it must be answered, not timed out.
    #[tokio::test(start_paused = true)]
    async fn a_bounded_call_is_answered_during_a_slow_renewal() {
        let answer = Arc::new(tokio::sync::Notify::new());
        let wake = answer.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            wake.notify_one();
        });
        let call = async {
            tokio::time::timeout(Duration::from_millis(80), answer.notified())
                .await
                .map_err(|_| failure(409, "call_timed_out"))
        };
        tokio::pin!(call);
        let renewal = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        };
        let started = tokio::time::Instant::now();
        let (renewed, finished) = renew_beside(renewal, call.as_mut()).await;
        assert!(renewed.is_ok());
        assert!(finished.expect("the call finished during the renewal").is_ok());
        // The renewal was not cut short by the call's answer.
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    /// The control for the test above: the same call left unpolled for the
    /// length of the renewal has run out of time when it is polled again.
    #[tokio::test(start_paused = true)]
    async fn the_same_call_times_out_when_the_renewal_is_awaited_alone() {
        let answer = Arc::new(tokio::sync::Notify::new());
        let call = async {
            tokio::time::timeout(Duration::from_millis(80), async {
                // Like a reply that needs reading once it has arrived.
                tokio::time::sleep(Duration::from_millis(20)).await;
                tokio::task::yield_now().await;
                answer.notified().await
            })
            .await
            .map_err(|_| failure(409, "call_timed_out"))
        };
        tokio::pin!(call);
        // One poll, so the call's clock is running.
        tokio::select! {biased;
            _ = &mut call => panic!("the call cannot be answered yet"),
            _ = std::future::ready(()) => {}
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        answer.notify_one();
        assert_eq!(call.await.unwrap_err().code, "call_timed_out");
    }

    #[tokio::test(start_paused = true)]
    async fn a_renewal_that_ends_first_leaves_the_call_pending_and_usable() {
        let answer = Arc::new(tokio::sync::Notify::new());
        let call = async {
            answer.notified().await;
            7u8
        };
        tokio::pin!(call);
        let (renewed, finished) = renew_beside(async { Ok(()) }, call.as_mut()).await;
        assert!(renewed.is_ok());
        assert!(finished.is_none());
        answer.notify_one();
        assert_eq!(call.await, 7);
    }

    /// A finished call is not polled again (an `async` block would panic), and
    /// a failed renewal still hands back what the call returned.
    #[tokio::test(start_paused = true)]
    async fn a_failed_renewal_still_returns_the_finished_call() {
        let call = async { 7u8 };
        tokio::pin!(call);
        let renewal = async {
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(failure(409, "shared_execution_expired"))
        };
        let (renewed, finished) = renew_beside(renewal, call.as_mut()).await;
        assert_eq!(renewed.unwrap_err().code, "shared_execution_expired");
        assert_eq!(finished, Some(7));
    }

    #[tokio::test]
    async fn shared_phase_does_not_construct_after_retirement_or_expiry() {
        for retired in [false, true] {
            let (_send, mut cancel) = watch::channel(retired);
            let constructed = AtomicBool::new(false);
            let until = if retired { Instant::now()+Duration::from_secs(1) } else { Instant::now() };
            let result = guarded(&mut cancel, until, || {
                constructed.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }).await;
            assert_eq!(result.unwrap_err().code, "shared_execution_closed");
            assert!(!constructed.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn shared_phase_cancellation_drops_original_child_before_returning() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
        }
        let (send, mut cancel) = watch::channel(false);
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let wake = entered.clone();
        let task = tokio::spawn(async move {
            guarded(&mut cancel, Instant::now()+Duration::from_secs(5), || async move {
                let _original = Dropped(observed);
                wake.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            }).await
        });
        entered.notified().await;
        send.send(true).unwrap();
        assert_eq!(task.await.unwrap().unwrap_err().code, "shared_execution_closed");
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_phase_large_child_does_not_expand_driver_future() {
        async fn large_child() -> Result<u8, NativeError> {
            let retained = [7u8; 256 * 1024];
            tokio::task::yield_now().await;
            Ok(std::hint::black_box(&retained)[0])
        }
        assert!(std::mem::size_of_val(&large_child()) >= 256 * 1024);
        tokio::spawn(async {
            let (_send, mut cancel) = watch::channel(false);
            let phase = guarded(&mut cancel, Instant::now()+Duration::from_secs(5), large_child);
            assert!(std::mem::size_of_val(&phase) <= 2 * std::mem::size_of::<usize>());
            assert_eq!(phase.await.unwrap(), 7);
        }).await.unwrap();
    }
}
