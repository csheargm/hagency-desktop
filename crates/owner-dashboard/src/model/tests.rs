use super::*;
use crate::fixtures::{self, FIXTURE_NOW_MS, OWNER, clock, ordinary};
use hagency_metering::dashboard::{HostingState, OwnerIdentity, UNKNOWN_COUNTS};

fn running() -> DashboardState {
    fixtures::scenario("running").unwrap()
}

#[test]
fn restored_views_are_owner_scoped_and_approvals_stay_reachable() {
    let mut state = running();
    let mut view = state.view.clone();
    view.owner_id = Some("@someone:example.test".into());
    assert!(!state.restore_view(view.clone()));
    view.owner_id = state.owner_scope.clone();
    assert!(state.restore_view(view));
    state.snapshot.as_mut().unwrap().hosting.pending_approvals = Some(2);
    assert!(state.present(clock()).attention.iter().any(|item| item.target == AttentionTarget::Approvals));
}

#[test]
fn setup_replaces_the_dashboard_and_only_offers_what_applies() {
    let mut state = running();
    state.set_connection(ConnectionStatus::DaemonUnavailable);
    for (need, start, glyph) in [
        (SetupNeed::NotSetUp, Some("Set up hosting"), Glyph::Setup),
        (SetupNeed::Replace, Some("Set up hosting"), Glyph::Setup),
        (SetupNeed::OtherOwner, None, Glyph::Warning),
    ] {
        assert!(state.set_setup(Some(SetupView { need, progress: SetupProgress::Idle })));
        let p = state.present(clock());
        let page = p.setup_page.expect("setup page");
        assert_eq!(page.start, start, "{need:?}");
        assert_eq!(page.status.glyph, glyph);
        assert!(!page.busy && !page.can_cancel && page.progress.is_none());
        // The page explains itself: no banner, connecting page or options.
        assert!(p.banner.is_none() && p.empty_page.is_none() && p.options.is_none());
    }
    let other = state.present(clock()).setup_page.unwrap();
    assert!(other.body.contains("one account's resource pool"), "{}", other.body);
    state.set_setup(Some(SetupView { need: SetupNeed::NotSetUp, progress: SetupProgress::SigningIn }));
    let page = state.present(clock()).setup_page.unwrap();
    assert!(page.busy && page.can_cancel && page.start.is_none());
    assert_eq!(page.progress.unwrap().text, "Complete sign-in in your browser…");
    state.set_setup(Some(SetupView { need: SetupNeed::NotSetUp, progress: SetupProgress::Registering }));
    let page = state.present(clock()).setup_page.unwrap();
    assert!(page.busy && !page.can_cancel && page.start.is_none());
    assert_eq!(page.progress.unwrap().text, "Signed in. Registering this Mac’s executor…");
    for progress in [SetupProgress::Preparing, SetupProgress::Registering, SetupProgress::Cancelling, SetupProgress::Enabling, SetupProgress::Confirming] {
        state.set_setup(Some(SetupView { need: SetupNeed::NotSetUp, progress }));
        let page = state.present(clock()).setup_page.unwrap();
        assert!(page.busy && !page.can_cancel && page.start.is_none());
    }
    state.set_setup(Some(SetupView { need: SetupNeed::NotSetUp, progress: SetupProgress::Failed { code: "token=secret".into() } }));
    let page = state.present(clock()).setup_page.unwrap();
    assert_eq!(page.start, Some("Try again"));
    let failure = page.progress.unwrap();
    assert_eq!(failure.tone, Tone::Critical);
    assert!(!failure.text.contains("secret"));
    assert!(state.set_setup(None));
    assert!(state.present(clock()).setup_page.is_none());
    assert!(state.present(clock()).banner.is_some(), "the service banner returns with the dashboard");
}

