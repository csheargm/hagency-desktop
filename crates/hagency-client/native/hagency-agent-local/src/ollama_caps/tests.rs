use super::*;
const NOON: i64 = 20_000 * SECONDS_PER_DAY + 12 * 3600;
fn caps(config: CapsConfig) -> (tempfile::TempDir, Caps) {
    let dir = tempfile::tempdir().unwrap();
    let caps = Caps::open(config, dir.path().join("caps.json")).unwrap();
    (dir, caps)
}
fn wide() -> CapsConfig {
    CapsConfig { server_concurrency: 8, reserve_tokens_per_run: 0, ..Default::default() }
}
#[test]
fn crash_mid_run_keeps_the_reservation_charged() {
    let config = CapsConfig { reserve_tokens_per_run: 100, user_tokens_per_day: 150, ..wide() };
    let (dir, mut c) = caps(config.clone());
    c.request("@a:x", NOON).unwrap();
    assert_eq!(c.user_tokens_today("@a:x"), 100, "charged before the run");
    drop(c); // crash: finish never happens
    let mut c = Caps::open(config, dir.path().join("caps.json")).unwrap();
    assert_eq!(c.user_tokens_today("@a:x"), 100);
    c.request("@a:x", NOON + 1).unwrap();
    c.finish("@a:x", 30, NOON + 2).unwrap();
    assert_eq!(c.user_tokens_today("@a:x"), 130, "second run settled to actual usage");
    assert!(matches!(c.request("@a:x", NOON + 3), Ok(Admission::Run)));
    c.finish("@a:x", 30, NOON + 4).unwrap();
    assert!(matches!(c.request("@a:x", NOON + 5), Err(Refusal::UserDailyLimit { .. })));
}
#[test]
fn requests_per_minute_are_limited_per_user() {
    let (_d, mut c) = caps(CapsConfig { requests_per_minute: 2, ..wide() });
    for t in [0, 1] {
        assert_eq!(c.request("@a:x", NOON + t), Ok(Admission::Run));
        c.finish("@a:x", 0, NOON + t).unwrap();
    }
    let refused = c.request("@a:x", NOON + 2).unwrap_err();
    assert_eq!(refused, Refusal::RateLimited { retry_at: NOON + 60 });
    assert_eq!(refused.message(), "Too many requests — try again at 12:01.");
    assert_eq!(c.request("@b:x", NOON + 2), Ok(Admission::Run));
    assert_eq!(c.request("@a:x", NOON + 61), Ok(Admission::Run));
}
#[test]
fn one_concurrent_run_per_user() {
    let (_d, mut c) = caps(wide());
    assert_eq!(c.request("@a:x", NOON), Ok(Admission::Run));
    assert_eq!(c.request("@a:x", NOON), Err(Refusal::AlreadyRunning));
}
#[test]
fn user_daily_tokens_persist_across_restart_and_reset_next_day() {
    let (dir, mut c) = caps(CapsConfig { user_tokens_per_day: 100, ..wide() });
    c.request("@a:x", NOON).unwrap();
    c.finish("@a:x", 150, NOON).unwrap();
    let mut c = Caps::open(CapsConfig { user_tokens_per_day: 100, ..wide() }, dir.path().join("caps.json")).unwrap();
    let refused = c.request("@a:x", NOON + 5).unwrap_err();
    assert_eq!(refused.message(), "Daily limit reached — resets at 00:00 UTC.");
    assert_eq!(c.request("@a:x", NOON + SECONDS_PER_DAY), Ok(Admission::Run));
}
#[test]
fn server_daily_budget_stops_everyone() {
    let (_d, mut c) = caps(CapsConfig { server_tokens_per_day: 10, ..wide() });
    c.request("@a:x", NOON).unwrap();
    c.finish("@a:x", 10, NOON).unwrap();
    assert!(matches!(c.request("@b:x", NOON), Err(Refusal::ServerDailyLimit { .. })));
}
#[test]
fn queued_runs_do_not_start_once_the_server_budget_is_spent() {
    let (_d, mut c) = caps(CapsConfig { server_tokens_per_day: 10, server_concurrency: 1, reserve_tokens_per_run: 0, ..Default::default() });
    c.request("@a:x", NOON).unwrap();
    c.request("@b:x", NOON).unwrap();
    assert_eq!(c.finish("@a:x", 10, NOON).unwrap(), None);
    assert!(matches!(c.request("@b:x", NOON + 1), Err(Refusal::ServerDailyLimit { .. })));
}
#[test]
fn zero_caps_are_rejected_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let zero = CapsConfig { requests_per_minute: 0, ..Default::default() };
    assert!(Caps::open(zero, dir.path().join("c.json")).is_err());
}
#[test]
fn server_queue_is_fair_across_users() {
    let (_d, mut c) = caps(CapsConfig { server_concurrency: 1, reserve_tokens_per_run: 0, ..Default::default() });
    assert_eq!(c.request("@a:x", NOON), Ok(Admission::Run));
    assert_eq!(c.request("@b:x", NOON), Ok(Admission::Queued { position: 1 }));
    assert_eq!(c.request("@c:x", NOON), Ok(Admission::Queued { position: 2 }));
    // @a cannot jump the queue with a second request.
    assert_eq!(c.request("@a:x", NOON), Err(Refusal::AlreadyRunning));
    assert_eq!(c.finish("@a:x", 1, NOON).unwrap().as_deref(), Some("@b:x"));
    assert_eq!(c.request("@a:x", NOON + 1), Ok(Admission::Queued { position: 2 }));
    assert_eq!(c.finish("@b:x", 1, NOON).unwrap().as_deref(), Some("@c:x"));
    assert_eq!(c.finish("@c:x", 1, NOON).unwrap().as_deref(), Some("@a:x"));
}
#[test]
fn kill_switch_refuses_and_drops_the_queue() {
    let (_d, mut c) = caps(CapsConfig { server_concurrency: 1, reserve_tokens_per_run: 0, ..Default::default() });
    c.request("@a:x", NOON).unwrap();
    c.request("@b:x", NOON).unwrap();
    c.set_stopped(true);
    assert_eq!(c.request("@c:x", NOON), Err(Refusal::Stopped));
    assert_eq!(c.finish("@a:x", 0, NOON).unwrap(), None);
    c.set_stopped(false);
    assert_eq!(c.request("@b:x", NOON + 1), Ok(Admission::Run));
}
#[test]
fn admin_exempt_user_skips_user_caps_but_not_server_budget() {
    let config = CapsConfig { requests_per_minute: 1, user_tokens_per_day: 1, server_tokens_per_day: 100,
        exempt_users: ["@admin:x".to_string()].into(), ..wide() };
    let (_d, mut c) = caps(config);
    assert_eq!(c.request("@admin:x", NOON), Ok(Admission::Run));
    assert_eq!(c.request("@admin:x", NOON), Err(Refusal::AlreadyRunning), "admins hold one slot too");
    c.finish("@admin:x", 0, NOON).unwrap();
    for _ in 0..2 {
        assert_eq!(c.request("@admin:x", NOON), Ok(Admission::Run));
        c.finish("@admin:x", 50, NOON).unwrap();
    }
    assert!(matches!(c.request("@admin:x", NOON), Err(Refusal::ServerDailyLimit { .. })));
}

#[test]
fn failed_reservation_save_leaves_no_slot_or_charge() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Caps::open(CapsConfig { reserve_tokens_per_run: 10, ..wide() }, dir.path().join("missing/caps.json")).unwrap();
    assert_eq!(c.request("@a:x", NOON), Err(Refusal::Storage));
    assert_eq!(c.user_tokens_today("@a:x"), 0);
    std::fs::create_dir(dir.path().join("missing")).unwrap();
    assert_eq!(c.request("@a:x", NOON + 1), Ok(Admission::Run), "not stuck as AlreadyRunning");
}
