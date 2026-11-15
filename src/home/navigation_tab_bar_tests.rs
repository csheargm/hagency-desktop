//! The desktop left rail (UX item A1), built and drawn offscreen.
use super::*;
use super::more::MORE_ITEMS;
use crate::home::sidebar_action_menu::SidebarMenuAction;
use crate::i18n::{text_for, Language};
use crate::moments::ui::MomentsAction;
use crate::octoscript_apps::MiniAppsAction;
use crate::shared::navigation_bar_button::NavigationBarButtonAction;
use makepad_widgets::makepad_draw::cx_draw::CxDraw;

/// The two primary items, top of the rail.
const PRIMARY: [&[LiveId; 1]; 2] = [ids!(home_button), ids!(agent_button)];
/// The bottom group: "More", then Settings (the avatar) and the account menu.
const BOTTOM: [&[LiveId; 1]; 3] = [ids!(more_button), ids!(profile_icon), ids!(account_switcher_button)];
/// Plain rail buttons that are on screen while "More" is closed, top to bottom.
const CLOSED_BUTTONS: [&[LiveId; 1]; 4] = [ids!(home_button), ids!(agent_button), ids!(more_button), ids!(account_switcher_button)];

fn rail() -> (Cx, NavigationTabBar) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    // A desktop-sized window, so the bar is the left rail.
    cx.display_context.screen_size = dvec2(1280.0, 800.0);
    let bar = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.NavigationTabBar {}});
        let bar = NavigationTabBar::script_from_value(vm, value);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "Navigation bar registration: {errors:?}");
        bar
    });
    (cx, bar)
}

fn draw(cx: &mut Cx, bar: &mut NavigationTabBar) {
    draw_at(cx, bar, 800.0)
}

fn draw_at(cx: &mut Cx, bar: &mut NavigationTabBar, height: f64) {
    cx.redraw_all();
    let size = dvec2(1280.0, height);
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut list = DrawList2d::new(cx);
    // A full repaint, as after a click: a default event only paints new or resized lists.
    let event = DrawEvent {redraw_all: true, ..DrawEvent::default()};
    let mut draw = CxDraw::new(cx, &event);
    let mut cx2d = Cx2d::new(&mut draw);
    cx2d.begin_pass(&pass, None);
    list.begin_always(&mut cx2d);
    cx2d.begin_root_turtle(size, Layout::flow_right());
    bar.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk {
        width: Size::Fixed(76.0),
        height: Size::Fixed(size.y),
        ..Walk::default()
    });
    cx2d.end_pass_sized_turtle();
    list.end(&mut cx2d);
    cx2d.end_pass(&pass);
}

fn click(cx: &mut Cx, bar: &mut NavigationTabBar, button: &[LiveId; 1]) -> ActionsBuf {
    let uid = bar.view.navigation_bar_button(cx, button).widget_uid();
    let clicked = cx.capture_actions(|cx| cx.widget_action(uid, NavigationBarButtonAction::Clicked));
    cx.capture_actions(|cx| bar.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()))
}

fn selected(cx: &mut Cx, bar: &NavigationTabBar, button: &[LiveId; 1]) -> bool {
    let button = bar.view.navigation_bar_button(cx, button);
    let inner = button.borrow().expect("rail button exists");
    inner.animator_in_state(cx, ids!(active.on))
}

fn rect(cx: &mut Cx, bar: &NavigationTabBar, path: &[LiveId; 1]) -> Rect {
    bar.view.widget(cx, path).area().rect(cx)
}

fn shown(cx: &mut Cx, bar: &NavigationTabBar, path: &[LiveId; 1]) -> bool {
    // A hidden widget keeps its last rect, so ask whether it is visible too.
    let r = rect(cx, bar, path);
    bar.view.widget(cx, path).visible() && r.size.x > 0.0 && r.size.y > 0.0
}

fn emits<T: ActionTrait>(actions: &ActionsBuf, matches: impl Fn(&T) -> bool) -> usize {
    actions.iter().filter(|a| a.downcast_ref::<T>().is_some_and(&matches)).count()
}

