use super::*;
use serde_json::json;
use std::sync::{Mutex as StdMutex, atomic::AtomicUsize};

#[test]
fn a_running_host_is_healthy_and_a_stopped_one_is_restarted() {
    let value = json!({"recoveries":[
        {"agentId":"a","restored":false,"error":"agent_runtime_already_active"},
        {"agentId":"b","restored":true},
        {"agentId":"c","restored":false,"error":"server_unavailable"},
        {"agentId":"d","restored":false,"error":"raw provider text must not pass"}]});
    assert_eq!(read_pass(&value), Pass {
        restarted: vec!["b".into()],
        failed: vec![("c".into(),"server_unavailable".into()),("d".into(),"runtime_recovery_unrecognized_error".into())],
    });
    assert_eq!(read_pass(&json!({})), Pass::default());
}

#[test]
fn failed_restarts_back_off_to_a_ceiling() {
    assert_eq!(next_delay(0), CHECK_EVERY);
    assert_eq!(next_delay(1), CHECK_EVERY * 2);
    assert!(next_delay(2) > next_delay(1));
    assert_eq!(next_delay(50), MAX_BACKOFF);
}

fn quick(_: u32) -> Duration { Duration::from_millis(1) }

/// Regression for the 2026-11-17 incident: the host stopped while the window
/// kept running. The watchdog must start it again on its own, keep checking
/// afterwards, and stop once the account's service is retired.
#[tokio::test]
async fn a_host_that_stops_is_restarted_until_the_window_retires() {
    let control = Arc::new(Control::default());
    let passes = Arc::new(AtomicUsize::new(0));
    let restarted = Arc::new(StdMutex::new(Vec::new()));
    let (c, p, r) = (control.clone(), passes.clone(), restarted.clone());
    let task = tokio::spawn(watch(control.clone(), || true, quick, move || {
        let (c, p, r) = (c.clone(), p.clone(), r.clone());
        async move {
            let n = p.fetch_add(1, Ordering::SeqCst);
            // Pass 0: the host is down and restarts. Pass 1: transport error.
            // Pass 2: running. Then the window retires.
            Ok(match n {
                0 => { r.lock().unwrap().push("a"); json!({"recoveries":[{"agentId":"a","restored":true}]}) }
                1 => return Err("server_unavailable".to_owned()),
                2 => json!({"recoveries":[{"agentId":"a","restored":false,"error":ALREADY_RUNNING}]}),
                _ => { c.retired.store(true, Ordering::Release); json!({"recoveries":[]}) }
            })
        }
    }));
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("watchdog stops after retirement").unwrap();
    assert_eq!(*restarted.lock().unwrap(), vec!["a"]);
    assert_eq!(passes.load(Ordering::SeqCst), 4, "a failed check never stops the watchdog");
}

#[tokio::test]
async fn retirement_wakes_and_stops_the_watchdog_without_a_pass() {
    let control = Arc::new(Control::default());
    let passes = Arc::new(AtomicUsize::new(0));
    let p = passes.clone();
    let slow = |_: u32| Duration::from_secs(3600);
    let task = tokio::spawn(watch(control.clone(), || true, slow, move || {
        p.fetch_add(1, Ordering::SeqCst);
        async { Ok(json!({})) }
    }));
    let worker = Worker { control, tasks: Default::default() };
    worker.retire();
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("retire wakes the sleeping watchdog").unwrap();
    assert_eq!(passes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_signed_out_window_runs_no_recovery() {
    let passes = Arc::new(AtomicUsize::new(0));
    let p = passes.clone();
    tokio::time::timeout(Duration::from_secs(5), watch(Arc::new(Control::default()), || false, quick, move || {
        p.fetch_add(1, Ordering::SeqCst);
        async { Ok(json!({})) }
    })).await.expect("watchdog ends when the account is gone");
    assert_eq!(passes.load(Ordering::SeqCst), 0);
}