#[test]
fn a_hosted_owner_gets_the_switch_and_a_confirmed_uninstall() {
    let mut state = running();
    assert!(state.present(clock()).options.is_none(), "hosts without options show none");
    assert!(!state.request_uninstall());
    state.set_options(Some(HostOptions { keep_running: true, busy: false, foreign_service: false }));
    let options = state.present(clock()).options.unwrap();
    assert!(options.keep_running && !options.uninstall_confirm && options.foreign_service.is_none());
    assert_eq!(options.keep_running_title, "Keep the resource pool running after Desktop quits");
    state.locale = Locale::ZhHans;
    assert_eq!(state.present(clock()).options.unwrap().keep_running_title, "关闭桌面端后继续运行资源池");
    state.locale = Locale::En;
    // Uninstall asks first; the confirmation is the only path to the action.
    assert!(state.request_uninstall());
    assert!(state.present(clock()).options.unwrap().uninstall_confirm);
    state.cancel_uninstall();
    assert_eq!(state.confirm_uninstall(), None);
    assert!(state.request_uninstall());
    assert_eq!(state.confirm_uninstall(), Some(DashboardAction::UninstallService));
    assert!(!state.confirm_uninstall);
    // Busy service work closes the confirmation and refuses another.
    assert!(state.request_uninstall());
    state.set_options(Some(HostOptions { keep_running: false, busy: true, foreign_service: true }));
    assert!(!state.confirm_uninstall && !state.request_uninstall());
    let options = state.present(clock()).options.unwrap();
    assert!(!options.keep_running && options.busy && options.foreign_service.is_some());
    // Setup or a missing snapshot hides the options.
    state.set_setup(Some(SetupView { need: SetupNeed::NotSetUp, progress: SetupProgress::Idle }));
    assert!(state.present(clock()).options.is_none());
    state.set_setup(None);
    state.set_owner_scope(Some("@next:example.test"));
    assert!(state.options.is_none() && state.setup.is_none());
}

#[test]
fn banner_actions_ask_the_host_for_the_matching_step() {
    let mut state = running();
    state.set_connection(ConnectionStatus::DaemonUnavailable);
    let banner = state.present(clock()).banner.unwrap();
    assert_eq!(banner.primary.action(), Some(DashboardAction::StartService));
    state.set_connection(ConnectionStatus::AuthExpired);
    assert_eq!(state.present(clock()).banner.unwrap().primary.action(), Some(DashboardAction::OpenSetup));
    state.set_connection(ConnectionStatus::OwnerMismatch);
    let banner = state.present(clock()).banner.unwrap();
    assert_eq!((banner.primary, banner.secondary), (BannerAction::Retry, BannerAction::None));
}

#[test]
fn every_fixture_scenario_presents_in_both_locales() {
    for name in fixtures::SCENARIOS {
        for locale in [Locale::En, Locale::ZhHans] {
            let mut state = fixtures::scenario(name).unwrap_or_else(|| panic!("missing scenario {name}"));
            state.locale = locale;
            let p = state.present(clock());
            // Something always explains the state: a page, a banner or hosting.
            assert!(
                p.empty_page.is_some() || p.setup_page.is_some() || p.banner.is_some() || p.hosting.is_some(),
                "{name}/{locale:?} shows nothing"
            );
        }
    }
    assert!(fixtures::scenario("nope").is_none());
}

#[test]
fn initial_state_is_connecting_not_ready() {
    let state = DashboardState::new(Locale::En);
    let p = state.present(clock());
    assert!(p.hosting.is_none());
    assert!(p.banner.is_none());
    let page = p.empty_page.expect("connecting page");
    assert!(page.busy);
    assert_eq!(page.status.glyph, Glyph::Clock);
    assert!(!state.can(CommandKind::Pause, Some(HostingState::Running)));
}

#[test]
fn default_snapshot_is_unconfigured_with_unknown_usage() {
    let mut state = DashboardState::new(Locale::En);
    assert_eq!(state.apply_snapshot(DashboardSnapshot::unconfigured(FIXTURE_NOW_MS), None), SnapshotOutcome::Applied);
    let p = state.present(clock());
    let hosting = p.hosting.unwrap();
    assert_eq!(hosting.title, "Hosting isn't set up");
    assert!(hosting.show_setup && !hosting.show_pause && !hosting.show_resume && !hosting.show_stop);
    assert_eq!(p.usage.total, "Unknown");
    assert!(!p.usage.total_known);
    assert_eq!(p.usage.cache_ratio, "Unavailable");
    assert_eq!(p.usage.completeness.text, "No measured usage reported.");
    assert_eq!(p.usage.cost, "Not estimated");
    assert_eq!(p.usage.title, "Measured usage · Period not reported");
    assert!(p.usage.lines.iter().all(|l| !l.known && l.value == "Unknown"));
}