fn click_settings(cx: &mut Cx, bar: &mut NavigationTabBar) -> ActionsBuf {
    let uid = bar.view.profile_icon(cx, ids!(profile_icon)).borrow().expect("avatar exists").inner.widget_uid();
    let clicked = cx.capture_actions(|cx| cx.widget_action(uid, NavigationBarButtonAction::Clicked));
    cx.capture_actions(|cx| bar.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()))
}

fn open_more(cx: &mut Cx, bar: &mut NavigationTabBar) {
    click(cx, bar, ids!(more_button));
    draw(cx, bar);
}

#[test]
fn rail_shows_only_chats_and_agents_above_more_settings_and_account() {
    let (mut cx, mut bar) = rail();
    draw(&mut cx, &mut bar);
    assert_eq!(bar.view.active_variant(), Some(id!(Desktop)));
    for (button, english, chinese) in [(PRIMARY[0], "Chats", "聊天"), (PRIMARY[1], "Agents", "智能体")] {
        let found = bar.view.navigation_bar_button(&mut cx, button);
        assert_eq!(found.borrow().expect("rail button exists").tooltip_text(), english);
        assert_eq!(text_for(Language::Chinese, english), chinese);
        assert!(bar.view.label(&mut cx, &[button[0], id!(label)]).text().starts_with(english), "{english} has a caption");
    }
    // Nothing else is on the rail between them and the bottom group.
    assert!(!bar.view.view(&mut cx, ids!(more_spaces)).visible());
    for (item, _) in MORE_ITEMS {
        assert!(!shown(&mut cx, &bar, item), "{item:?} waits behind More");
    }
    assert!(!shown(&mut cx, &bar, ids!(root_spaces_bar)) && !shown(&mut cx, &bar, ids!(top_separator)));
    for gone in [ids!(new_or_join_button), ids!(add_room_button), ids!(appearance_button), ids!(toggle_spaces_bar_button)] {
        assert!(bar.view.widget(&mut cx, gone).is_empty(), "{gone:?} is not on the rail");
    }
    // Chats and Agents on top; More, Settings and Account pinned to the bottom.
    let agents = rect(&mut cx, &bar, PRIMARY[1]);
    let mut above = agents.pos.y + agents.size.y;
    assert!(rect(&mut cx, &bar, PRIMARY[0]).pos.y < agents.pos.y);
    for button in BOTTOM {
        let r = rect(&mut cx, &bar, button);
        assert!(r.size.x >= 44.0 && r.size.y >= 44.0 && r.pos.y >= above, "{button:?} {r:?}");
        above = r.pos.y + r.size.y;
    }
    assert!(above > 800.0 - 40.0, "the bottom group sits at the bottom: {above}");
    assert!(rect(&mut cx, &bar, BOTTOM[0]).pos.y - (agents.pos.y + agents.size.y) > 300.0, "empty space between the groups");
}

