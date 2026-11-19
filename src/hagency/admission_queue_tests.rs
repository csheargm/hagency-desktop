use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Every test here shares the one process-wide slot pool.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A burst of admissions (sign-in with many rooms): never more than
/// `MAX_ADMISSIONS` run at once, the limit is used, and every one completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn at_most_max_admissions_run_at_once_and_all_complete() {
    let _serial = SERIAL.lock().await;
    const BURST: usize = 20;
    let (running, peak, done) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let tasks: Vec<_> = (0..BURST).map(|_| {
        let (running, peak, done) = (running.clone(), peak.clone(), done.clone());
        tokio::spawn(limited(async move {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            running.fetch_sub(1, Ordering::SeqCst);
            done.fetch_add(1, Ordering::SeqCst);
        }))
    }).collect();
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(done.load(Ordering::SeqCst), BURST, "every queued admission completes");
    assert_eq!(peak.load(Ordering::SeqCst), MAX_ADMISSIONS, "the limit holds and is used");
}

/// A failed fetch is not asked again at once: each retry waits its pause,
/// and no slot is held while waiting, so other admissions go ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_try_backs_off_without_holding_a_slot_and_a_later_try_succeeds() {
    let _serial = SERIAL.lock().await;
    let pauses = [Duration::ZERO, Duration::from_millis(60), Duration::from_millis(120)];
    let mut tries: Vec<Instant> = Vec::new();
    let start = Instant::now();
    let tries_ref = &mut tries;
    let value = with_backoff(&pauses, || {
        tries_ref.push(Instant::now());
        let n = tries_ref.len();
        async move {
            assert_eq!(ADMISSIONS.available_permits(), MAX_ADMISSIONS - 1, "a try holds one slot");
            (n == 3).then_some("fetched")
        }
    }).await;
    assert_eq!(value, Some("fetched"));
    assert_eq!(tries.len(), 3);
    assert!(tries[1] - tries[0] >= pauses[1] && tries[2] - tries[1] >= pauses[2], "{:?}", tries.iter().map(|t| *t - start).collect::<Vec<_>>());
    assert_eq!(ADMISSIONS.available_permits(), MAX_ADMISSIONS, "no slot is kept after the tries");
    let none = with_backoff(&[Duration::ZERO, Duration::from_millis(10)], || async { None::<()> }).await;
    assert_eq!(none, None, "out of tries: no value");
}

/// GPT review P2 (round 2): a slot covered a whole room's batch. Each card
/// takes its own slot, so rooms with many cards interleave; the peak stays 4.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn rooms_with_many_cards_interleave_and_the_peak_stays_at_the_limit() {
    let _serial = SERIAL.lock().await;
    const ROOMS: usize = 6;
    const CARDS: usize = 5;
    let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rooms: Vec<_> = (0..ROOMS).map(|room| {
        let (running, peak, order) = (running.clone(), peak.clone(), order.clone());
        tokio::spawn(async move {
            let cards: Vec<usize> = (0..CARDS).collect();
            each_limited(&cards, |_| {
                let (running, peak, order) = (running.clone(), peak.clone(), order.clone());
                async move {
                    peak.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    order.lock().unwrap().push(room);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    true
                }
            }).await
        })
    }).collect();
    for room in rooms {
        assert!(room.await.unwrap(), "every room's cards are admitted");
    }
    let order = order.lock().unwrap().clone();
    assert_eq!(order.len(), ROOMS * CARDS, "every card completes");
    assert_eq!(peak.load(Ordering::SeqCst), MAX_ADMISSIONS, "the limit holds and is used");
    let first_finish = (0..ROOMS).map(|r| order.iter().rposition(|&o| o == r).unwrap()).min().unwrap();
    let last_start = (0..ROOMS).map(|r| order.iter().position(|&o| o == r).unwrap()).max().unwrap();
    assert!(last_start < first_finish, "every room starts before any room finishes: {order:?}");
}
