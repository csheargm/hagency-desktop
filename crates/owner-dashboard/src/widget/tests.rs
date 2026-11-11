//! Headless checks of the production widget: DSL registration in light and
//! dark stylesheets, instantiation, and in-place sync of every fixture
//! without errors. Pixel/bounds evidence comes from the controller harness.
use super::*;
use crate::controls::OdListWidgetRefExt;
use crate::fixtures;
use crate::theme::{contrast, tokens_for_vm};
use makepad_widgets::desktop_style::{self, DesktopStyle, StyleSheet};

fn register(cx: &mut Cx, dark: bool) {
    cx.with_vm(|vm| {
        desktop_style::install(vm, StyleSheet::load(DesktopStyle::Macos.sheet(dark)) /* JIT PORT: sheets load by entry */);
        vm.bx.captured_errors = Some(Vec::new());
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "dashboard registration failed (dark={dark}): {errors:?}");
    });
}

fn instantiate(cx: &mut Cx) -> WidgetRef {
    cx.with_vm(|vm| {
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, { mod.widgets.OwnerDashboard{} });
        let widget = WidgetRef::script_from_value(vm, value);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "instantiation failed: {errors:?}");
        widget
    })
}

#[test]
fn tokens_are_readable_in_light_and_dark() {
    for dark in [false, true] {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        register(&mut cx, dark);
        let t = cx.with_vm(tokens_for_vm);
        assert_eq!(t.dark, dark);
        for (name, fg, bg) in [
            ("ink/page", t.ink, t.page),
            ("ink/surface", t.ink, t.surface),
            ("muted/surface", t.muted, t.surface),
            ("muted/page", t.muted, t.page),
            ("muted/field", t.muted, t.field),
            ("accent/surface", t.accent, t.surface),
            ("accent/field", t.accent, t.field),
            ("primary/fill", t.on_accent, t.accent_fill),
            ("primary/hover", t.on_accent, t.accent_hover),
            ("primary/down", t.on_accent, t.accent_down),
            ("positive", t.positive.0, t.surface),
            ("positive/bg", t.positive.0, t.positive.1),
            ("caution", t.caution.0, t.surface),
            ("caution/bg", t.caution.0, t.caution.1),
            ("critical", t.critical.0, t.surface),
            ("critical/bg", t.critical.0, t.critical.1),
        ] {
            assert!(contrast(fg, bg) >= 4.5, "dark={dark} {name}: {}", contrast(fg, bg));
        }
        assert!(t.control_height >= 44.0, "primary targets are at least 44 points");
        assert_ne!(t.page, t.surface, "grouped cards stay distinct from the page");
    }
}

#[test]
fn larger_text_scales_tokens_and_control_height() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    crate::theme::set_text_scale(&mut cx, 2.0);
    register(&mut cx, false);
    let t = cx.with_vm(tokens_for_vm);
    assert_eq!(t.text_scale, 2.0);
    assert!(t.control_height >= 24.0 * 2.0 + 20.0);
}

#[test]
fn every_fixture_syncs_in_place_in_both_locales() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_clock(&mut cx, Some(fixtures::clock()));
    for locale in [Locale::En, Locale::ZhHans] {
        dashboard.set_locale(&mut cx, locale);
        for name in fixtures::SCENARIOS {
            let state = fixtures::scenario(name).unwrap();
            dashboard.load_state(&mut cx, state);
            let errors = cx.with_vm(|vm| vm.take_errors());
            assert!(errors.is_empty(), "{name}/{locale:?}: {errors:?}");
        }
    }
    assert!(dashboard.sync_count() >= 2 * fixtures::SCENARIOS.len() as u64);
}