#[test]
fn every_moved_entry_is_one_click_behind_more_and_does_what_it_did() {
    let (mut cx, mut bar) = rail();
    draw(&mut cx, &mut bar);
    open_more(&mut cx, &mut bar);
    assert!(selected(&mut cx, &bar, ids!(more_button)), "More is marked while open");
    // The spaces bar (empty in tests: no spaces joined) and its divider come back.
    assert!(bar.view.view(&mut cx, ids!(more_spaces)).visible() && shown(&mut cx, &bar, ids!(top_separator)), "the spaces are behind More");
    let more = rect(&mut cx, &bar, ids!(more_button));
    let mut above = rect(&mut cx, &bar, PRIMARY[1]).pos.y;
    for (item, english) in MORE_ITEMS {
        let r = rect(&mut cx, &bar, item);
        assert!(r.size.x >= 44.0 && r.size.y >= 44.0, "{english} is a full target: {r:?}");
        assert!(r.pos.y > above && r.pos.y + r.size.y <= more.pos.y + 0.5, "{english} sits in order above More: {r:?}");
        above = r.pos.y;
        let tooltip = bar.view.navigation_bar_button(&mut cx, item).borrow().expect("item exists").tooltip_text().to_owned();
        assert_eq!(tooltip, english);
    }
    let contacts = click(&mut cx, &mut bar, ids!(contacts_button));
    assert_eq!(emits::<NavigationBarAction>(&contacts, |a| *a == NavigationBarAction::GoToTab(SelectedTab::Contacts)), 1);
    let moments = click(&mut cx, &mut bar, ids!(moments_button));
    assert_eq!(emits::<MomentsAction>(&moments, |a| matches!(a, MomentsAction::Open { author: None })), 1);
    let apps = click(&mut cx, &mut bar, ids!(octoscript_apps_button));
    assert_eq!(emits::<MiniAppsAction>(&apps, |a| matches!(a, MiniAppsAction::Open)), 1);
    let files = click(&mut cx, &mut bar, ids!(file_transfer_button));
    assert_eq!(emits::<MomentsAction>(&files, |a| matches!(a, MomentsAction::FileTransfer)), 1);
    assert_eq!(emits::<NavigationBarAction>(&files, |_| true), 0, "File Transfer opens only its chat");
    // Agents and Settings stay one click from the rail itself.
    let agents = click(&mut cx, &mut bar, ids!(agent_button));
    assert_eq!(emits::<MiniAppsAction>(&agents, |a| matches!(a, MiniAppsAction::OpenAgent)), 1);
    let settings = click_settings(&mut cx, &mut bar);
    assert_eq!(emits::<NavigationBarAction>(&settings, |a| *a == NavigationBarAction::OpenSettings), 1);
    // No rail button opens the New or Join menu.
    for button in CLOSED_BUTTONS.into_iter().chain(MORE_ITEMS.map(|(item, _)| item)) {
        let emitted = click(&mut cx, &mut bar, button);
        assert_eq!(emits::<SidebarMenuAction>(&emitted, |_| true), 0, "{button:?} opens no sidebar menu");
    }
}

#[test]
fn more_closes_again_and_a_page_behind_it_reopens_it() {
    let (mut cx, mut bar) = rail();
    draw(&mut cx, &mut bar);
    open_more(&mut cx, &mut bar);
    open_more(&mut cx, &mut bar);
    assert!(!selected(&mut cx, &bar, ids!(more_button)));
    assert!(!bar.view.view(&mut cx, ids!(more_spaces)).visible(), "a second click closes More");
    assert!(!shown(&mut cx, &bar, ids!(contacts_button)));
    // Contacts opened elsewhere (e.g. the mobile title bar) still shows its marked item.
    let confirmed = cx.capture_actions(|cx| cx.action(NavigationBarAction::TabSelected(SelectedTab::Contacts)));
    bar.handle_event(&mut cx, &Event::Actions(confirmed), &mut Scope::empty());
    draw(&mut cx, &mut bar);
    assert!(shown(&mut cx, &bar, ids!(contacts_button)) && selected(&mut cx, &bar, ids!(contacts_button)));
}

#[test]
fn short_windows_keep_every_rail_button_on_screen_and_at_least_44_points() {
    for open in [false, true] {
        // Open, the rail carries eight 44-point targets plus the avatar, which
        // needs about 510 points; closed, it fits even a 400-point window.
        let heights: &[f64] = if open { &[800.0, 600.0, 520.0] } else { &[800.0, 600.0, 480.0, 400.0] };
        for &height in heights {
            let (mut cx, mut bar) = rail();
            draw_at(&mut cx, &mut bar, height);
            if open {
                click(&mut cx, &mut bar, ids!(more_button));
                draw_at(&mut cx, &mut bar, height);
            }
            let items = MORE_ITEMS.map(|(item, _)| item);
            let buttons: Vec<_> = if open {
                PRIMARY.into_iter().chain(items).chain([ids!(more_button), ids!(account_switcher_button)]).collect()
            } else {
                CLOSED_BUTTONS.to_vec()
            };
            let mut above = 0.0;
            for button in buttons {
                let r = rect(&mut cx, &bar, button);
                assert!(r.size.y >= 44.0 - 0.5 && r.size.x >= 44.0, "{open} {height}: {button:?} {r:?}");
                assert!(r.pos.y >= above - 0.5 && r.pos.y + r.size.y <= height + 0.5, "{open} {height}: {button:?} is on the rail: {r:?}");
                above = r.pos.y + r.size.y;
            }
        }
    }
}

