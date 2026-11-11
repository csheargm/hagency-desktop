//! Deterministic review fixtures for tests and the headless harness.
//!
//! Compiled only for tests or with the `fixtures` feature. Every value here is
//! synthetic and labeled `example.test`; no production code path reads it and
//! no fixture is presented as live data.
use crate::format::Clock;
use crate::model::{
    CommandKind, CommandResult, ConnectionStatus, DashboardState, HostOptions, SetupNeed, SetupProgress, SetupView,
};
use hagency_metering::TokenCounts;
use hagency_metering::dashboard::{
    Activity, DASHBOARD_API_VERSION, DashboardSnapshot, HostingState, HostingStatus, OwnerIdentity, ProviderAccount,
    QuotaWindow, ResourceUsage, UNKNOWN_COUNTS, UsageSummary, period::UsagePeriod,
};

/// 2026-11-01 07:30:00 UTC.
pub const FIXTURE_NOW_MS: u64 = 1_793_518_200_000;
pub const FIXTURE_OFFSET_MINUTES: i32 = 8 * 60;
const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;

pub fn clock() -> Clock {
    Clock::fixed(FIXTURE_NOW_MS, FIXTURE_OFFSET_MINUTES)
}

pub const OWNER: &str = "@owner:example.test";

pub const SCENARIOS: &[&str] = &[
    "connecting", "daemon-absent", "incompatible", "auth-expired", "failed", "unconfigured", "starting",
    "running", "paused", "draining", "stopped", "offline", "hosting-failed", "empty", "large", "long-names",
    "shared-account", "unknown", "stale", "rejected", "uncertain", "awaiting", "stop-confirm", "disconnected",
    "setup", "setup-replace", "setup-other-owner", "setup-signing-in", "setup-registering", "setup-confirming", "setup-failed",
    "hosted", "hosted-off", "hosted-stopped", "uninstall-confirm", "foreign-service",
];

fn setup(need: SetupNeed, progress: SetupProgress) -> Option<SetupView> {
    Some(SetupView { need, progress })
}

fn options(keep_running: bool) -> Option<HostOptions> {
    Some(HostOptions { keep_running, busy: false, foreign_service: false })
}

fn counts(input: u64, output: u64, read: u64, write: u64) -> TokenCounts {
    TokenCounts { input: Some(input), output: Some(output), cache_read: Some(read), cache_write: Some(write) }
}

fn window(id: &str, label: &str, used: Option<f64>, reset_in: Option<u64>, minutes: Option<u64>) -> QuotaWindow {
    QuotaWindow {
        id: id.into(),
        label: label.into(),
        used_percent: used,
        resets_at_ms: reset_in.map(|d| FIXTURE_NOW_MS + d),
        window_minutes: minutes,
    }
}

fn resource(id: &str, name: &str, account: Option<&str>, provider: &str, model: &str, agents: u64, budget: Option<u64>, state: &str, counts: TokenCounts) -> ResourceUsage {
    ResourceUsage {
        id: id.into(),
        name: name.into(),
        account_id: account.map(Into::into),
        provider: provider.into(),
        model: model.into(),
        agent_count: Some(agents),
        budget_tokens: budget,
        state: state.into(),
        counts,
    }
}

fn activity(id: &str, ago: u64, title: &str, detail: &str, state: &str) -> Activity {
    Activity { id: id.into(), at_ms: FIXTURE_NOW_MS - ago, title: title.into(), detail: detail.into(), state: state.into() }
}