#[test]
fn stable_ids_exist_for_the_harness() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    for path in [
        ids!(od_refresh), ids!(od_setup), ids!(od_reconnect), ids!(od_pause), ids!(od_resume),
        ids!(od_stop), ids!(od_stop_confirm), ids!(od_stop_cancel), ids!(od_check_command), ids!(od_retry_command),
        ids!(od_dismiss_command), ids!(od_hosting_setup), ids!(od_detail_back), ids!(od_setup_start),
        ids!(od_setup_cancel), ids!(od_uninstall), ids!(od_uninstall_cancel), ids!(od_uninstall_confirm),
        ids!(od_use_this_service),
    ] {
        assert!(!widget.button(&cx, path).is_empty(), "missing button {path:?}");
    }
    for path in [
        ids!(od_tab_overview), ids!(od_tab_resources), ids!(od_tab_usage), ids!(od_tab_activity), ids!(od_period_day),
        ids!(od_period_week), ids!(od_period_month), ids!(od_content), ids!(od_connection_banner), ids!(od_command_row),
        ids!(od_stop_sheet), ids!(od_hosting_card), ids!(od_empty_page), ids!(od_detail), ids!(od_setup_page),
        ids!(od_options_card), ids!(od_uninstall_sheet),
    ] {
        assert!(!widget.view(&cx, path).is_empty(), "missing view {path:?}");
    }
    for path in [ids!(od_hosting_state), ids!(od_command_status), ids!(od_sync), ids!(od_banner_title), ids!(od_setup_title)] {
        assert!(!widget.label(&cx, path).is_empty(), "missing label {path:?}");
    }
    assert!(!widget.check_box(&cx, ids!(od_keep_running)).is_empty(), "missing switch");
    assert!(widget.widget(&cx, ids!(od_open_other)).is_empty(), "no cross-app button remains");
}

#[test]
fn setup_page_replaces_the_dashboard_and_the_switch_follows_the_host() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_clock(&mut cx, Some(fixtures::clock()));
    let visible = |cx: &Cx, path: &[LiveId]| widget.widget(cx, path).visible();
    dashboard.load_state(&mut cx, fixtures::scenario("setup").unwrap());
    assert!(visible(&cx, ids!(od_setup_page)));
    assert!(!visible(&cx, ids!(od_overview)) && !visible(&cx, ids!(od_tabs)) && !visible(&cx, ids!(od_connection_banner)));
    assert!(widget.button(&cx, ids!(od_setup_start)).visible() && !widget.button(&cx, ids!(od_setup_cancel)).visible());
    assert_eq!(widget.label(&cx, ids!(od_setup_title)).text(), "Set up background hosting");
    dashboard.load_state(&mut cx, fixtures::scenario("setup-signing-in").unwrap());
    assert!(visible(&cx, ids!(od_setup_progress)) && widget.button(&cx, ids!(od_setup_cancel)).visible());
    assert!(!widget.button(&cx, ids!(od_setup_start)).visible());
    dashboard.load_state(&mut cx, fixtures::scenario("setup-other-owner").unwrap());
    assert!(!widget.button(&cx, ids!(od_setup_start)).visible());
    // Hosted: the dashboard with its options card and the saved switch value.
    dashboard.load_state(&mut cx, fixtures::scenario("hosted").unwrap());
    assert!(!visible(&cx, ids!(od_setup_page)) && visible(&cx, ids!(od_overview)) && visible(&cx, ids!(od_options_card)));
    assert!(widget.check_box(&cx, ids!(od_keep_running)).active(&cx));
    assert!(widget.button(&cx, ids!(od_stop)).visible());
    dashboard.set_options(&mut cx, Some(crate::model::HostOptions { keep_running: false, busy: false, foreign_service: false }));
    assert!(!widget.check_box(&cx, ids!(od_keep_running)).active(&cx));
    dashboard.load_state(&mut cx, fixtures::scenario("hosted-stopped").unwrap());
    assert!(widget.button(&cx, ids!(od_resume)).visible() && !widget.button(&cx, ids!(od_stop)).visible());
    dashboard.load_state(&mut cx, fixtures::scenario("uninstall-confirm").unwrap());
    assert!(visible(&cx, ids!(od_uninstall_sheet)) && !visible(&cx, ids!(od_service_actions)));
    assert_eq!(widget.label(&cx, ids!(od_uninstall_title)).text(), "Uninstall the hosting service?");
    dashboard.load_state(&mut cx, fixtures::scenario("foreign-service").unwrap());
    assert!(widget.button(&cx, ids!(od_use_this_service)).visible() && visible(&cx, ids!(od_foreign_service)));
}