#[test]
fn explore_rooms_keeps_chats_marked() {
    let (mut cx, mut bar) = rail();
    draw(&mut cx, &mut bar);
    assert!(selected(&mut cx, &bar, ids!(home_button)));
    let marked_buttons = [ids!(home_button), ids!(contacts_button)];
    for (tab, marked) in [
        (SelectedTab::AddRoom, ids!(home_button)),
        (SelectedTab::Contacts, ids!(contacts_button)),
        (SelectedTab::AddRoom, ids!(home_button)),
        (SelectedTab::Home, ids!(home_button)),
    ] {
        let confirmed = cx.capture_actions(|cx| cx.action(NavigationBarAction::TabSelected(tab.clone())));
        bar.handle_event(&mut cx, &Event::Actions(confirmed), &mut Scope::empty());
        for button in marked_buttons {
            assert_eq!(selected(&mut cx, &bar, button), button == marked, "{tab:?}: {button:?}");
        }
    }
}
#[test]
fn approval_alert_waits_for_the_agent_icon_and_reapplies_after_layout_and_theme_changes() {
    let (mut cx, mut bar) = rail();
    cx.with_vm(|vm| vm.bx.captured_errors = Some(Vec::new()));
    bar.apply_view_mode(ViewModeOverride::ForceNarrow);
    draw(&mut cx, &mut bar);
    assert_eq!(bar.view.active_variant(), Some(id!(Mobile)));
    assert!(bar.view.icon(&mut cx, ids!(agent_button.icon)).is_empty());
    bar.apply_approval_alert_state(&mut cx, true);
    assert_eq!(bar.approval_alert, None, "defer until the desktop rail exists");
    assert!(cx.with_vm(|vm| vm.take_errors()).is_empty(), "missing icon must not invoke script evaluation");

    bar.apply_view_mode(ViewModeOverride::ForceWide);
    draw(&mut cx, &mut bar);
    let label = bar.view.label(&mut cx, ids!(agent_button.label));
    assert!(!bar.view.icon(&mut cx, ids!(agent_button.icon)).is_empty());
    cx.with_vm(|vm| vm.bx.captured_errors = Some(Vec::new()));
    bar.apply_approval_alert_state(&mut cx, true);
    assert!(label.text().ends_with('●'));
    assert_eq!(label.borrow().unwrap().draw_text.color, bar.appearance.role("color.status.danger.foreground"));
    assert!(cx.with_vm(|vm| vm.take_errors()).is_empty(), "the real icon must receive the alert color");

    // Reapplying a theme must restore the color even when the text did not change.
    label.set_text_color(&mut cx, vec4(0.0, 0.0, 0.0, 0.0));
    cx.with_vm(|vm| bar.on_after_apply(vm, &Apply::ScriptReapply, &mut Scope::empty(), ScriptValue::default()));
    bar.apply_approval_alert_state(&mut cx, true);
    assert_eq!(label.borrow().unwrap().draw_text.color, bar.appearance.role("color.status.danger.foreground"));

    bar.apply_view_mode(ViewModeOverride::ForceNarrow);
    draw(&mut cx, &mut bar);
    bar.apply_approval_alert_state(&mut cx, true);
    assert_eq!(bar.approval_alert, None, "mobile has no Agent rail icon");
    bar.apply_view_mode(ViewModeOverride::ForceWide);
    draw(&mut cx, &mut bar);
    bar.apply_approval_alert_state(&mut cx, true);
    assert!(bar.view.label(&mut cx, ids!(agent_button.label)).text().ends_with('●'));
    bar.apply_approval_alert_state(&mut cx, false);
    assert!(!bar.view.label(&mut cx, ids!(agent_button.label)).text().ends_with('●'));
}