#[test]
fn older_revisions_and_other_owners_never_replace_the_view() {
    let mut state = running();
    let mut old = ordinary(HostingState::Paused);
    old.revision = 41;
    assert_eq!(state.apply_snapshot(old, None), SnapshotOutcome::Stale);
    assert_eq!(state.snapshot.as_ref().unwrap().hosting.state, HostingState::Running);

    let mut other = ordinary(HostingState::Paused);
    other.revision = 99;
    other.owner = Some(OwnerIdentity { user_id: "@someone:example.test".into(), display_name: "X".into(), server: "example.test".into() });
    assert_eq!(state.apply_snapshot(other, None), SnapshotOutcome::WrongOwner);
    assert_eq!(state.snapshot.as_ref().unwrap().revision, 42);

    let mut same = ordinary(HostingState::Running);
    same.revision = 42;
    assert_eq!(state.apply_snapshot(same, None), SnapshotOutcome::Applied);
}

#[test]
fn incompatible_snapshot_is_not_applied() {
    let mut state = running();
    let mut future = ordinary(HostingState::Running);
    future.version = 2;
    future.revision = 100;
    assert_eq!(state.apply_snapshot(future, None), SnapshotOutcome::Incompatible);
    assert_eq!(state.snapshot.as_ref().unwrap().revision, 42);
    let p = state.present(clock());
    let banner = p.banner.unwrap();
    assert_eq!(banner.title, "Update required");
    assert!(banner.body.contains("version 2"));
    assert!(banner.body.contains("last known"));
    assert!(!p.hosting.unwrap().can_pause, "commands are disabled while incompatible");
}

#[test]
fn account_switch_clears_private_state_and_ignores_late_replies() {
    let mut state = running();
    state.select_resource(Some("res_terra".into()));
    state.request(CommandKind::Pause, FIXTURE_NOW_MS).unwrap();
    let id = state.command.as_ref().unwrap().ticket.request_id.clone();
    assert!(state.set_owner_scope(Some("@other:example.test")));
    assert!(state.snapshot.is_none());
    assert!(state.command.is_none());
    assert_eq!(state.view.selected_resource, None);
    assert_eq!(state.connection, ConnectionStatus::Connecting);
    assert_eq!(state.command_result(&id, CommandResult::Completed, FIXTURE_NOW_MS), None);
    // The previous owner's snapshot arriving late is rejected.
    assert_eq!(state.apply_snapshot(ordinary(HostingState::Running), None), SnapshotOutcome::WrongOwner);
    assert!(!state.set_owner_scope(Some("@other:example.test")));
}

#[test]
fn pause_waits_for_confirmation_from_a_snapshot() {
    let mut state = running();
    let action = state.request(CommandKind::Pause, FIXTURE_NOW_MS).unwrap();
    let DashboardAction::PauseHosting(ticket) = action else { panic!("{action:?}") };
    assert_eq!(ticket.request_id, "fixture-1");
    assert_eq!(ticket.expected_revision, Some(42));
    // Nothing else may be sent while it is in flight.
    assert!(state.request(CommandKind::Stop, FIXTURE_NOW_MS).is_none());
    assert!(!state.present(clock()).hosting.unwrap().can_pause);

    let follow = state.command_result("fixture-1", CommandResult::Accepted, FIXTURE_NOW_MS);
    assert!(matches!(follow, Some(DashboardAction::Refresh { .. })));
    let line = state.present(clock()).command.unwrap();
    assert!(line.status.text.contains("waiting for the hosting service to confirm"), "{}", line.status.text);

    // Draining reports remaining work, still not "paused".
    let mut draining = ordinary(HostingState::Draining);
    draining.revision = 43;
    state.apply_snapshot(draining, None);
    let p = state.present(clock());
    assert_eq!(p.hosting.as_ref().unwrap().title, "Pausing");
    assert!(p.command.unwrap().status.text.contains("2 running task"));

    let mut paused = ordinary(HostingState::Paused);
    paused.revision = 44;
    state.apply_snapshot(paused, None);
    let p = state.present(clock());
    assert_eq!(p.hosting.as_ref().unwrap().title, "Hosting is paused");
    let line = p.command.unwrap();
    assert_eq!(line.status.tone, Tone::Positive);
    assert!(line.can_dismiss);
    assert!(state.can(CommandKind::Resume, None));
}