#[test]
fn rows_are_reused_across_refreshes() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_clock(&mut cx, Some(fixtures::clock()));
    dashboard.set_owner_scope(&mut cx, Some(fixtures::OWNER));
    dashboard.set_connection(&mut cx, ConnectionStatus::Connected);
    dashboard.set_snapshot(&mut cx, fixtures::refresh_step(0));
    let uids = |cx: &Cx| -> Vec<WidgetUid> {
        widget.od_list(cx, ids!(od_accounts)).rows().iter().map(|(_, r)| r.widget_uid()).collect()
    };
    let activity = |cx: &Cx| -> Vec<WidgetUid> {
        widget.od_list(cx, ids!(od_activity)).rows().iter().map(|(_, r)| r.widget_uid()).collect()
    };
    let (before, before_activity) = (uids(&cx), activity(&cx));
    assert_eq!(before.len(), 2);
    assert_eq!(before_activity.len(), 4);
    for step in 1..=10 {
        let outcome = dashboard.set_snapshot(&mut cx, fixtures::refresh_step(step));
        assert_eq!(outcome, Some(SnapshotOutcome::Applied));
    }
    assert_eq!(uids(&cx), before, "plan cards were remounted by a refresh");
    assert_eq!(activity(&cx), before_activity, "activity rows were remounted by a refresh");
    // A late, older snapshot is ignored.
    assert_eq!(dashboard.set_snapshot(&mut cx, fixtures::refresh_step(3)), Some(SnapshotOutcome::Stale));
    assert_eq!(dashboard.state().unwrap().snapshot.unwrap().revision, 52);
}

#[test]
fn duplicate_activity_ids_keep_each_existing_row_on_refresh() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_owner_scope(&mut cx, Some(fixtures::OWNER));
    let mut snapshot = fixtures::refresh_step(0);
    snapshot.activities.push(snapshot.activities[0].clone());
    dashboard.set_snapshot(&mut cx, snapshot.clone());
    let uids = |cx: &Cx| -> Vec<WidgetUid> {
        widget.od_list(cx, ids!(od_activity)).rows().iter().map(|(_, row)| row.widget_uid()).collect()
    };
    let before = uids(&cx);
    snapshot.revision += 1;
    dashboard.set_snapshot(&mut cx, snapshot);
    assert_eq!(uids(&cx), before);
}

#[test]
fn command_flow_through_the_widget_api() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_clock(&mut cx, Some(fixtures::clock()));
    dashboard.load_state(&mut cx, fixtures::scenario("uncertain").unwrap());
    let state = dashboard.state().unwrap();
    assert_eq!(state.command.as_ref().unwrap().phase, CommandPhase::Uncertain);
    // The connector reports the lookup result for the same ID.
    dashboard.command_result(&mut cx, "fixture-1", CommandResult::Completed);
    let state = dashboard.state().unwrap();
    assert!(matches!(state.command.unwrap().phase, CommandPhase::Confirmed { .. }));
    // Owner switch clears everything private.
    dashboard.set_owner_scope(&mut cx, Some("@other:example.test"));
    let state = dashboard.state().unwrap();
    assert!(state.snapshot.is_none() && state.command.is_none());
}

#[test]
fn pagination_reaches_rows_beyond_two_hundred_and_clamps_after_removal() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    let mut state = fixtures::scenario("running").unwrap();
    state.view.tab = Tab::Resources;
    state.view.pages.insert("resources".into(), 10);
    let template = state.snapshot.as_ref().unwrap().resources[0].clone();
    state.snapshot.as_mut().unwrap().resources = (0..231).map(|i| {
        let mut resource = template.clone(); resource.id=format!("r{i:03}"); resource.name=resource.id.clone(); resource
    }).collect();
    dashboard.load_state(&mut cx,state.clone());
    {
        let mut inner=dashboard.borrow_mut().unwrap(); inner.sync(&mut cx);
        let rows=inner.view.od_list(&cx,ids!(od_resources)).rows();
        assert_eq!(rows.len(),PAGE_ROWS);
        assert_eq!(rows.first().unwrap().0,"r200");
        assert_eq!(rows.last().unwrap().0,"r219");
        assert_eq!(inner.pagers.iter().find(|(key,_,_)| key=="resources").unwrap().2,231);
    }
    state.snapshot.as_mut().unwrap().resources.truncate(7);
    dashboard.load_state(&mut cx,state);
    let mut inner=dashboard.borrow_mut().unwrap(); inner.sync(&mut cx);
    let rows=inner.view.od_list(&cx,ids!(od_resources)).rows();
    assert_eq!(rows.len(),7);
    assert_eq!(rows[0].0,"r000");
}

