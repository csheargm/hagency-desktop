//! The Status tab's load never stays on "Loading…": a stalled owner call
//! becomes the timeout message within its bound, and saved watches are read
//! a few at a time, in roster order.
use super::super::load::{self, bounded, saved_all, too_long};
use super::super::{Watches, panel};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SHORT: Duration = Duration::from_millis(100);

#[tokio::test]
async fn a_stalled_saved_call_yields_the_timeout_message_within_the_bound() {
    let started = Instant::now();
    let ids = vec!["agt_ok".to_owned(), "agt_stuck".to_owned()];
    let result = saved_all(ids, load::CONCURRENT_READS, SHORT, |id| async move {
        if id == "agt_stuck" { std::future::pending::<()>().await; }
        Ok(json!({"id": id}))
    }).await;
    assert_eq!(result, Err(too_long()));
    assert!(started.elapsed() < Duration::from_secs(2), "bounded, took {:?}", started.elapsed());
    let loaded: super::super::Loaded = result.map(|_| Watches { rows: Vec::new(), status: Err(String::new()) });
    assert!(load::is_too_long(&loaded), "the refresh retries this once");
    let notice = panel::notice(Some(&loaded), None).unwrap();
    assert!(notice.contains("Room status is unavailable: it took too long. Open Status again to retry."), "{notice}");
}

#[tokio::test]
async fn the_whole_load_is_bounded_too() {
    let stalled = bounded("a test call", SHORT, std::future::pending::<Result<(), String>>()).await;
    assert_eq!(stalled, Err(too_long()));
    assert_eq!(bounded("a test call", SHORT, async { Ok::<_, String>(7) }).await, Ok(7));
    assert!(load::TOTAL_BOUND <= Duration::from_secs(20), "the tab answers within 20 s");
}

#[tokio::test]
async fn saved_watches_are_read_concurrently_within_the_limit_in_roster_order() {
    let (busy, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let ids: Vec<String> = (0..10).map(|i| format!("agt_{i}")).collect();
    let result = saved_all(ids.clone(), load::CONCURRENT_READS, Duration::from_secs(5), |id| {
        let (busy, peak) = (busy.clone(), peak.clone());
        async move {
            let now = busy.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            busy.fetch_sub(1, Ordering::SeqCst);
            Ok::<Value, String>(json!(id))
        }
    }).await.unwrap();
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= load::CONCURRENT_READS && peak > 1, "peak {peak}");
    assert_eq!(result.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(), ids);
    assert!(result.iter().all(|(id, reply)| reply == &json!(id)));
}

#[tokio::test]
async fn a_failed_saved_read_fails_the_load_with_its_reason() {
    let result = saved_all(vec!["agt_x".into()], 4, SHORT, |_| async { Err::<Value, _>("owner down".to_owned()) }).await;
    assert_eq!(result, Err("owner down".to_owned()));
}