#[test]
fn uncertain_outcome_reuses_the_original_request_id() {
    let mut state = fixtures::scenario("uncertain").unwrap();
    let p = state.present(clock());
    let line = p.command.unwrap();
    assert_eq!(line.status.tone, Tone::Caution);
    assert!(line.can_check && !line.can_retry && !line.can_dismiss);
    assert!(p.attention.iter().any(|a| a.glyph == Glyph::Question));
    // No new command (with a new ID) can be issued while uncertain.
    assert!(state.request(CommandKind::Pause, FIXTURE_NOW_MS).is_none());
    assert!(state.retry_rejected(FIXTURE_NOW_MS).is_none());
    let check = state.check_command().unwrap();
    let DashboardAction::CheckCommand { kind, ticket } = check else { panic!() };
    assert_eq!(kind, CommandKind::Pause);
    assert_eq!(ticket.request_id, "fixture-1");
    assert!(state.check_command().is_none(), "one lookup at a time");
    state.command_result("fixture-1", CommandResult::Uncertain { message: "again".into() }, FIXTURE_NOW_MS);
    assert_eq!(state.command.as_ref().unwrap().phase, CommandPhase::Uncertain);
    state.command_result("fixture-1", CommandResult::Completed, FIXTURE_NOW_MS);
    assert!(matches!(state.command.as_ref().unwrap().phase, CommandPhase::Confirmed { .. }));
}

#[test]
fn failed_status_check_cannot_create_a_new_ticket_even_after_a_snapshot_refresh() {
    let mut state = fixtures::scenario("uncertain").unwrap();
    let original = state.command.as_ref().unwrap().ticket.clone();
    for result in [
        CommandResult::NotSubmitted { code: "not_connected".into(), retryable: true },
        CommandResult::NotSubmitted { code: "busy".into(), retryable: true },
        CommandResult::Rejected { code: "busy".into(), message: String::new(), retryable: true },
        CommandResult::Rejected { code: "owner_scope_mismatch".into(), message: String::new(), retryable: false },
    ] {
        let DashboardAction::CheckCommand { ticket, .. } = state.check_command().unwrap() else { panic!() };
        assert_eq!(ticket, original);
        state.command_result(&ticket.request_id, result, FIXTURE_NOW_MS);
        assert_eq!(state.command.as_ref().unwrap().phase, CommandPhase::Uncertain);
        let mut snapshot = fixtures::ordinary(HostingState::Running);
        snapshot.revision += 1;
        state.apply_snapshot(snapshot, None);
        assert!(state.retry_rejected(FIXTURE_NOW_MS).is_none());
        assert_eq!(state.command.as_ref().unwrap().ticket, original);
    }
}

#[test]
fn rejection_names_action_reason_and_offers_retry_with_new_id() {
    let mut state = fixtures::scenario("rejected").unwrap();
    let line = state.present(clock()).command.unwrap();
    assert_eq!(line.status.tone, Tone::Critical);
    assert_eq!(line.status.glyph, Glyph::Cross);
    assert!(line.status.text.starts_with("Pause hosting failed: Hosting changed"), "{}", line.status.text);
    assert!(line.status.text.contains("(revision_conflict)"));
    assert!(line.can_retry && line.can_dismiss && !line.can_check);
    let retry = state.retry_rejected(FIXTURE_NOW_MS).unwrap();
    let DashboardAction::PauseHosting(ticket) = retry else { panic!() };
    assert_eq!(ticket.request_id, "fixture-2");

    let mut zh = fixtures::scenario("rejected").unwrap();
    zh.locale = Locale::ZhHans;
    let text = zh.present(clock()).command.unwrap().status.text;
    assert!(text.starts_with("暂停托管失败"), "{text}");
}