#[test]
fn host_typography_alone_selects_compact_navigation_without_remounting() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    cx.with_vm(|vm| {
        script_eval!(vm, { mod.theme.octo_typography_scale = 1.5 });
        crate::script_mod(vm);
    });
    assert_eq!(crate::theme::effective_text_scale(&mut cx), 1.5);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.load_state(&mut cx, fixtures::scenario("running").unwrap());
    assert!(widget.widget(&cx, ids!(od_owner_menu)).visible());
    assert!(widget.widget(&cx, ids!(od_destination_row)).visible());
    assert!(!widget.widget(&cx, ids!(od_identity_actions)).visible());
    let ids_before: Vec<_> = [ids!(od_content), ids!(od_refresh), ids!(od_destination), ids!(od_usage_card)]
        .iter().map(|path| widget.widget(&cx, *path).widget_uid()).collect();
    {
        let mut inner = dashboard.borrow_mut().unwrap();
        inner.header_expanded = true;
        inner.changed(&mut cx);
    }
    assert!(widget.widget(&cx, ids!(od_identity_actions)).visible());
    assert!(widget.widget(&cx, ids!(od_freshness)).visible());
    dashboard.set_snapshot(&mut cx, fixtures::refresh_step(1));
    assert!(widget.widget(&cx, ids!(od_identity_actions)).visible(), "refresh must not collapse owner controls");
    let ids_after: Vec<_> = [ids!(od_content), ids!(od_refresh), ids!(od_destination), ids!(od_usage_card)]
        .iter().map(|path| widget.widget(&cx, *path).widget_uid()).collect();
    assert_eq!(ids_before, ids_after);
}

#[test]
fn narrow_resource_rows_keep_full_width_metadata_and_confirmation_is_contextual() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    crate::theme::set_text_scale(&mut cx, 2.0);
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.load_state(&mut cx, fixtures::scenario("running").unwrap());
    let rows = widget.od_list(&cx, ids!(od_resources)).rows();
    assert!(!rows.is_empty());
    for (_, row) in rows {
        assert!(matches!(row.borrow::<View>().unwrap().layout.flow, Flow::Down));
        assert!(matches!(row.view(&cx, ids!(resource_actions)).borrow().unwrap().walk.width, Size::Fill { .. }));
    }
    dashboard.load_state(&mut cx, fixtures::scenario("stop-confirm").unwrap());
    assert!(!widget.widget(&cx, ids!(od_hosting_summary)).visible());
    assert!(widget.widget(&cx, ids!(od_stop_sheet)).visible());
    assert!(widget.button(&cx, ids!(od_stop_cancel)).visible());
    assert!(widget.button(&cx, ids!(od_stop_confirm)).visible());
    assert_eq!(widget.label(&cx, ids!(od_stop_title)).text(), "Stop hosting?");
}

#[test]
fn navigation_uses_available_width_and_effective_typography() {
    assert!(!compact_navigation(400.0, 1.0));
    assert!(compact_navigation(400.0, 1.25));
    assert!(!compact_navigation(960.0, 1.25));
    assert!(compact_navigation(960.0, 2.0));
    assert!(compact_navigation(360.0, 1.25));
}

#[test]
fn completed_action_does_not_mask_disconnection_or_replace_filter_drafts() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    register(&mut cx, false);
    let widget = instantiate(&mut cx);
    let dashboard = widget.as_owner_dashboard();
    dashboard.set_clock(&mut cx, Some(fixtures::clock()));
    dashboard.load_state(&mut cx, fixtures::scenario("running").unwrap());
    {
        let mut inner = dashboard.borrow_mut().unwrap();
        inner.state.request(CommandKind::Pause, fixtures::FIXTURE_NOW_MS);
        let id = inner.state.command.as_ref().unwrap().ticket.request_id.clone();
        inner.state.command_result(&id, CommandResult::Completed, fixtures::FIXTURE_NOW_MS);
        inner.changed(&mut cx);
    }
    dashboard.set_notice(&mut cx, Some(Status::new(Tone::Positive, Glyph::Check, "Earlier setup completed")));
    let field = widget.text_input(&cx, ids!(od_date_start));
    field.set_text(&mut cx, "2026-10-14");
    let uid = field.widget_uid();
    dashboard.set_connection(&mut cx, ConnectionStatus::Failed { message: "Connection lost".into() });
    assert_eq!(widget.label(&cx, ids!(od_sync)).text(), "Disconnected");
    {
        let inner = dashboard.borrow().unwrap();
        assert_eq!(inner.header_status(&inner.state.present(inner.clock())).target, HeaderTarget::Connection);
        assert!(inner.pending_scroll.is_none() && inner.pending_reveal.is_none());
    }
    assert_eq!(field.text(), "2026-10-14");
    assert_eq!(field.widget_uid(), uid);
    dashboard.set_owner_scope(&mut cx, Some("@next:example.test"));
    assert_eq!(field.text(), "");
    assert!(!widget.widget(&cx, ids!(od_notice)).visible());
}