/// An ordinary owner with a shared Codex account (Terra and Astra), a
/// Claude account and partial measured usage.
pub fn ordinary(state: HostingState) -> DashboardSnapshot {
    DashboardSnapshot {
        version: DASHBOARD_API_VERSION,
        revision: 42,
        observed_at_ms: FIXTURE_NOW_MS - 20_000,
        owner: Some(OwnerIdentity { user_id: OWNER.into(), display_name: "Edward".into(), server: "example.test".into() }),
        hosting: HostingStatus {
            state,
            detail: None,
            active_runs: Some(if matches!(state, HostingState::Running | HostingState::Draining) { 2 } else { 0 }),
            pending_approvals: Some(1),
        },
        accounts: vec![
            ProviderAccount {
                    usage: None,
                id: "acct_codex".into(),
                label: "Codex · team@example.test".into(),
                provider: "Codex".into(),
                plan: Some("Pro".into()),
                state: "fresh".into(),
                observed_at_ms: Some(FIXTURE_NOW_MS - 3 * MIN),
                quota_windows: vec![
                    window("primary", "Primary", Some(62.0), Some(2 * HOUR + 14 * MIN), Some(300)),
                    window("secondary", "Weekly", Some(91.5), Some(3 * 24 * HOUR), Some(10_080)),
                ],
            },
            ProviderAccount {
                    usage: None,
                id: "acct_claude".into(),
                label: "Claude · owner@example.test".into(),
                provider: "Claude".into(),
                plan: Some("Max".into()),
                state: "partial".into(),
                observed_at_ms: Some(FIXTURE_NOW_MS - 12 * MIN),
                quota_windows: vec![
                    window("five_hour", "Session", Some(18.0), Some(4 * HOUR), Some(300)),
                    window("seven_day", "Weekly", None, None, Some(10_080)),
                ],
            },
        ],
        resources: vec![
            resource("res_terra", "Terra", Some("acct_codex"), "Codex", "gpt-6.1-terra", 3, Some(2_000_000), "measured", counts(412_000, 96_500, 1_250_000, 0)),
            resource("res_astra", "Astra", Some("acct_codex"), "Codex", "gpt-6.1-astra", 1, None, "partial", TokenCounts { input: Some(88_000), output: Some(12_000), cache_read: None, cache_write: Some(0) }),
            resource("res_sonnet", "Reviewer", Some("acct_claude"), "Claude", "claude-sonnet", 2, Some(500_000), "idle", counts(0, 0, 0, 0)),
        ],
        inventory_available: true,
        inventory_complete: true,
        usage: UsageSummary {
            counts: TokenCounts { input: Some(500_000), output: Some(108_500), cache_read: None, cache_write: Some(0) },
            complete: false,
            observed_at_ms: Some(FIXTURE_NOW_MS - 40_000),
            estimated_cost_usd: None,
        },
        usage_breakdown: None,
        activities: vec![
            activity("act_1", 2 * MIN, "Terra answered in Project Atlas", "Request from @maria:example.test", "succeeded"),
            activity("act_2", 6 * MIN, "Astra is running", "Room: Design review", "running"),
            activity("act_3", 40 * MIN, "Approval requested", "Reviewer asked to run a high-risk tool", "pending"),
            activity("act_4", 3 * HOUR, "Settlement outcome unknown", "Receipt is being reconciled", "uncertain"),
        ],
    }
}

fn large() -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    s.hosting.active_runs = Some(48);
    s.hosting.pending_approvals = Some(1_204);
    s.resources = (0..60)
        .map(|i| {
            resource(
                &format!("res_{i:03}"),
                &format!("Pool Agent {i:03}"),
                Some(if i % 2 == 0 { "acct_codex" } else { "acct_claude" }),
                if i % 2 == 0 { "Codex" } else { "Claude" },
                if i % 2 == 0 { "gpt-6.1-terra" } else { "claude-sonnet" },
                i % 7 + 1,
                (i % 3 != 0).then_some(10_000_000_000),
                if i % 5 == 0 { "partial" } else { "measured" },
                counts(9_876_543_210 + i, 1_234_567_890, 18_000_000_000_000, 7_654_321),
            )
        })
        .collect();
    s.usage = UsageSummary {
        counts: counts(987_654_321_000, 123_456_789_000, 18_446_744_073_709, 9_999_999_999),
        complete: true,
        observed_at_ms: Some(FIXTURE_NOW_MS - 30_000),
        estimated_cost_usd: Some(1_234_567.891),
    };
    s.activities = (0..80)
        .map(|i| activity(&format!("act_{i:03}"), i * 7 * MIN, &format!("Request {i} completed"), "Project Atlas", if i % 9 == 0 { "failed" } else { "succeeded" }))
        .collect();
    s
}