#[test]
fn non_retryable_rejection_has_no_retry() {
    let mut state = running();
    state.request(CommandKind::Pause, FIXTURE_NOW_MS);
    state.command_result("fixture-1", CommandResult::Rejected { code: "unheard_of".into(), message: "x".into(), retryable: false }, FIXTURE_NOW_MS);
    let line = state.present(clock()).command.unwrap();
    assert!(!line.can_retry);
    assert!(line.status.text.contains("The hosting service rejected the request. (unheard_of)"));
    state.dismiss_command();
    assert!(state.command.is_none());
    assert!(state.can(CommandKind::Pause, None));
}

#[test]
fn results_for_unknown_request_ids_are_ignored() {
    let mut state = running();
    state.request(CommandKind::Pause, FIXTURE_NOW_MS);
    assert!(state.command_result("someone-else", CommandResult::Completed, FIXTURE_NOW_MS).is_none());
    assert_eq!(state.command.as_ref().unwrap().phase, CommandPhase::Submitting);
}

#[test]
fn stop_requires_confirmation_and_cancel_keeps_hosting() {
    let mut state = running();
    assert!(state.request(CommandKind::Stop, FIXTURE_NOW_MS).is_none());
    assert!(state.confirm_stop);
    assert!(state.present(clock()).stop_confirm);
    state.cancel_stop();
    assert!(state.command.is_none() && !state.confirm_stop);
    state.request(CommandKind::Stop, FIXTURE_NOW_MS);
    let action = state.request(CommandKind::Stop, FIXTURE_NOW_MS).unwrap();
    assert!(matches!(action, DashboardAction::StopHosting(_)));
    // "Stopped" only after a snapshot confirms it.
    state.command_result("fixture-1", CommandResult::Accepted, FIXTURE_NOW_MS);
    assert_eq!(state.present(clock()).hosting.unwrap().title, "Hosting is on");
    let mut stopped = ordinary(HostingState::Stopped);
    stopped.revision = 50;
    state.apply_snapshot(stopped, None);
    let p = state.present(clock());
    assert_eq!(p.hosting.as_ref().unwrap().title, "Hosting is stopped");
    let hosting = p.hosting.unwrap();
    assert!(hosting.show_resume && hosting.can_resume && !hosting.show_setup);
    assert!(matches!(state.request(CommandKind::Resume, FIXTURE_NOW_MS + 1), Some(DashboardAction::ResumeHosting(_))));
}

#[test]
fn disconnect_keeps_last_known_data_and_disables_commands() {
    let state = fixtures::scenario("disconnected").unwrap();
    let p = state.present(clock());
    assert!(p.hosting.is_some(), "content stays visible");
    assert!(p.sync_stale);
    let banner = p.banner.unwrap();
    assert_eq!(banner.primary, BannerAction::Retry);
    assert!(banner.body.contains("closed the connection"));
    assert!(banner.body.contains("last known"));
    let hosting = p.hosting.unwrap();
    assert!(!hosting.can_pause && !hosting.can_stop);
    assert!(hosting.show_pause, "the control stays in place, disabled");
}

#[test]
fn daemon_absent_offers_to_start_the_service_and_retry() {
    let p = fixtures::scenario("daemon-absent").unwrap().present(clock());
    assert!(p.empty_page.is_none());
    let banner = p.banner.unwrap();
    assert_eq!((banner.primary, banner.secondary), (BannerAction::StartService, BannerAction::Retry));
    assert_eq!(banner.status.glyph, Glyph::Offline);
    let auth = fixtures::scenario("auth-expired").unwrap().present(clock()).banner.unwrap();
    assert_eq!(auth.primary, BannerAction::SignIn);
}

#[test]
fn refresh_keeps_content_and_shows_progress() {
    let mut state = running();
    let action = state.begin_refresh();
    assert_eq!(action, DashboardAction::Refresh { period: None });
    let p = state.present(clock());
    assert!(p.hosting.is_some());
    assert_eq!(p.sync_line, "Refreshing…");
    state.apply_snapshot(fixtures::refresh_step(1), None);
    assert!(!state.refreshing);
    assert!(state.present(clock()).sync_line.starts_with("Updated"));
}

#[test]
fn shared_provider_account_groups_resources_by_verified_id_only() {
    let state = running();
    let p = state.present(clock());
    let codex = p.accounts.iter().find(|a| a.id == "acct_codex").unwrap();
    let names: Vec<_> = codex.resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Terra", "Astra"]);
    assert_eq!(codex.subtitle, "Codex · Pro");
    assert_eq!(codex.quotas[0].label, "Primary · 5-hour window");
    assert_eq!(codex.quotas[0].value, "62% used");
    assert_eq!(codex.quotas[0].reset, "Resets in 2h 14m · Today 17:44 (UTC+8)");
    assert_eq!(codex.quotas[1].tone, Tone::Critical);
    assert!(p.attention.iter().any(|a| a.text.contains("91.5") || a.text.contains("92%")));
    assert!(p.unassigned.is_empty());
}

#[test]
fn unknown_unsupported_and_unverified_never_read_as_zero() {
    let p = fixtures::scenario("unknown").unwrap().present(clock());
    for account in &p.accounts {
        for q in &account.quotas {
            assert_eq!(q.value, "Not reported");
            assert_eq!(q.fraction, None);
            assert_eq!(q.reset, "Reset time not reported");
            assert_eq!(q.tone, Tone::Neutral);
        }
    }
    let unverified = p.accounts.iter().find(|a| a.id == "acct_unverified").unwrap();
    assert_eq!(unverified.status.text, "Identity not verified");
    assert!(unverified.note.is_some());
    assert_eq!(unverified.observed, "Not observed yet");
    assert_eq!(p.unassigned.len(), 1);
    assert_eq!(p.unassigned[0].status.text, "Unrecognized status");
    assert!(p.resources.iter().all(|r| r.tokens == "Unknown"));
    assert_eq!(p.usage.total, "Unknown");
    assert_eq!(p.activities.iter().find(|a| a.id == "act_1").unwrap().status.glyph, Glyph::Question);
    let hosting = p.hosting.unwrap();
    assert_eq!((hosting.active_runs.as_str(), hosting.pending_approvals.as_str()), ("Unknown", "Unknown"));
    assert_eq!(hosting.body, "Accepting new work for your rented Agents.");
}

#[test]
fn partial_totals_are_unknown_and_flagged() {
    let p = running().present(clock());
    // cache_read is unknown, so the total is unknown and ratio unavailable.
    assert_eq!(p.usage.total, "Unknown");
    assert_eq!(p.usage.cache_ratio, "Unavailable");
    assert_eq!(p.usage.completeness.tone, Tone::Caution);
    assert!(p.attention.iter().any(|a| a.text == "Some usage measurements are incomplete."));
    let terra = p.usage.by_resource.iter().find(|r| r.id == "res_terra").unwrap();
    assert_eq!(terra.total, "1,758,500");
    // 1,250,000 / (412,000 + 1,250,000 + 0)
    assert_eq!(terra.cache_ratio, "75%");
    assert_eq!(p.usage.title, "Measured usage · Today");
}

#[test]
fn account_filter_uses_ledger_totals_and_never_infers_zero_from_empty_resources() {
    let mut state = running();
    let snapshot = state.snapshot.as_mut().unwrap();
    snapshot.accounts.iter_mut().find(|a| a.id == "acct_claude").unwrap().usage = Some(
        crate::dto::UsageSummary { counts: TokenCounts { input: Some(400), output: Some(100), cache_read: Some(0), cache_write: Some(0) }, complete: true, observed_at_ms: Some(fixtures::FIXTURE_NOW_MS), estimated_cost_usd: None }
    );
    snapshot.resources.clear();
    state.set_account_filter(Some("acct_claude".into()));
    let p = state.present(clock());
    assert!(p.usage.by_resource.is_empty());
    assert_eq!(p.usage.total, "500", "ledger history survives resource removal");
    assert_eq!(p.usage.cost, "Not estimated");
    state.set_account_filter(Some("acct_codex".into()));
    assert_eq!(state.present(clock()).usage.total, "Unknown", "missing account measurements are not zero");
}