fn long_names() -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    let long = "A very long resource name that keeps going so the layout has to wrap it gracefully 一个非常非常长的中文资源名称用于测试换行";
    s.resources[0].name = long.into();
    s.accounts[0].label = format!("Codex · {}@example.test", "really-long-mailbox-name".repeat(4));
    s.accounts[0].quota_windows[0].label = "Primary rolling usage window reported by provider".into();
    s.activities[0].title = long.into();
    s.activities[0].detail = "Detail ".repeat(60);
    s.hosting.detail = Some("Worker pool warming up; ".repeat(20));
    s
}

fn unknown() -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    for account in &mut s.accounts {
        account.state = "unsupported".into();
        account.plan = None;
        account.observed_at_ms = None;
        for w in &mut account.quota_windows {
            w.used_percent = None;
            w.resets_at_ms = None;
            w.window_minutes = None;
        }
    }
    s.accounts.push(ProviderAccount {
                    usage: None,
        id: "acct_unverified".into(),
        label: "Codex · unverified".into(),
        provider: "Codex".into(),
        plan: None,
        state: "unverified".into(),
        observed_at_ms: None,
        quota_windows: vec![],
    });
    for r in &mut s.resources {
        r.counts = UNKNOWN_COUNTS;
        r.state = "unmeasured".into();
    }
    s.resources.push(resource("res_octos", "Octos helper", None, "Octos", "octos-local", 1, None, "something_new", UNKNOWN_COUNTS));
    s.usage = UsageSummary::default();
    s.hosting.active_runs = None;
    s.hosting.pending_approvals = None;
    s.activities[0].state = "future_state".into();
    s
}

fn stale() -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    s.observed_at_ms = FIXTURE_NOW_MS - 2 * HOUR;
    s.accounts[0].state = "stale".into();
    s.accounts[0].observed_at_ms = Some(FIXTURE_NOW_MS - 26 * HOUR);
    s.accounts[0].quota_windows[0].resets_at_ms = Some(FIXTURE_NOW_MS - 10 * MIN);
    s
}

fn shared_account() -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    s.accounts.truncate(1);
    s.accounts[0].state = "limited".into();
    s.accounts[0].quota_windows[0].used_percent = Some(100.0);
    s.resources.retain(|r| r.account_id.as_deref() == Some("acct_codex"));
    s
}