#[test]
fn large_counters_do_not_overflow() {
    let p = fixtures::scenario("large").unwrap().present(clock());
    assert_eq!(p.resources.len(), 60);
    assert_eq!(p.activities.len(), 80);
    assert!(p.usage.total_known);
    assert_eq!(p.usage.cost, "≈ $1,234,567.89");
    assert_eq!(p.hosting.unwrap().pending_approvals, "1,204");
    assert!(total_tokens(&TokenCounts { input: Some(u64::MAX), output: Some(1), cache_read: Some(0), cache_write: Some(0) }).is_none());
}

#[test]
fn stale_data_and_past_resets_are_explicit() {
    let p = fixtures::scenario("stale").unwrap().present(clock());
    assert!(p.sync_stale);
    assert_eq!(p.sync_line, "Updated 2 h ago");
    let codex = &p.accounts[0];
    assert_eq!(codex.status.text, "Out of date");
    assert_eq!(codex.observed, "Observed 1 day ago");
    assert!(codex.quotas[0].reset.starts_with("Reset due Today 15:20"), "{}", codex.quotas[0].reset);
}

#[test]
fn activities_are_newest_first_with_bounded_text() {
    let p = fixtures::scenario("long-names").unwrap().present(clock());
    let times: Vec<_> = p.activities.iter().map(|a| a.when.clone()).collect();
    assert_eq!(times[0], "2 min ago");
    assert!(p.activities[0].detail.chars().count() <= 240);
    assert!(p.hosting.unwrap().detail.unwrap().chars().count() <= 240);
}

#[test]
fn resource_detail_survives_removal_with_explanation() {
    let mut state = running();
    state.select_resource(Some("res_terra".into()));
    let d = state.present(clock()).detail.unwrap();
    assert!(d.present);
    assert!(d.facts.iter().any(|(k, v)| *k == "Resource ID" && v == "res_terra"));
    assert_eq!(d.cache_ratio, "75%");
    let mut next = ordinary(HostingState::Running);
    next.revision = 43;
    next.resources.retain(|r| r.id != "res_terra");
    state.apply_snapshot(next, None);
    let d = state.present(clock()).detail.unwrap();
    assert!(!d.present);
    assert_eq!(d.row.status.text, "This resource is no longer reported by the hosting service.");
}

#[test]
fn view_state_round_trips_and_is_owner_scoped() {
    let mut state = running();
    state.select_tab(Tab::Usage);
    state.set_account_filter(Some("acct_codex".into()));
    state.select_period(UsagePeriod::Week);
    state.view.scroll_y = 120.0;
    let json = serde_json::to_string(&state.view).unwrap();
    assert!(json.contains("\"tab\":\"usage\""));
    assert!(json.contains("\"accountFilter\":\"acct_codex\""));
    assert!(!json.to_lowercase().contains("token\""), "no credentials in view state");
    let restored: ViewState = serde_json::from_str(&json).unwrap();
    let mut other = running();
    assert!(other.restore_view(restored.clone()));
    assert_eq!(other.view.tab, Tab::Usage);
    assert_eq!(other.view.scroll_y, 120.0);
    let mut foreign = running();
    foreign.set_owner_scope(Some("@x:example.test"));
    assert!(!foreign.restore_view(restored));
    // Unknown/missing fields default rather than failing.
    let partial: ViewState = serde_json::from_str("{\"tab\":\"activity\",\"future\":1}").unwrap();
    assert_eq!(partial.tab, Tab::Activity);
}

#[test]
fn select_tab_resets_scroll_only_on_change() {
    let mut state = running();
    state.view.scroll_y = 50.0;
    assert!(!state.select_tab(Tab::Overview));
    assert_eq!(state.view.scroll_y, 50.0);
    assert!(state.select_tab(Tab::Activity));
    assert_eq!(state.view.scroll_y, 0.0);
}

#[test]
fn every_hosting_state_has_text_glyph_and_actions() {
    use HostingState::*;
    for st in [Unconfigured, Starting, Running, Paused, Draining, Stopped, Offline, Failed] {
        let mut state = DashboardState::new(Locale::ZhHans);
        state.set_owner_scope(Some(OWNER));
        state.apply_snapshot(ordinary(st), None);
        let h = state.present(clock()).hosting.unwrap();
        assert!(!h.title.is_empty() && !h.body.is_empty());
        assert_eq!(h.can_pause, h.show_pause, "{st:?}");
        assert_eq!(h.can_resume, h.show_resume, "{st:?}");
        assert_eq!(h.can_stop, h.show_stop, "{st:?}");
    }
}

#[test]
fn request_ids_match_the_daemon_charset() {
    let mut state = running();
    state.request_ids = None;
    let Some(DashboardAction::PauseHosting(ticket)) = state.request(CommandKind::Pause, FIXTURE_NOW_MS) else { panic!() };
    assert!(ticket.request_id.len() <= 64);
    assert!(ticket.request_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
}

#[test]
fn unknown_counts_constant_is_all_unknown() {
    assert_eq!(total_tokens(&UNKNOWN_COUNTS), None);
}

#[test]
fn pending_dimension_filter_never_displays_global_usage_as_filtered() {
    let mut state = running();
    state.view.model_filter=Some("gpt-selected".into());
    let presentation=state.present(clock());
    assert_eq!(presentation.usage.total,"Unknown");
    assert!(presentation.usage.by_resource.is_empty());
    assert_eq!(presentation.overview_usage.lines, running().present(clock()).overview_usage.lines);
}

#[test]
fn accepted_command_settles_after_later_state_change_but_unknown_does_not() {
    for hosting in [HostingState::Running, HostingState::Stopped, HostingState::Failed, HostingState::Offline] {
        let mut state = running();
        state.request(CommandKind::Pause, FIXTURE_NOW_MS).unwrap();
        state.command_result("fixture-1", CommandResult::Accepted, FIXTURE_NOW_MS);
        let mut snapshot = ordinary(hosting);
        snapshot.revision = 43;
        state.apply_snapshot(snapshot.clone(), None);
        assert_eq!(state.command.as_ref().unwrap().phase, CommandPhase::Superseded);
        assert!(state.present(clock()).command.unwrap().can_dismiss);
        state.dismiss_command();
        assert!(state.command.is_none());
        if matches!(hosting, HostingState::Failed | HostingState::Offline | HostingState::Stopped) {
            assert!(state.can(CommandKind::Resume, None));
            assert!(state.present(clock()).hosting.unwrap().show_resume);
        }
        let mut uncertain = fixtures::scenario("uncertain").unwrap();
        let ticket = uncertain.command.as_ref().unwrap().ticket.clone();
        uncertain.apply_snapshot(snapshot, None);
        uncertain.dismiss_command();
        assert_eq!(uncertain.command.as_ref().unwrap().ticket, ticket);
        assert!(uncertain.command.as_ref().unwrap().in_flight());
    }
}

#[test]
fn stale_and_wrong_owner_refresh_replies_stop_the_spinner() {
    let mut state = running();
    state.begin_refresh();
    let mut snapshot = ordinary(HostingState::Running);
    snapshot.revision = 1;
    assert_eq!(state.apply_snapshot(snapshot.clone(), None), SnapshotOutcome::Stale);
    assert!(!state.refreshing);
    state.begin_refresh();
    snapshot.owner.as_mut().unwrap().user_id = "@other:example.test".into();
    assert_eq!(state.apply_snapshot(snapshot, None), SnapshotOutcome::WrongOwner);
    assert!(!state.refreshing);
}

#[test]
fn original_refusal_settles_recovered_ticket_and_acceptance_can_arrive_from_another_host() {
    let mut state = fixtures::scenario("uncertain").unwrap();
    state.check_command().unwrap();
    state.command_result("fixture-1", CommandResult::Rejected { code: "revision_conflict".into(), message: String::new(), retryable: true }, FIXTURE_NOW_MS);
    assert!(matches!(state.command.as_ref().unwrap().phase, CommandPhase::Rejected { .. }));
    assert!(state.retry_rejected(FIXTURE_NOW_MS).is_some());

    let mut state = fixtures::scenario("uncertain").unwrap();
    let mut accepted = state.command.as_ref().unwrap().clone();
    accepted.phase = CommandPhase::Accepted;
    state.recover_command(accepted);
    let mut paused = ordinary(HostingState::Paused);
    paused.revision = 43;
    state.apply_snapshot(paused, None);
    assert!(matches!(state.command.as_ref().unwrap().phase, CommandPhase::Confirmed { .. }));
}