/// Build a state for a named scenario. Unknown names return `None`.
pub fn scenario(name: &str) -> Option<DashboardState> {
    let mut state = DashboardState::new(crate::i18n::Locale::En);
    state.request_ids = Some((1..=8).map(|i| format!("fixture-{i}")).collect());
    state.set_owner_scope(Some(OWNER));
    let apply = |state: &mut DashboardState, s: DashboardSnapshot| {
        state.apply_snapshot(s, Some(UsagePeriod::Day));
    };
    match name {
        "connecting" => {}
        "daemon-absent" => state.set_connection(ConnectionStatus::DaemonUnavailable),
        "incompatible" => state.set_connection(ConnectionStatus::Incompatible { daemon_version: Some(2) }),
        "auth-expired" => state.set_connection(ConnectionStatus::AuthExpired),
        "failed" => state.connection_failed("Connection refused (os error 61)"),
        "unconfigured" => {
            let mut s = DashboardSnapshot::unconfigured(FIXTURE_NOW_MS - 5_000);
            s.revision = 1;
            apply(&mut state, s);
        }
        "starting" => apply(&mut state, ordinary(HostingState::Starting)),
        "running" => apply(&mut state, ordinary(HostingState::Running)),
        "hosting-history" => {
            let mut s = ordinary(HostingState::Running);
            s.activities = vec![
                activity("daemon-123-2", MIN, "Provider reports refreshed", "3 resource reports updated. 1 unavailable; previous reports keep their observation times.", "warning"),
                activity("daemon-123-1", 2 * MIN, "Hosting started", "Pool workers started.", "succeeded"),
            ];
            apply(&mut state, s);
        }
        "paused" => apply(&mut state, ordinary(HostingState::Paused)),
        "draining" => apply(&mut state, ordinary(HostingState::Draining)),
        "stopped" => apply(&mut state, ordinary(HostingState::Stopped)),
        "offline" => apply(&mut state, ordinary(HostingState::Offline)),
        "hosting-failed" => {
            let mut s = ordinary(HostingState::Failed);
            s.hosting.detail = Some("Runtime authorization was lost (authorization_or_lease_lost).".into());
            apply(&mut state, s);
        }
        "empty" => {
            let mut s = ordinary(HostingState::Running);
            s.accounts.clear();
            s.resources.clear();
            s.activities.clear();
            s.usage = UsageSummary::default();
            s.hosting.pending_approvals = Some(0);
            s.hosting.active_runs = Some(0);
            apply(&mut state, s);
        }
        "large" => apply(&mut state, large()),
        "long-names" => apply(&mut state, long_names()),
        "shared-account" => apply(&mut state, shared_account()),
        "unknown" => apply(&mut state, unknown()),
        "stale" => apply(&mut state, stale()),
        "rejected" | "uncertain" | "awaiting" => {
            apply(&mut state, ordinary(HostingState::Running));
            let action = state.request(CommandKind::Pause, FIXTURE_NOW_MS);
            debug_assert!(action.is_some());
            let id = state.command.as_ref()?.ticket.request_id.clone();
            let result = match name {
                "rejected" => CommandResult::Rejected { code: "revision_conflict".into(), message: String::new(), retryable: true },
                "uncertain" => CommandResult::Uncertain { message: "timeout".into() },
                _ => CommandResult::Accepted,
            };
            state.command_result(&id, result, FIXTURE_NOW_MS);
            if name != "uncertain" {
                state.refreshing = false;
            }
        }
        "stop-confirm" => {
            apply(&mut state, ordinary(HostingState::Running));
            state.request(CommandKind::Stop, FIXTURE_NOW_MS);
        }
        "disconnected" => {
            apply(&mut state, ordinary(HostingState::Running));
            state.connection_failed("The hosting service closed the connection.");
        }
        "setup" => { state.set_setup(setup(SetupNeed::NotSetUp, SetupProgress::Idle)); }
        "setup-replace" => { state.set_setup(setup(SetupNeed::Replace, SetupProgress::Idle)); }
        "setup-other-owner" => { state.set_setup(setup(SetupNeed::OtherOwner, SetupProgress::Idle)); }
        "setup-signing-in" => { state.set_setup(setup(SetupNeed::NotSetUp, SetupProgress::SigningIn)); }
        "setup-registering" => { state.set_setup(setup(SetupNeed::NotSetUp, SetupProgress::Registering)); }
        "setup-confirming" => { state.set_setup(setup(SetupNeed::NotSetUp, SetupProgress::Confirming)); }
        "setup-failed" => {
            state.set_setup(setup(SetupNeed::NotSetUp, SetupProgress::Failed { code: "daemon_sign_in_cancelled".into() }));
        }
        "hosted" | "hosted-off" | "uninstall-confirm" | "foreign-service" => {
            apply(&mut state, ordinary(HostingState::Running));
            state.set_options(options(name != "hosted-off"));
            if name == "uninstall-confirm" { state.request_uninstall(); }
            if name == "foreign-service" { state.options.as_mut()?.foreign_service = true; }
        }
        "hosted-stopped" => {
            apply(&mut state, ordinary(HostingState::Stopped));
            state.set_options(options(true));
        }
        _ => return None,
    }
    Some(state)
}

/// Ordinary snapshot advanced by `step` revisions with changing counters,
/// used by the stability check (ten refreshes must not move the layout).
pub fn refresh_step(step: u64) -> DashboardSnapshot {
    let mut s = ordinary(HostingState::Running);
    s.revision += step;
    s.observed_at_ms = FIXTURE_NOW_MS - 20_000 + step * 1_000;
    if let Some(input) = s.usage.counts.input.as_mut() {
        *input += step * 1_337;
    }
    if let Some(output) = s.resources[0].counts.output.as_mut() {
        *output += step * 211;
    }
    s.accounts[0].quota_windows[0].used_percent = Some(62.0 + step as f64 * 0.5);
    s
}
