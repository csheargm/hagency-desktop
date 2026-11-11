use makepad_widgets::*;
use crate::miniapps::side::MiniAppsSideRegionWidgetExt;
use super::back_swipe::BackSwipe;

use crate::{
    app::{AppState, AppStateAction, SelectedRoom},
    home::{
        mobile_chat_info::MobileChatInfoWidgetRefExt,
        invite_screen::InviteScreenWidgetRefExt,
        navigation_tab_bar::{NavigationBarAction, SelectedTab},
        room_screen::RoomScreenWidgetRefExt,
        rooms_list::{AcceptedInviteKind, RoomsListAction},
        space_lobby::SpaceLobbyScreenWidgetRefExt,
        spaces_bar::SpacesBarAction,
    },
    settings::{
        app_preferences::{AppPreferencesGlobal, AppPreferencesAction, ViewModeOverride},
        settings_screen::SettingsScreenWidgetRefExt,
    },
    profile::user_profile::UserProfileSlidingPaneWidgetRefExt,
    shared::mention_popup::MentionablePopupRef,
    utils::RoomNameId,
};


script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    // Defines the total height of the StackNavigationView's header.
    // This has to be set in multiple places because of how StackNavigation
    // uses an Overlay view internally.
    mod.widgets.STACK_VIEW_HEADER_HEIGHT = 45

    // A reusable base for StackNavigationView children in the mobile layout.
    // Each specific screen view (room, invite, space lobby) extends this
    // and places its own screen widget inside the body.
    mod.widgets.RobrixStackNavigationView = StackNavigationView {
        width: Fill, height: Fill
        draw_bg.color: (COLOR_PRIMARY)
        header +: {
            height: (mod.widgets.STACK_VIEW_HEADER_HEIGHT),
            padding: 0
            align: Align{y: 0.5}

            show_bg: true
            draw_bg +: {
                color: mod.widgets.RINX_PAGE
                pixel: fn() {return self.color}
            }

            content +: {
                height: (mod.widgets.STACK_VIEW_HEADER_HEIGHT)
                align: Align{y: 0.5}
                padding: Inset{
                    left: (mod.widgets.SAFE_INSET_PAD_LEFT),
                    right: (mod.widgets.SAFE_INSET_PAD_RIGHT),
                }
                button_container +: {
                    padding: 0,
                    margin: 0
                    left_button +: {
                        width: 48 height: (mod.widgets.STACK_VIEW_HEADER_HEIGHT)
                        padding: 0 margin: 0
                        align: Align{x: 0.5 y: 0.5}
                        draw_icon +: {svg: ICON_CHEVRON_LEFT color: (ROOM_NAME_TEXT_COLOR)}
                        icon_walk: Walk{width: 8 height: 14}
                        spacing: 0
                        text: ""
                    }
                }
                title_container +: {
                    // padding: Inset{top: 8}
                    title +: {
                        draw_text +: {
                            color: mod.widgets.RINX_INK
                            text_style: theme.font_bold {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}
                        }
                    }
                }
            }
        }
        body +: {
            // The top margin leaves room for the stack nav header.
            // The other padding is for safe inset areas.
            margin: Inset{top: (mod.widgets.STACK_VIEW_HEADER_HEIGHT)}
            padding: Inset{
                left: (mod.widgets.SAFE_INSET_PAD_LEFT),
                right: (mod.widgets.SAFE_INSET_PAD_RIGHT),
                bottom: (mod.widgets.SAFE_INSET_PAD_BOTTOM),
            }
        }
    }

    // A wrapper view around the SpacesBar that lets us show/hide it via animation.
    mod.widgets.SpacesBarWrapper = set_type_default() do #(SpacesBarWrapper::register_widget(vm)) {
        ..mod.widgets.RoundedShadowView

        width: Fill,
        height: (NAVIGATION_TAB_BAR_SIZE)
        margin: Inset{
            left: (4.0 + mod.widgets.SAFE_INSET_PAD_LEFT),
            right: (4.0 + mod.widgets.SAFE_INSET_PAD_RIGHT),
        }
        show_bg: true
        draw_bg +: {
            color: (COLOR_PRIMARY_DARKER)
            border_radius: 4.0
            border_size: 0.0
            shadow_color: #0005
            shadow_radius: 15.0
            shadow_offset: vec2(1.0, 0.0)
        }

        CachedWidget {
            root_spaces_bar := mod.widgets.SpacesBar {}
        }

        animator: Animator{
            spaces_bar_animator: {
                default: @hide
                show: AnimatorState{
                    redraw: true
                    from: { all: Forward { duration: (mod.widgets.SPACES_BAR_ANIMATION_DURATION_SECS) } }
                    apply: { height: (NAVIGATION_TAB_BAR_SIZE),  draw_bg: { shadow_color: #x00000055 } }
                }
                hide: AnimatorState{
                    redraw: true
                    from: { all: Forward { duration: (mod.widgets.SPACES_BAR_ANIMATION_DURATION_SECS) } }
                    apply: { height: 0,  draw_bg: { shadow_color: (COLOR_TRANSPARENT) } }
                }
            }
        }
    }

    // The home screen widget contains the main content:
    // rooms list, room screens, and the settings screen as an overlay.
    // It adapts to both desktop and mobile layouts.
    mod.widgets.HomeScreen = #(HomeScreen::register_widget(vm)) {
        main_adaptive_view := AdaptiveView {
            // NOTE: within each of these sub views, we used `CachedWidget` wrappers
            //       to ensure that there is only a single global instance of each
            //       of those widgets, which means they maintain their state
            //       across transitions between the Desktop and Mobile variant.
            Desktop := SolidView {
                width: Fill, height: Fill
                flow: Right
                align: Align{x: 0.0, y: 0.0}
                padding: 0,
                margin: 0,

                show_bg: true
                draw_bg +: {
                    color: (COLOR_SECONDARY)
                }

                CachedWidget {
                    navigation_tab_bar := mod.widgets.NavigationTabBar {}
                }

                // To the right of that, we use the PageFlip widget to show either
                // the main desktop UI or the settings screen.
                home_screen_page_flip := PageFlip {
                    width: Fill, height: Fill
                    // We only need bottom and right-side padding,
                    // as the others are handled by the parent widget
                    // or by the navigation bar.
                    padding: Inset{
                        bottom: (mod.widgets.SAFE_INSET_PAD_BOTTOM),
                        right: (mod.widgets.SAFE_INSET_PAD_RIGHT),
                    }

                    lazy_init: true,
                    active_page: @home_page

                    home_page := View {
                        width: Fill, height: Fill
                        flow: Down

                        desktop_ui := mod.widgets.MainDesktopUI {}
                    }

                    contacts_page := MobileHub {kind: 0}
                    discover_page := MobileHub {kind: 1}
                    me_page := MobileHub {kind: 2}

                    settings_page := RoundedView {
                        width: Fill, height: Fill
                        // This weird margin is just to make it line up with the home_page content.
                        margin: Inset{top: 3, left: 1, right: 0, bottom: 0}
                        show_bg: true,
                        draw_bg +: {
                            color: (COLOR_PRIMARY)
                            border_radius: 4.0
                        }

                        CachedWidget {
                            settings_screen := mod.widgets.SettingsScreen {}
                        }
                    }

                    add_room_page := RoundedView {
                        width: Fill, height: Fill
                        // This weird margin is just to make it line up with the home_page content.
                        margin: Inset{top: 3, left: 1, right: 0, bottom: 0}
                        show_bg: true,
                        draw_bg +: {
                            color: (COLOR_PRIMARY)
                            border_radius: 4.0
                        }

                        CachedWidget {
                            add_room_screen := mod.widgets.AddRoomScreen {}
                        }
                    }
                }

                // Mini apps open here, beside the chat (or over it when the
                // window is too narrow for both). The same instance moves to
                // the Mobile layout, so its tabs survive a layout switch.
                CachedWidget {
                    mini_apps_side := mod.widgets.MiniAppsSideRegion {}
                }
            }

            Mobile := SolidView {
                width: Fill, height: Fill
                // Overlay: the mini-apps region covers the screen when shown.
                flow: Overlay

                show_bg: true
                draw_bg.color: (COLOR_PRIMARY)

                view_stack := StackNavigation {
                    root_view +: {
                        flow: Down
                        width: Fill, height: Fill

                        // At the top of the root view, we use the PageFlip widget to show either
                        // the main list of rooms or the settings screen.
                        home_screen_page_flip := PageFlip {
                            width: Fill, height: Fill
                            padding: Inset{
                                left: (mod.widgets.SAFE_INSET_PAD_LEFT),
                                right: (mod.widgets.SAFE_INSET_PAD_RIGHT),
                            }

                            lazy_init: true,
                            active_page: @home_page

                            home_page := View {
                                width: Fill, height: Fill
                                // Note: while the other page views have top padding, we do NOT add that here
                                // because it is added in the `RoomsSideBar`'s `RoundedShadowView` itself.
                                flow: Down

                                mod.widgets.RoomsSideBar {}
                            }

                            contacts_page := MobileHub {kind: 0}
                            discover_page := MobileHub {kind: 1}
                            me_page := MobileHub {kind: 2}

                            settings_page := View {
                                width: Fill, height: Fill

                                CachedWidget {
                                    settings_screen := mod.widgets.SettingsScreen {}
                                }
                            }

                            add_room_page := View {
                                width: Fill, height: Fill

                                CachedWidget {
                                    add_room_screen := mod.widgets.AddRoomScreen {}
                                }
                            }
                        }

                        // Keep the legacy rail's sync consumer cached across adaptive
                        // layouts. Mobile membership navigation now lives inside Chats.
                        mobile_spaces_navigation := View {
                            width: Fill height: Fit visible: false
                            CachedWidget {
                                spaces_bar_wrapper := mod.widgets.SpacesBarWrapper {}
                            }
                        }

                        mobile_navigation := View {
                            width: Fill height: Fit
                            CachedWidget {navigation_tab_bar := mod.widgets.NavigationTabBar {}}
                        }
                    }

                    stack_templates: {
                        RoomScreenStackNavigationView := mod.widgets.RobrixStackNavigationView {
                            header +: {content +: {
                                title_container +: {padding: Inset{left: 62 right: 54}}
                                info_controls := View {
                                    width: Fill height: 45 align: Align{x: 1 y: 0.5}
                                    chat_info_button := RinxChatInfoButton {}
                                }
                            }}
                            body +: {
                                room_screen := mod.widgets.RoomScreen {
                                    room_screen_wrapper +: {
                                        draw_bg.color: mod.widgets.RINX_PAGE
                                        room_columns +: {
                                            chat_center +: {
                                                timeline_and_input_bar +: {
                                                    room_input_bar := mod.widgets.MobileRoomInputBar {}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        ChatInfoStackNavigationView := mod.widgets.RobrixStackNavigationView {
                            body +: {chat_info := mod.widgets.MobileChatInfo {}}
                        }

                        InviteScreenStackNavigationView := mod.widgets.RobrixStackNavigationView {
                            body +: {
                                invite_screen := mod.widgets.InviteScreen {}
                            }
                        }

                        SpaceLobbyScreenStackNavigationView := mod.widgets.RobrixStackNavigationView {
                            body +: {
                                space_lobby_screen := mod.widgets.SpaceLobbyScreen {}
                            }
                        }
                    }
                }

                CachedWidget {
                    mini_apps_side := mod.widgets.MiniAppsSideRegion {}
                }
            }
        }
    }
}


/// A simple wrapper around the SpacesBar that allows us to animate showing or hiding it.
#[derive(Script, Widget, Animator)]
pub struct SpacesBarWrapper {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[apply_default] animator: Animator,
}

impl ScriptHook for SpacesBarWrapper {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        scope: &mut Scope,
        _value: ScriptValue,
    ) {
        // When the widget tree is re-applied (e.g. after a preference change),
        // the deref `view` resets its height to the DSL default,
        // which clashes with whatever animator state we were in (shown, hidden).
        // Thus, we re-apply the current animator state to prevent a hidden SpacesBar
        // from briefly becoming shown before being hidden again.
        // Note that we can't just call `animator_cut` cuz that uses the script VM
        // which is unavailable from this `on_after_apply`
        if !apply.is_script_reapply() {
            return;
        }
        if let Some(state_apply) = self
            .animator
            .current_state_apply(live_id!(spaces_bar_animator))
        {
            self.script_apply(vm, &Apply::Animate, scope, state_apply.into());
        }
    }
}

impl Widget for SpacesBarWrapper {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.animator_handle_event(cx, event).must_redraw() {
            self.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // don't draw anything if we're hidden (and also not animating in)
        if self.animator_in_state(cx, ids!(spaces_bar_animator.hide))
            && !self.animator.is_track_animating(live_id!(spaces_bar_animator))
        {
            return DrawStep::done();
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SpacesBarWrapperRef {
    /// Shows or hides the spaces bar by animating it in or out.
    fn show_or_hide(&self, cx: &mut Cx, show: bool) {
        let Some(mut inner) = self.borrow_mut() else { return };
        if show {
            inner.animator_play(cx, ids!(spaces_bar_animator.show));
        } else {
            inner.animator_play(cx, ids!(spaces_bar_animator.hide));
        }
        if show && inner.area().is_empty() {
            // We don't draw anything while the spaces bar is hidden.
            // Thus, before we show anything for the first time, we have to redraw
            // the whole spaces bar and everything within it.
            cx.redraw_all();
        } else {
            inner.redraw(cx);
        }
    }
}


/// The variant that the main `AdaptiveView` last selected,
/// or `None` if it hasn't made its first selection yet.
///
/// Don't query this directly, instead call [`effective_is_desktop()`].
///
/// The inner value should only be modified by [`HomeScreen::apply_view_mode()`].
#[derive(Default)]
pub struct MainViewIsDesktop(Option<bool>);

/// An action emitted when the main `AdaptiveView` switches between Desktop and Mobile.
#[derive(Debug)]
pub struct MainViewVariantChangedAction;

/// Returns whether the UI is currently showing the wide "desktop" layout.
pub fn effective_is_desktop(cx: &mut Cx) -> bool {
    cx.global::<MainViewIsDesktop>().0
        .unwrap_or(true) // Before the first selection, default to desktop mode
}


#[derive(Script, Widget)]
pub struct HomeScreen {
    #[rust] pending_matrix_link: Option<crate::matrix_links::Navigation>,
    #[rust] back_swipe: BackSwipe,
    #[rust] mobile_detail_open: bool,
    /// Chat Info temporarily covers the selected room, whose state is saved normally.
    #[rust] mobile_chat_info: Option<LiveId>,
    #[deref] view: View,

    /// The previously-selected navigation tab, used to determine which tab
    /// and top-level view we return to after closing the settings screen.
    ///
    /// Note that the current selected tap is stored in `AppState` so that
    /// other widgets can easily access it.
    #[rust] previous_selection: SelectedTab,
    #[rust] explore_return_tab: SelectedTab,
    #[rust] pending_new_chat: bool,
    #[rust] is_spaces_bar_shown: bool,

    /// A history of previously-selected screens for mobile stack navigation.
    /// When a view is popped off the stack, the previous `selected_room` is restored.
    #[rust] mobile_screen_history: Vec<SelectedRoom>,

    /// The most recently applied view-mode override, used to short-circuit
    /// redundant `AdaptiveView` selector reinstalls when an
    /// [`AppPreferencesAction::ViewModeChanged`] action repeats the current
    /// value (e.g., the unconditional broadcast on app-state restore).
    #[rust] applied_view_mode: ViewModeOverride,

    /// The last effective AdaptiveView mode we observed. `Some(true)` means desktop mode.
    #[rust] last_effective_is_desktop: Option<bool>,

    /// The frame (redraw id) the mini-apps region was last laid out for.
    #[rust] mini_apps_side_redraw: u64,
}

impl ScriptHook for HomeScreen {
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        self.reapply_view_mode(vm);
    }

    fn on_after_reload(&mut self, vm: &mut ScriptVm) {
        self.reapply_view_mode(vm);
    }
}

impl Widget for HomeScreen {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::MouseDown(mouse) = event && mouse.button == MouseButton::PRIMARY {
            self.clear_stale_splitter_hover(cx, mouse, scope);
        }
        if let Event::Scroll(scroll) = event
            && cx.is_scrolling_allowed_within(&self.view.area())
        {
            let back = self.back_swipe.update(scroll);
            let explore = scope.data.get::<AppState>().is_some_and(|app| app.selected_tab == SelectedTab::AddRoom);
            if back && (explore || !effective_is_desktop(cx)) && self.view.area().rect(cx).contains(scroll.abs) {
                self.handle_event(cx, &Event::BackPressed { handled: std::cell::Cell::new(false) }, scope);
                return;
            }
        }
        if let Event::Actions(actions) = event {
            let app_state = scope.data.get_mut::<AppState>().unwrap();
            if !effective_is_desktop(cx) {
                let stack = self.view.stack_navigation(cx, ids!(view_stack));
                if !stack.is_transitioning() && self.mobile_chat_info.is_none() {
                    if let Some(parent) = stack.current_view() {
                        let view = stack.view_by_id(cx, parent);
                        if view.button(cx, ids!(chat_info_button)).clicked(actions) {
                            if let Some(SelectedRoom::JoinedRoom {room_name_id} | SelectedRoom::Thread {room_name_id, ..}) = app_state.selected_room.as_ref() {
                                if let Some((info_id, info)) = stack.create_view_from_template(cx, id!(ChatInfoStackNavigationView)) {
                                    info.mobile_chat_info(cx, ids!(chat_info)).show(cx, room_name_id.clone());
                                    stack.set_title(cx, info_id, crate::i18n::tr("Chat Info"));
                                    self.mobile_chat_info = Some(info_id);
                                    stack.push(cx, info_id);
                                }
                            }
                        }
                    }
                }
            }
            for action in actions {
                if let Some(crate::matrix_links::LinkAction::EnterHome(navigation)) = action.downcast_ref() {
                    if navigation.is_current() {
                        self.switch_to_tab(cx, app_state, SelectedTab::Home);
                        self.view.mini_apps_side_region(cx, ids!(mini_apps_side)).reveal_chat(cx);
                        self.pending_matrix_link = Some(navigation.clone());
                        self.view.redraw(cx);
                    }
                }
                if let Some(crate::matrix_links::LinkAction::Navigate(navigation)) = action.downcast_ref()
                    && navigation.is_current() && !effective_is_desktop(cx) {
                    if app_state.selected_room.as_ref() != Some(&navigation.selected) {
                        self.push_selected_screen_view(cx, app_state, navigation.selected.clone());
                    }
                    let stack = self.view.stack_navigation(cx, ids!(view_stack));
                    if let Some(view) = stack.destination_view().or_else(|| stack.current_view()) {
                        let screen = stack.view_by_id(cx, view).room_screen(cx, ids!(room_screen));
                        if let Some(event) = &navigation.event {
                            screen.jump_to_matrix_event(cx, event.clone(), navigation.ticket.clone());
                        }
                    }
                }
                if effective_is_desktop(cx) && (
                    matches!(action.downcast_ref::<crate::home::main_desktop_ui::MainDesktopUiAction>(), Some(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats))
                    || matches!(action.downcast_ref::<RoomsListAction>(), Some(RoomsListAction::Selected(_)))
                ) {
                    self.switch_to_tab(cx, app_state, SelectedTab::Home);
                }
                if let Some(super::room_history::RoomHistoryAction::Jump {room, event}) = action.downcast_ref() {
                    if !effective_is_desktop(cx) {
                        if self.mobile_chat_info.is_some() {
                            // Room search covers the main room, including when opened from a thread.
                            if matches!(app_state.selected_room.as_ref(), Some(SelectedRoom::Thread {..})) {
                                if let Some(previous) = app_state.selected_room.take() {previous.close_thread_timeline(cx);}
                                app_state.selected_room = Some(SelectedRoom::JoinedRoom {room_name_id: room.clone()});
                            }
                            self.pop_selected_screen_view(cx, app_state);
                        } else if !matches!(app_state.selected_room.as_ref(), Some(SelectedRoom::JoinedRoom {room_name_id}) if room_name_id.room_id() == room.room_id()) {
                            self.push_selected_screen_view(cx, app_state, SelectedRoom::JoinedRoom {room_name_id: room.clone()});
                        }
                        let stack = self.view.stack_navigation(cx, ids!(view_stack));
                        if let Some(view_id) = stack.destination_view().or_else(|| stack.current_view()) {
                            stack.view_by_id(cx, view_id).room_screen(cx, ids!(room_screen)).jump_to_history_event(cx, event.clone());
                        }
                    }
                }
                if matches!(action.downcast_ref(), Some(NavigationBarAction::GoToTab(_) | NavigationBarAction::GoToHome | NavigationBarAction::GoToAddRoom | NavigationBarAction::GoToSpace { .. } | NavigationBarAction::OpenSettings | NavigationBarAction::OpenOwnProfile)) || matches!(action.downcast_ref(), Some(crate::logout::logout_confirm_modal::LogoutAction::ClearAppState { .. })) {
                    self.pending_new_chat = false;
                }
                match action.downcast_ref() {
                    Some(NavigationBarAction::GoToTab(tab)) => {
                        self.switch_to_tab(cx, app_state, tab.clone());
                    }
                    Some(NavigationBarAction::NewChat) => {
                        if app_state.selected_tab == SelectedTab::Contacts {
                            cx.action(super::mobile::MobileNavigationAction::NewChatRequested);
                        } else {
                            self.pending_new_chat = true;
                            self.switch_to_tab(cx, app_state, SelectedTab::Contacts);
                        }
                    }
                    Some(NavigationBarAction::GoToHome) => {
                        self.switch_to_tab(cx, app_state, SelectedTab::Home);
                    }
                    Some(NavigationBarAction::GoToAddRoom) => {
                        if app_state.selected_tab != SelectedTab::AddRoom {
                            self.explore_return_tab = if app_state.selected_tab == SelectedTab::Settings {
                                self.previous_selection.clone()
                            } else { app_state.selected_tab.clone() };
                        }
                        self.switch_to_tab(cx, app_state, SelectedTab::AddRoom);
                    }
                    Some(NavigationBarAction::GoToSpace { space_name_id }) => {
                        self.switch_to_tab(cx, app_state, SelectedTab::Space { space_name_id: space_name_id.clone() });
                    }
                    // Only open the settings screen if it is not currently open.
                    Some(NavigationBarAction::OpenSettings | NavigationBarAction::OpenOwnProfile) => {
                        if !matches!(app_state.selected_tab, SelectedTab::Settings) {
                            self.previous_selection = std::mem::replace(&mut app_state.selected_tab, SelectedTab::Settings);
                            cx.action(NavigationBarAction::TabSelected(app_state.selected_tab.clone()));
                            if let Some(settings_page) = self.update_active_page_from_selection(cx, app_state) {
                                settings_page
                                    .settings_screen(cx, ids!(settings_screen))
                                    .populate(cx, None, app_state);
                                if matches!(action.downcast_ref(), Some(NavigationBarAction::OpenOwnProfile)) {
                                    settings_page.settings_screen(cx, ids!(settings_screen)).open_personal_info(cx);
                                }
                                self.view.redraw(cx);
                            } else {
                                error!("BUG: failed to set active page to show settings screen.");
                            }
                        }
                    }
                    Some(NavigationBarAction::CloseSettings | NavigationBarAction::CloseAddRoom) => {
                        let expected = match action.downcast_ref() {
                            Some(NavigationBarAction::CloseAddRoom) => SelectedTab::AddRoom,
                            _ => SelectedTab::Settings,
                        };
                        if app_state.selected_tab == expected {
                            app_state.selected_tab = if expected == SelectedTab::AddRoom {
                                self.explore_return_tab.clone()
                            } else { self.previous_selection.clone() };
                            cx.action(NavigationBarAction::TabSelected(app_state.selected_tab.clone()));
                            if let Some(page) = self.update_active_page_from_selection(cx, app_state) {
                                page.handle_event(cx, &Event::Signal, &mut Scope::with_data(app_state));
                                page.redraw(cx);
                            }
                            self.view.redraw(cx);
                        }
                    }
                    Some(NavigationBarAction::ToggleSpacesBar) => {
                        self.is_spaces_bar_shown = !self.is_spaces_bar_shown;
                        self.view.spaces_bar_wrapper(cx, ids!(spaces_bar_wrapper))
                            .show_or_hide(cx, self.is_spaces_bar_shown);
                    }
                    // We're the ones who emitted this action, so we don't need to handle it again.
                    Some(NavigationBarAction::TabSelected(SelectedTab::Contacts)) if self.pending_new_chat => {
                        self.pending_new_chat = false;
                        cx.action(super::mobile::MobileNavigationAction::NewChatRequested);
                    }
                    Some(NavigationBarAction::TabSelected(_))
                    | None => { }
                }

                if let Some(super::mobile::MobileNavigationAction::DetailVisibility(open)) = action.downcast_ref() {
                    self.mobile_detail_open = *open;
                    self.view.redraw(cx);
                }

                // React to App Settings changes that affect the HomeScreen layout.
                if let Some(AppPreferencesAction::ViewModeChanged(new_mode)) = action.downcast_ref() {
                    if *new_mode != self.applied_view_mode {
                        self.apply_view_mode(cx, *new_mode);
                        // Set & broadcast the new variant now so that the mobile cleanup in
                        // `sync_effective_view_mode()` can run before the dock reloads.
                        let parent_size = self.view.area().rect(cx).size;
                        if parent_size.x > 0.0
                            || !matches!(new_mode, ViewModeOverride::Automatic)
                            || cx.display_context.is_screen_size_known()
                        {
                            let variant = (new_mode.variant_selector())(cx, &parent_size);
                            cx.global::<MainViewIsDesktop>().0 = Some(variant == live_id!(Desktop));
                        }
                        self.view.redraw(cx);
                    }
                }

                // An invited space's InviteScreen should be shown on the main home screen's dock,
                // so navigate to that first (un-select any selected space).
                //
                // This is to ensure that we don't show a new space invite within an existing
                // unrelated space's separate dock / rooms list.
                if let SpacesBarAction::InvitedSpaceClicked { space_name_id } = action.as_widget_action().cast()
                    && !effective_is_desktop(cx)
                {
                    self.switch_to_tab(cx, app_state, SelectedTab::Home);
                    self.push_selected_screen_view(
                        cx,
                        app_state,
                        SelectedRoom::InvitedRoom { room_name_id: space_name_id },
                    );
                    continue;
                }

                // Handle room selections. Desktop owns tab creation in MainDesktopUI,
                // while mobile owns StackNavigation screen pushes here.
                match action.as_widget_action().cast() {
                    RoomsListAction::Selected(selected_room) if !effective_is_desktop(cx) => {
                        self.push_selected_screen_view(cx, app_state, selected_room);
                    }
                    // On desktop, `MainDesktopUI` handles this, so we only need to update this in mobile view mode.
                    RoomsListAction::InviteAccepted { room_name_id, kind } if !effective_is_desktop(cx) => {
                        let is_space = kind.is_space();
                        self.upgrade_mobile_invite_to_joined(cx, app_state, &room_name_id, kind);
                        cx.action(AppStateAction::UpgradedInviteToJoinedRoom {
                            room_id: room_name_id.room_id().clone(),
                            is_space,
                        });
                    }
                    _ => {}
                }

                if let StackNavigationTransitionAction::ViewReleased(view_id) =
                    action.as_widget_action().cast()
                {
                    let stack_navigation = self.view.stack_navigation(cx, ids!(view_stack));
                    self.hide_screen_in_released_stack_view(cx, &stack_navigation, view_id);
                }

                // When a stack navigation pop is requested (back button pressed),
                // reveal the previous screen from HomeScreen's mobile history.
                if let StackNavigationAction::Pop = action.as_widget_action().cast() {
                    self.pop_selected_screen_view(cx, app_state);
                }

                if let Some(
                    AppStateAction::RoomNameUpdated(new_room_name)
                    | AppStateAction::RoomLoadedSuccessfully { room_name_id: new_room_name, .. }
                ) = action.downcast_ref() {
                    for room in &mut self.mobile_screen_history {
                        room.update_room_name(new_room_name);
                    }
                    self.previous_selection.update_space_name(new_room_name);
                    self.explore_return_tab.update_space_name(new_room_name);
                    let stack_navigation = self.view.stack_navigation(cx, ids!(view_stack));
                    if let Some(view_id) = stack_navigation.destination_view()
                        && let Some(room) = app_state.selected_room.as_ref()
                        && room.room_id() == new_room_name.room_id()
                    {
                        if self.mobile_chat_info != Some(view_id) {
                            stack_navigation.set_title(cx, view_id, &room.display_name());
                        }
                    }
                }
            }
        }

        // Consume a contact-page Back before the StackNavigation widget sees
        // its generated Pop on the next event turn (it otherwise pops to root).
        // Keep every unrelated widget/backend action in the same batch.
        let mut generated = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        generated.retain(|action| {
            !(matches!(action.as_widget_action().cast(), StackNavigationAction::Pop)
                && self.dismiss_mobile_profile(cx))
        });
        cx.extend_actions(generated);

        // Now that we've forwarded the event (above) to our children, the AdaptiveView instance
        // has properly updated its view mode, so we can now query and sync it across robrix.
        if let Event::Actions(_) = event {
            let app_state = scope.data.get_mut::<AppState>().unwrap();
            self.sync_effective_view_mode(cx, app_state);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let app_state = scope.data.get_mut::<AppState>().unwrap();
        // Note: We need to update the active page before drawing,
        // because if we switched between Desktop and Mobile views,
        // the PageFlip widget will have been reset to its default,
        // so we must re-set it to the correct page based on `app_state.selected_tab`.
        self.update_active_page_from_selection(cx, app_state);
        let show_tabs = matches!(app_state.selected_tab, SelectedTab::Home | SelectedTab::Contacts | SelectedTab::Discover | SelectedTab::Me | SelectedTab::Space {..})
            && !(app_state.selected_tab == SelectedTab::Contacts && self.mobile_detail_open);
        self.view.view(cx, ids!(mobile_navigation)).set_visible(cx, show_tabs);

        // Contact details share the stack's single mobile header. Query the
        // current view rather than retaining any released room widget.
        if !effective_is_desktop(cx) && self.mobile_chat_info.is_none()
            && matches!(app_state.selected_room, Some(SelectedRoom::JoinedRoom {..} | SelectedRoom::Thread {..}))
        {
            let stack = self.view.stack_navigation(cx, ids!(view_stack));
            if let Some(view_id) = stack.current_view() {
                let view = stack.view_by_id(cx, view_id);
                let profile_open = view.user_profile_sliding_pane(cx, ids!(user_profile_sliding_pane))
                    .is_currently_shown(cx);
                let title = if profile_open { crate::i18n::tr("Contact Info").to_owned() }
                    else { app_state.selected_room.as_ref().unwrap().display_name() };
                stack.set_title(cx, view_id, &title);
                view.view(cx, ids!(info_controls)).set_visible(cx, !profile_open);
            }
        }

        // Lay out the mini-apps region once per frame: a draw resumed after a
        // step runs in another turtle and must not measure again.
        if self.mini_apps_side_redraw != cx.redraw_id {
            self.mini_apps_side_redraw = cx.redraw_id;
            let saved_width = app_state.mini_apps_width;
            self.prepare_mini_apps_side(cx, walk, saved_width);
        }

        let result = self.view.draw_walk(cx, scope, walk);
        if result.is_done() && let Some(navigation) = self.pending_matrix_link.take()
            && navigation.is_current() {
            cx.action(crate::matrix_links::LinkAction::Navigate(navigation));
        }
        result
    }
}

impl HomeScreen {
    /// Docks the mini-apps region beside the chat when the rooms list, the
    /// chat's minimum and the region's minimum all fit; otherwise it shows
    /// over the page (desktop) or over everything (mobile).
    fn prepare_mini_apps_side(&mut self, cx: &mut Cx2d, walk: Walk, saved_width: Option<f64>) {
        use crate::miniapps::side::{MiniAppsSideRegionWidgetExt, ROOMS_DEFAULT};
        let side = self.view.mini_apps_side_region(cx, ids!(mini_apps_side));
        if side.borrow().is_none() {
            return;
        }
        let size = cx.peek_walk_turtle(walk).size;
        // The variant this frame will draw, decided before the AdaptiveView draws it.
        let desktop = (self.applied_view_mode.variant_selector())(cx, &size) == live_id!(Desktop);
        let [rail, inset] = cx.with_vm(|vm| {
            let widgets = vm.module(id!(widgets));
            ["NAVIGATION_TAB_BAR_SIZE", "SAFE_INSET_PAD_LEFT"].map(|name| {
                vm.bx.heap.value(widgets, LiveId::from_str(name).into(), NoTrap).as_number().unwrap_or(0.0)
            })
        });
        let rooms = self.view.dock(cx, ids!(dock)).splitter_position(id!(root))
            .unwrap_or(ROOMS_DEFAULT);
        let covers = side.prepare(cx, size.x - rail - inset, rooms, desktop, saved_width);
        // Only the desktop page beside the left bar gives way to an overlay;
        // an unswitched Mobile variant this frame is never touched.
        let page = self.view.widget(cx, ids!(Desktop.home_screen_page_flip));
        if !page.is_empty() {
            page.set_visible(cx, !covers);
        } else if desktop {
            // The Desktop layout is created by this frame's draw: lay out again next frame.
            self.view.redraw(cx);
        }
    }
}

impl HomeScreen {
    /// A native webview or overlay can consume the move between two splitters.
    /// Deliver the missing hover-out before the next splitter press, otherwise
    /// the old grip can remain highlighted after hover ownership changes.
    fn clear_stale_splitter_hover(&self, cx: &mut Cx, mouse: &MouseDownEvent, scope: &mut Scope) {
        let handle = self.view.widget(cx, ids!(mini_apps_side.handle));
        let handle_rect = handle.area().rect(cx);
        if !handle.visible() || handle_rect.size.x <= 0.0 || handle_rect.size.y <= 0.0 {
            return;
        }
        let margin = crate::shared::pane_splitter::HIT_MARGIN;
        let on_side = mouse.abs.y >= handle_rect.pos.y
            && mouse.abs.y <= handle_rect.pos.y + handle_rect.size.y
            && mouse.abs.x >= handle_rect.pos.x - margin
            && mouse.abs.x <= handle_rect.pos.x + handle_rect.size.x + margin;
        let dock = self.view.dock(cx, ids!(dock));
        let dock_rect = self.view.widget(cx, ids!(dock)).area().rect(cx);
        let on_rooms = dock.splitter_position(id!(root)).is_some_and(|position| {
            let x = dock_rect.pos.x + position;
            mouse.abs.y >= dock_rect.pos.y && mouse.abs.y <= dock_rect.pos.y + dock_rect.size.y
                && mouse.abs.x >= x - margin
                && mouse.abs.x <= x + crate::shared::pane_splitter::WIDTH + margin
        });
        if !on_side && !on_rooms {
            return;
        }
        let leave = Event::MouseLeave(makepad_widgets::makepad_platform::event::MouseLeaveEvent {
            abs: mouse.abs,
            window_id: mouse.window_id,
            modifiers: mouse.modifiers,
            time: mouse.time,
            handled: std::cell::Cell::new(Area::Empty),
        });
        if let Some(mut inner) = dock.borrow_mut() {
            inner.handle_event_with_bodies(cx, &leave, scope, makepad_widgets::dock::BodyDispatch::Skip);
        }
        handle.handle_event(cx, &leave, scope);
    }

    fn dismiss_mobile_profile(&mut self, cx: &mut Cx) -> bool {
        if effective_is_desktop(cx) || self.mobile_chat_info.is_some() { return false; }
        let stack = self.view.stack_navigation(cx, ids!(view_stack));
        let Some(view_id) = stack.current_view() else { return false; };
        let view = stack.view_by_id(cx, view_id);
        let profile = view.user_profile_sliding_pane(cx, ids!(user_profile_sliding_pane));
        if !profile.is_currently_shown(cx) { return false; }
        profile.dismiss(cx);
        self.view.redraw(cx);
        true
    }
    /// Installs a variant selector on the main `AdaptiveView` that honors the
    /// current [`ViewModeOverride`] preference, and publishes each choice so
    /// that `effective_is_desktop()` always matches that same view mode.
    fn apply_view_mode(&mut self, cx: &mut Cx, mode: ViewModeOverride) {
        let mut select_variant = mode.variant_selector();
        self.view
            .adaptive_view(cx, ids!(main_adaptive_view))
            .set_variant_selector(move |cx, parent_size| {
                let variant = select_variant(cx, parent_size);
                let is_desktop = variant == live_id!(Desktop);
                if cx.global::<MainViewIsDesktop>().0.replace(is_desktop) != Some(is_desktop) {
                    cx.action(MainViewVariantChangedAction);
                }
                variant
            });
        self.applied_view_mode = mode;
    }

    /// Reinstalls the AdaptiveView variant selector based on the current user preference.
    fn reapply_view_mode(&mut self, vm: &mut ScriptVm) {
        vm.with_cx_mut(|cx| {
            let mode = cx.global::<AppPreferencesGlobal>().0.view_mode;
            self.apply_view_mode(cx, mode);
        });
    }

    fn sync_effective_view_mode(&mut self, cx: &mut Cx, app_state: &mut AppState) {
        // Do nothing until the AdaptiveView instance has actually selected a variant
        let Some(is_desktop) = cx.global::<MainViewIsDesktop>().0 else { return };
        let Some(was_desktop) = self.last_effective_is_desktop.replace(is_desktop) else {
            return;
        };
        if was_desktop == is_desktop {
            return;
        }

        // If we transitioned from mobile --> desktop view mode, the dock will reload the tabs
        // from its previously-saved state, so we need to free the current selected room now
        // (if it was a thread timeline), and then also clear any thread timelines in the mobile nav stack.
        if !was_desktop && is_desktop {
            if let Some(room) = app_state.selected_room.as_ref() {
                room.close_thread_timeline(cx);
            }
        }

        // If the mentionable popup was shown, close it because the whole UI has changed/moved.
        if cx.has_global::<MentionablePopupRef>() {
            cx.get_global::<MentionablePopupRef>().clone().cancel(cx);
        }

        self.clear_mobile_navigation_state(cx);

        // Switching into mobile mode lands on the rooms list, so no room should
        // be drawn as selected until one is actually clicked.
        if !is_desktop {
            cx.action(AppStateAction::FocusNone);
        }
    }

    fn update_active_page_from_selection(
        &mut self,
        cx: &mut Cx,
        app_state: &mut AppState,
    ) -> Option<WidgetRef> {
        self.view
            .page_flip(cx, ids!(home_screen_page_flip))
            .set_active_page(
                cx,
                match app_state.selected_tab {
                    SelectedTab::Space { .. }
                    | SelectedTab::Home => id!(home_page),
                    SelectedTab::Contacts => id!(contacts_page),
                    SelectedTab::Discover => id!(discover_page),
                    SelectedTab::Me => id!(me_page),
                    SelectedTab::Settings => id!(settings_page),
                    SelectedTab::AddRoom => id!(add_room_page),
                },
            )
    }

    /// Populates a `StackNavigationView` with the given room/screen's info.
    ///
    /// Returns the LiveId of the view that should be pushed onto or revealed by
    /// the stack navigation widget.
    fn populate_mobile_stack_view(
        &mut self,
        cx: &mut Cx,
        stack_navigation: &StackNavigationRef,
        selected_screen: &SelectedRoom,
    ) -> Option<LiveId> {
        let view_id = match selected_screen {
            SelectedRoom::JoinedRoom { room_name_id }
            | SelectedRoom::Thread { room_name_id, .. } => {
                let Some((view_id, stack_navigation_view)) =
                    stack_navigation.create_view_from_template(cx, id!(RoomScreenStackNavigationView))
                else {
                    error!("BUG: failed to create mobile RoomScreen StackNavigationView");
                    return None;
                };
                Self::hide_displayed_stack_screen(cx, &stack_navigation_view);
                let thread_root = if let SelectedRoom::Thread { thread_root_event_id, .. } = selected_screen {
                    Some(thread_root_event_id.clone())
                } else {
                    None
                };
                stack_navigation_view
                    .room_screen(cx, ids!(room_screen))
                    .set_displayed_room(cx, room_name_id, thread_root);
                view_id
            }
            SelectedRoom::InvitedRoom { room_name_id } => {
                let Some((view_id, stack_navigation_view)) =
                    stack_navigation.create_view_from_template(cx, id!(InviteScreenStackNavigationView))
                else {
                    error!("BUG: failed to create mobile InviteScreen StackNavigationView");
                    return None;
                };
                Self::hide_displayed_stack_screen(cx, &stack_navigation_view);
                stack_navigation_view
                    .invite_screen(cx, ids!(invite_screen))
                    .set_displayed_invite(cx, room_name_id);
                view_id
            }
            SelectedRoom::Space { space_name_id } => {
                let Some((view_id, stack_navigation_view)) =
                    stack_navigation.create_view_from_template(cx, id!(SpaceLobbyScreenStackNavigationView))
                else {
                    error!("BUG: failed to create mobile SpaceLobbyScreen StackNavigationView");
                    return None;
                };
                Self::hide_displayed_stack_screen(cx, &stack_navigation_view);
                stack_navigation_view
                    .space_lobby_screen(cx, ids!(space_lobby_screen))
                    .set_displayed_space(cx, space_name_id);
                view_id
            }
        };

        stack_navigation.set_title(cx, view_id, &selected_screen.display_name());
        Some(view_id)
    }

    /// Hides the screen within a stack view that was released by the StackNavigation widget.
    fn hide_screen_in_released_stack_view(
        &mut self,
        cx: &mut Cx,
        stack_navigation: &StackNavigationRef,
        view_id: LiveId,
    ) {
        if stack_navigation.stack_view_ids().contains(&view_id) {
            return;
        }
        let stack_navigation_view = stack_navigation.view_by_id(cx, view_id);
        Self::hide_displayed_stack_screen(cx, &stack_navigation_view);
    }

    fn clear_mobile_navigation_state(&mut self, cx: &mut Cx) {
        self.mobile_chat_info = None;
        // When switching from mobile --> desktop view mode, we discard the nav stack,
        // and thus we need to free & destroy any thread timelines in it.
        // Note that freeing the current room is handled in `sync_effective_view_mode`.
        for room in &self.mobile_screen_history {
            room.close_thread_timeline(cx);
        }
        self.mobile_screen_history.clear();

        let stack_navigation = self.view.stack_navigation(cx, ids!(view_stack));
        for view_id in stack_navigation.dynamic_stack_view_ids() {
            let stack_navigation_view = stack_navigation.view_by_id(cx, view_id);
            Self::hide_displayed_stack_screen(cx, &stack_navigation_view);
        }
        // Also go back to the root stack view to ensure no old roomscreens persist.
        stack_navigation.pop_to_root(cx);
    }

    fn hide_displayed_stack_screen(cx: &mut Cx, stack_navigation_view: &WidgetRef) {
        stack_navigation_view.mobile_chat_info(cx, ids!(chat_info)).clear();
        stack_navigation_view
            .room_screen(cx, ids!(room_screen))
            .hide_displayed_room(cx);
        stack_navigation_view
            .invite_screen(cx, ids!(invite_screen))
            .hide_displayed_invite(cx);
        stack_navigation_view
            .space_lobby_screen(cx, ids!(space_lobby_screen))
            .hide_displayed_space(cx);
    }

    /// Pushes the given screen onto the mobile screen history and animates it in.
    fn push_selected_screen_view(
        &mut self,
        cx: &mut Cx,
        app_state: &mut AppState,
        sr: SelectedRoom,
    ) {
        let stack_navigation = self.view.stack_navigation(cx, ids!(view_stack));
        if stack_navigation.is_transitioning() {
            return;
        }
        let has_current_mobile_screen = stack_navigation.current_view().is_some();
        // If it has the same room ID and the same screen type (invite, joined, etc),
        // then we actually don't need to do anything. Otherwise we need to change it
        // to a new screen, e.g., a joined RoomScreen or a joined SpaceLobbyScreen.
        let is_same_screen = app_state.selected_room.as_ref().is_some_and(|c|
            c == &sr && std::mem::discriminant(c) == std::mem::discriminant(&sr)
        );
        if has_current_mobile_screen && is_same_screen {
            return;
        }
        let Some(view_id) = self.populate_mobile_stack_view(cx, &stack_navigation, &sr) else {
            return;
        };

        // Save the current selected_room onto the navigation stack before replacing it.
        if has_current_mobile_screen {
            if let Some(prev) = app_state.selected_room.take() {
                self.mobile_screen_history.push(prev);
            }
        }
        app_state.selected_room = Some(sr);
        stack_navigation.push(cx, view_id);
        self.view.redraw(cx);
    }

    /// Switches to (selects) the given navigation tab, if it isn't already the selected one.
    fn switch_to_tab(&mut self, cx: &mut Cx, app_state: &mut AppState, new_tab: SelectedTab) {
        if app_state.selected_tab == new_tab { return }
        self.previous_selection = std::mem::replace(&mut app_state.selected_tab, new_tab);
        cx.action(NavigationBarAction::TabSelected(app_state.selected_tab.clone()));
        self.update_active_page_from_selection(cx, app_state);
        self.view.redraw(cx);
    }

    /// Upgrades a room's or space's InviteScreen that is being shown in mobile view mode
    /// to that newly-joined RoomScreen or SpaceLobbyScreen.
    fn upgrade_mobile_invite_to_joined(
        &mut self,
        cx: &mut Cx,
        app_state: &mut AppState,
        room_name_id: &RoomNameId,
        kind: AcceptedInviteKind,
    ) {
        let is_space = kind.is_space();
        let room_id = room_name_id.room_id();
        let is_this_invite = |sr: &SelectedRoom| matches!(
            sr, SelectedRoom::InvitedRoom { room_name_id: r } if r.room_id() == room_id
        );
        if app_state.selected_room.as_ref().is_some_and(is_this_invite) {
            // The new SpaceLobbyScreen should be shown within its top-level ancestor space's tab/dock,
            // so navigate to that, which also sets the rooms list into that space's mode.
            if let AcceptedInviteKind::Space { dock_space: Some(dock_space) } = &kind {
                self.switch_to_tab(cx, app_state, SelectedTab::Space { space_name_id: dock_space.clone() });
            }
            self.push_selected_screen_view(
                cx,
                app_state,
                SelectedRoom::to_joined(room_name_id.clone(), is_space),
            );
            // The invite we replaced was pushed onto the mobile history stack,
            // so we need to remove it to ensure that it won't show up if the user goes back.
            if self.mobile_screen_history.last().is_some_and(is_this_invite) {
                self.mobile_screen_history.pop();
            }
        }
        for room in &mut self.mobile_screen_history {
            room.upgrade_invite_to_joined(room_id, is_space);
        }
    }

    /// Pops the current mobile screen, revealing the previous screen or the room list root.
    fn pop_selected_screen_view(&mut self, cx: &mut Cx, app_state: &mut AppState) {
        let stack_nav = self.view.stack_navigation(cx, ids!(view_stack));
        if stack_nav.is_transitioning() {
            return;
        }
        if self.dismiss_mobile_profile(cx) { return; }
        if self.mobile_chat_info.is_some() {
            // StackNavigation releases the covered room. Rehydrate it through
            // the regular timeline/draft restore path before revealing it.
            // Keeping a released room live retains stale cached draw areas.
            if let Some(selected) = app_state.selected_room.as_ref() {
                if let Some(view_id) = self.populate_mobile_stack_view(cx, &stack_nav, selected) {
                    self.mobile_chat_info = None;
                    stack_nav.pop_to_view(cx, view_id);
                    self.view.redraw(cx);
                }
            }
            return;
        }
        let Some(current_screen) = app_state.selected_room.take() else {
            // If we didn't have a current screen, something's buggy,
            // so the safest option is to clear the mobile stack and start over. nbd.
            self.mobile_screen_history.clear();
            return;
        };
        match self.mobile_screen_history.pop() {
            Some(previous) => {
                let Some(view_id) = self.populate_mobile_stack_view(cx, &stack_nav, &previous) else {
                    // Nav failed; current_screen is restored, so don't free it.
                    app_state.selected_room = Some(current_screen);
                    self.mobile_screen_history.push(previous);
                    return;
                };
                // current_screen is gone for good — free its thread timeline if it is one.
                current_screen.close_thread_timeline(cx);
                app_state.selected_room = Some(previous);
                stack_nav.pop_to_view(cx, view_id);
            }
            None => {
                current_screen.close_thread_timeline(cx);
                app_state.selected_room = None;
                stack_nav.pop_to_root(cx);
                log!("Rinx navigation: returned to chat list");
            }
        }
        self.view.redraw(cx);
    }
}

#[cfg(test)]
pub(crate) mod responsive_layout_tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    #[test]
    fn settled_hidden_spaces_bar_does_not_draw_and_first_show_restores_it() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let bar = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.SpacesBarWrapper {}});
            let bar = WidgetRef::script_from_value(vm, value);
            bar.register_as_ui_root(vm);
            bar
        });
        for shown in [false, true] {
            if shown {
                let mut inner = bar.borrow_mut::<SpacesBarWrapper>().unwrap();
                inner.animator_cut(&mut cx, ids!(spaces_bar_animator.show));
            }
            cx.redraw_all();
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(400.0, 200.0));
            let mut list = DrawList2d::new(&mut cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(dvec2(400.0, 200.0), Layout::flow_overlay());
            bar.draw_all(&mut cx2d, &mut Scope::empty());
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
            drop(cx2d); drop(draw);
            assert_eq!(bar.area().is_empty(), !shown);
            if shown { assert!(bar.area().rect(&cx).size.y > 0.0); }
        }
    }

    pub(crate) fn matrix_link_delivery(navigation: crate::matrix_links::Navigation, width: f64) {
        use crate::home::main_desktop_ui::MainDesktopUIWidgetExt;
        let (mut cx, mut home, mut state) = home();
        state.selected_tab = SelectedTab::Me;
        draw_pane(&mut cx, &mut home, &mut state, width);
        let actions = cx.capture_actions(|cx|crate::matrix_links::publish_navigation(cx, navigation.clone()));
        home.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::with_data(&mut state));
        assert_eq!(state.selected_tab, SelectedTab::Home);
        for _ in 0..3 {
            let actions = cx.capture_actions(|cx|draw_pane(cx, &mut home, &mut state, width));
            home.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::with_data(&mut state));
        }
        let screen = if effective_is_desktop(&mut cx) {
            home.view.main_desktop_ui(&cx, ids!(desktop_ui)).matrix_link_screen(&cx, &navigation)
        } else {
            let stack = home.view.stack_navigation(&cx, ids!(view_stack));
            let target = stack.destination_view().or_else(||stack.current_view()).unwrap();
            stack.view_by_id(&mut cx, target).room_screen(&cx, ids!(room_screen))
        };
        screen.assert_matrix_link_target(&cx, &navigation);
        navigation.ticket.cancel();
        screen.handle_event(&mut cx, &Event::Signal, &mut Scope::with_data(&mut state));
        screen.assert_matrix_link_cancelled();
    }

    fn home() -> (Cx, HomeScreen, AppState) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        // OctoSense's outer window stays wide while only the Rinx tile resizes.
        cx.display_context.screen_size = dvec2(1440.0, 900.0);
        let home = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, { mod.widgets.HomeScreen {} });
            let home = HomeScreen::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            home
        });
        (cx, home, AppState::default())
    }

    fn draw_pane(cx: &mut Cx, home: &mut HomeScreen, state: &mut AppState, width: f64) {
        cx.redraw_all();
        let pass = DrawPass::new(cx);
        pass.set_size(cx, dvec2(1440.0, 900.0));
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut cx2d = Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass, None);
        list.begin_always(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(1440.0, 900.0), Layout::flow_overlay());
        home.draw_walk_all(&mut cx2d, &mut Scope::with_data(state), Walk {
            width: Size::Fixed(width),
            height: Size::Fixed(760.0),
            ..Walk::default()
        });
        cx2d.end_pass_sized_turtle();
        list.end(&mut cx2d);
        cx2d.end_pass(&pass);
    }

    fn assert_layout(cx: &mut Cx, home: &HomeScreen, desktop: bool) {
        let adaptive = home.view.adaptive_view(cx, ids!(main_adaptive_view));
        assert_eq!(adaptive.borrow().unwrap().active_variant(),
            Some(if desktop { id!(Desktop) } else { id!(Mobile) }));
        assert_eq!(effective_is_desktop(cx), desktop);
        assert_eq!(crate::home::account_menu::is_desktop_layout(cx), desktop);
    }

    #[test]
    fn recovery_closing_settings_catches_up_the_covered_room() {
        let (mut cx, mut home, mut state)=home();state.selected_tab=SelectedTab::Home;
        let page=home.update_active_page_from_selection(&mut cx,&mut state).unwrap();
        let widget=cx.with_vm(|vm| {
            let value=script_eval!(vm,{mod.widgets.RoomScreen {}});WidgetRef::script_from_value(vm,value)
        });
        page.as_view().borrow_mut().unwrap().children.push((id!(recovery_probe),widget.clone()));
        let room=widget.as_room_screen();
        let sender=super::super::room_screen::crypto_recovery_resume_tests::prepare(&mut cx,&room);
        draw_pane(&mut cx,&mut home,&mut state,1280.0);
        let actions=cx.capture_actions(|cx|cx.action(NavigationBarAction::OpenSettings));
        home.handle_event(&mut cx,&Event::Actions(actions),&mut Scope::with_data(&mut state));
        assert_eq!(state.selected_tab,SelectedTab::Settings);
        super::super::room_screen::crypto_recovery_resume_tests::queue(&sender);
        let actions=cx.capture_actions(|cx|cx.action(NavigationBarAction::CloseSettings));
        home.handle_event(&mut cx,&Event::Actions(actions),&mut Scope::with_data(&mut state));
        assert_eq!(state.selected_tab,SelectedTab::Home);
        super::super::room_screen::crypto_recovery_resume_tests::assert_updated(&room);
    }

    #[test]
    fn embedded_home_follows_pane_resizes_inside_a_wide_host() {
        let (mut cx, mut home, mut state) = home();
        for (width, desktop) in [(1024.0, true), (375.0, false), (860.0, true), (859.0, false)] {
            draw_pane(&mut cx, &mut home, &mut state, width);
            assert_layout(&mut cx, &home, desktop);
            assert_eq!(cx.display_context.screen_size, dvec2(1440.0, 900.0));
            if !desktop {
                let nav = home.view.view(&mut cx, ids!(mobile_navigation)).area().rect(&cx);
                assert!(nav.size.x > 0.0 && nav.pos.x + nav.size.x <= width + 0.5,
                    "Mobile navigation must fit the Rinx pane: {nav:?}");
            }
        }
    }

    #[test]
    fn returning_to_automatic_uses_the_current_pane() {
        let (mut cx, mut home, mut state) = home();
        draw_pane(&mut cx, &mut home, &mut state, 375.0);
        for (mode, desktop) in [(ViewModeOverride::ForceWide, true), (ViewModeOverride::Automatic, false)] {
            state.app_prefs.view_mode = mode;
            let actions = cx.capture_actions(|cx| state.app_prefs.on_view_mode_changed(cx));
            home.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::with_data(&mut state));
            assert_eq!(effective_is_desktop(&mut cx), desktop,
                "Preference changes must prepare navigation for the pane's next layout");
            draw_pane(&mut cx, &mut home, &mut state, 375.0);
            assert_layout(&mut cx, &home, desktop);
        }
    }
}

/// The docked mini-apps region beside the chat, in the desktop UI drawn offscreen.
#[cfg(test)]
mod mini_apps_side_tests {
    use super::*;
    use crate::hagency::ui::HagencyAction;
    use crate::miniapps::{
        MiniAppsAction,
        side::{MiniAppsSideRegionRef, MiniAppsSideRegionWidgetExt, SideLayout, CHAT_MIN, HANDLE, SIDE_DEFAULT, SIDE_MIN},
        surface::SurfaceKey,
    };
    use crate::shared::navigation_bar_button::NavigationBarButtonAction;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    const HEIGHT: f64 = 800.0;

    fn home(width: f64) -> (Cx, HomeScreen, AppState) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.display_context.screen_size = dvec2(width, HEIGHT);
        let home = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, { mod.widgets.HomeScreen {} });
            let home = HomeScreen::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Home screen registration: {errors:?}");
            home
        });
        (cx, home, AppState::default())
    }

    /// Draws one frame and delivers what it emitted, as the event loop would.
    fn frame(cx: &mut Cx, home: &mut HomeScreen, state: &mut AppState, width: f64) {
        let emitted = cx.capture_actions(|cx| {
            cx.redraw_all();
            let pass = DrawPass::new(cx);
            pass.set_size(cx, dvec2(width, HEIGHT));
            let mut list = DrawList2d::new(cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(dvec2(width, HEIGHT), Layout::flow_overlay());
            home.draw_walk_all(&mut cx2d, &mut Scope::with_data(state), Walk {
                width: Size::Fixed(width),
                height: Size::Fixed(HEIGHT),
                ..Walk::default()
            });
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        });
        deliver(cx, home, state, emitted);
    }

    fn deliver(cx: &mut Cx, home: &mut HomeScreen, state: &mut AppState, actions: ActionsBuf) {
        if !actions.is_empty() {
            let more = cx.capture_actions(|cx| home.handle_event(cx, &Event::Actions(actions), &mut Scope::with_data(state)));
            if !more.is_empty() {
                home.handle_event(cx, &Event::Actions(more), &mut Scope::with_data(state));
            }
        }
    }

    fn side(cx: &mut Cx, home: &HomeScreen) -> MiniAppsSideRegionRef {
        home.view.mini_apps_side_region(cx, ids!(mini_apps_side))
    }

    fn rect(cx: &mut Cx, home: &HomeScreen, path: &[LiveId]) -> Rect {
        home.view.widget(cx, path).area().rect(cx)
    }

    /// Opens a chat tab the way the rooms list does.
    fn open_chat(cx: &mut Cx, home: &mut HomeScreen, state: &mut AppState) -> SelectedRoom {
        let room = SelectedRoom::JoinedRoom {
            room_name_id: RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Design".into()), "!design:example.org".try_into().unwrap()),
        };
        let uid = home.view.widget(cx, ids!(rooms_list)).widget_uid();
        let selected = cx.capture_actions(|cx| cx.widget_action(uid, RoomsListAction::Selected(room.clone())));
        deliver(cx, home, state, selected);
        room
    }

    /// The tab `main_tabs` shows, from the dock's own state.
    fn main_tabs_selection(cx: &mut Cx, home: &HomeScreen) -> Option<LiveId> {
        let dock = home.view.dock(cx, ids!(dock));
        let (tabs_id, _) = dock.find_tab_bar_of_tab(id!(home_tab))?;
        match dock.clone_state()?.get(&tabs_id)? {
            DockItem::Tabs {tabs, selected, ..} => tabs.get(*selected).copied(),
            _ => None,
        }
    }

    fn click(cx: &mut Cx, home: &mut HomeScreen, state: &mut AppState, uid: WidgetUid) {
        let clicked = cx.capture_actions(|cx| cx.widget_action(uid, NavigationBarButtonAction::Clicked));
        deliver(cx, home, state, clicked);
    }

    /// The Agent tab's Hosting page, through the production dock: setup
    /// replaces the dashboard until the daemon hosts the owner (ADR-192).
    #[cfg(feature = "onboarding-test-support")]
    #[test]
    fn agent_tab_hosting_page_shows_setup_until_the_daemon_hosts() {
        use crate::hagency::hosting::{Execution, SetupReason, View};
        let (mut cx, mut home, mut state) = home(1440.0);
        frame(&mut cx, &mut home, &mut state, 1440.0);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::OpenAgent);
        let surface = region.surface(&mut cx, region.selected().unwrap());
        let setup = View { execution: Some(Ok(Execution::Setup(SetupReason::NotEnrolled))), service: None, job: None, generation: 1 };
        surface.owner_fixture_hosting(&mut cx, &setup, true);
        frame(&mut cx, &mut home, &mut state, 1440.0);
        assert_eq!(surface.hosting_state(&mut cx), Some(true), "the Hosting tab is in front");
        let visible = |cx: &Cx, path: &[LiveId]| surface.widget(cx, path).visible();
        assert!(visible(&cx, ids!(owner_host.dashboard.od_setup_page)));
        assert!(!visible(&cx, ids!(owner_host.dashboard.od_overview)));
        let hosted = View { execution: Some(Ok(Execution::Daemon { home: "/h".into() })), service: None, job: None, generation: 2 };
        surface.owner_fixture_hosting(&mut cx, &hosted, true);
        surface.update_owner_fixture(&mut cx, owner_dashboard::fixtures::ordinary(owner_dashboard::dto::HostingState::Running));
        frame(&mut cx, &mut home, &mut state, 1440.0);
        assert!(!visible(&cx, ids!(owner_host.dashboard.od_setup_page)));
        assert!(visible(&cx, ids!(owner_host.dashboard.od_options_card)));
        region.close_all(&mut cx);
    }

    #[test]
    fn agent_sidebar_opens_hagency_and_reselects_its_retained_management_page() {
        let width = 1440.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        let button = home.view.widget(&mut cx, ids!(agent_button));
        assert!(!button.is_empty());
        let target = button.area().rect(&cx);
        assert!(target.size.x >= 44.0 && target.size.y >= 44.0);
        let pressed = cx.capture_actions(|cx| cx.widget_action(button.widget_uid(), NavigationBarButtonAction::Clicked));
        let emitted = cx.capture_actions(|cx| home.handle_event(cx, &Event::Actions(pressed), &mut Scope::with_data(&mut state)));
        let open = emitted.iter().find_map(|action| action.downcast_ref::<MiniAppsAction>());
        assert!(matches!(open, Some(MiniAppsAction::OpenAgent)));
        let region = side(&mut cx, &home);
        region.open(&mut cx, open.unwrap());
        let agent = region.selected().unwrap();
        assert_eq!(region.key(&mut cx, agent), Some(SurfaceKey::App(crate::miniapps::palpo::APP_ID.to_owned())));
        region.hagency_action(&mut cx, &HagencyAction::OpenProjects);
        assert_eq!(region.key(&mut cx, agent), Some(SurfaceKey::Management));
        region.open(&mut cx, &MiniAppsAction::Open);
        assert_eq!(region.tabs().len(), 2);
        region.open(&mut cx, &MiniAppsAction::OpenAgent);
        assert_eq!(region.selected(), Some(agent));
        assert_eq!(region.tabs().len(), 2, "Agent reuses its tab instead of making a duplicate");
        assert_eq!(region.key(&mut cx, agent), Some(SurfaceKey::Management), "reselecting Agent keeps the page open");
        region.close_all(&mut cx);
    }

    #[test]
    fn opening_mini_apps_docks_tabs_beside_the_chat_and_closing_the_last_hides_them() {
        let width = 1440.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        let room = open_chat(&mut cx, &mut home, &mut state);
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(main_tabs_selection(&mut cx, &home), Some(room.tab_id()), "the chat is open in main_tabs");
        let full_chat = rect(&mut cx, &home, ids!(dock));
        let region = side(&mut cx, &home);
        assert!(region.tabs().is_empty() && region.layout() == SideLayout::Hidden);

        // The left bar's Mini apps: a side tab, while the chat stays selected and usable.
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        let catalog = region.tabs();
        assert_eq!(catalog.len(), 1);
        assert_eq!(region.selected(), catalog.first().copied());
        assert_eq!(region.key(&mut cx, catalog[0]), Some(SurfaceKey::Catalog));
        assert_eq!(region.layout(), SideLayout::Docked);
        assert_eq!(main_tabs_selection(&mut cx, &home), Some(room.tab_id()), "the chat tab stays selected in main_tabs");
        let dock = rect(&mut cx, &home, ids!(dock));
        let docked = rect(&mut cx, &home, ids!(mini_apps_side));
        assert!((docked.size.x - (SIDE_DEFAULT + HANDLE)).abs() < 1.0, "docked at its default width: {docked:?}");
        assert!(dock.pos.x + dock.size.x <= docked.pos.x + 0.5, "beside the chat, not over it: {dock:?} {docked:?}");
        assert!((docked.pos.x + docked.size.x - width).abs() < 1.0, "on the right edge: {docked:?}");
        let chat = dock.size.x - 300.0;
        assert!(chat >= CHAT_MIN && dock.size.x < full_chat.size.x, "the chat keeps its minimum: {dock:?}");
        assert!(home.view.widget(&mut cx, ids!(Desktop.home_screen_page_flip)).visible());

        // Reopening focuses the same tab.
        region.open(&mut cx, &MiniAppsAction::Open);
        assert_eq!(region.tabs(), catalog);

        // Hagency management is its own item, in its own tab.
        region.hagency_action(&mut cx, &HagencyAction::OpenProjects);
        frame(&mut cx, &mut home, &mut state, width);
        let tabs = region.tabs();
        assert_eq!(tabs.len(), 2);
        let management = tabs[1];
        assert_eq!(region.selected(), Some(management));
        assert_eq!(region.key(&mut cx, management), Some(SurfaceKey::Management));
        region.hagency_action(&mut cx, &HagencyAction::OpenAgents);
        assert_eq!(region.tabs(), tabs, "management opens again in its tab");
        region.open(&mut cx, &MiniAppsAction::Open);
        assert_eq!((region.tabs(), region.selected()), (tabs.clone(), Some(catalog[0])), "the catalog's tab is focused again");
        assert_eq!(main_tabs_selection(&mut cx, &home), Some(room.tab_id()));

        // Closing a tab stops its panel; closing the last hides the region and
        // gives the chat its width back.
        let panels: Vec<_> = tabs.iter().map(|id| region.surface(&mut cx, *id).panel(&mut cx)).collect();
        assert!(panels.iter().all(|panel| panel.is_open()));
        for (index, id) in tabs.iter().rev().enumerate() {
            frame(&mut cx, &mut home, &mut state, width);
            let strip = home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.tabs));
            let mut close = None;
            strip.children(&mut |tab_id, tab| if tab_id == *id {
                close = Some(tab.widget(&cx, ids!(close)).widget_uid());
            });
            click(&mut cx, &mut home, &mut state, close.expect("each tab has a close button"));
            assert_eq!(region.tabs().len(), 1 - index);
            assert!(!panels[tabs.len() - 1 - index].is_open(), "closing a tab stops its app and revokes its lease");
        }
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(region.layout(), SideLayout::Hidden);
        assert!(!home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        assert_eq!(rect(&mut cx, &home, ids!(dock)).size.x, full_chat.size.x);
        assert_eq!(main_tabs_selection(&mut cx, &home), Some(room.tab_id()));
    }

    #[test]
    fn a_tabs_own_back_on_its_catalog_closes_only_that_tab() {
        let width = 1440.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        let region = side(&mut cx, &home);
        region.hagency_action(&mut cx, &HagencyAction::OpenProjects);
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        let tabs = region.tabs();
        assert_eq!(tabs.len(), 2);
        let catalog = tabs[1];
        assert_eq!(region.key(&mut cx, catalog), Some(SurfaceKey::Catalog));
        // The catalog panel's own Back, as the person taps it.
        let back = home.view.widget(&mut cx, &[id!(mini_apps_side), catalog, id!(mini_apps_panel), id!(close)]).widget_uid();
        let tapped = cx.capture_actions(|cx| cx.widget_action(back, ButtonAction::Clicked(KeyModifiers::default())));
        let emitted = cx.capture_actions(|cx| home.handle_event(cx, &Event::Actions(tapped), &mut Scope::with_data(&mut state)));
        assert_eq!(region.tabs(), vec![tabs[0]], "only its own tab closes");
        assert_eq!(region.selected(), Some(tabs[0]));
        assert!(!emitted.iter().any(|action| matches!(action.downcast_ref(), Some(MiniAppsAction::Close))),
            "the app shell never sees it, so other tabs and the window stay open");
    }

    #[test]
    fn a_window_too_narrow_for_both_shows_the_region_over_the_chat_with_back() {
        // A desktop layout, but too narrow for the rooms list, the chat and the region.
        let width = 1000.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(region.layout(), SideLayout::Overlay);
        let page = home.view.widget(&mut cx, ids!(Desktop.home_screen_page_flip));
        assert!(!page.visible(), "the overlay replaces the page instead of squeezing the chat");
        let over = rect(&mut cx, &home, ids!(mini_apps_side));
        let rail = rect(&mut cx, &home, ids!(navigation_tab_bar.Desktop));
        assert!(rail.size.x > 0.0);
        assert!((over.pos.x - (rail.pos.x + rail.size.x)).abs() < 1.0 && (over.pos.x + over.size.x - width).abs() < 1.0, "{over:?}");
        assert!(home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.back)).visible());
        assert!(!home.view.widget(&mut cx, ids!(mini_apps_side.handle)).visible());

        // Back returns to the chat; the tab keeps running and comes back on reopening.
        let back = home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.back)).widget_uid();
        let tabs = region.tabs();
        let panel = region.surface(&mut cx, tabs[0]).panel(&mut cx);
        click(&mut cx, &mut home, &mut state, back);
        frame(&mut cx, &mut home, &mut state, width);
        assert!(page.visible() && !home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        assert_eq!(region.tabs(), tabs);
        assert!(panel.is_open());
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(region.tabs(), tabs);
        assert!(!page.visible() && home.view.widget(&mut cx, ids!(mini_apps_side)).visible());

        // Widening docks it beside the chat; narrowing again never covers the chat by itself.
        frame(&mut cx, &mut home, &mut state, 1440.0);
        assert_eq!(region.layout(), SideLayout::Docked);
        assert!(page.visible());
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(region.layout(), SideLayout::Overlay);
        assert!(page.visible() && !home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        assert!(panel.is_open());
    }

    #[test]
    fn the_mobile_layout_shows_the_region_over_the_screen_and_left_bar_navigation_steps_it_aside() {
        let width = 600.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        assert!(!effective_is_desktop(&mut cx));
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        assert_eq!(region.layout(), SideLayout::Overlay);
        let over = rect(&mut cx, &home, ids!(mini_apps_side));
        assert!(over.pos.x.abs() < 1.0 && (over.size.x - width).abs() < 1.0 && (over.size.y - HEIGHT).abs() < 1.0, "{over:?}");
        assert!(home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.back)).visible());
        let back = home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.back)).widget_uid();
        click(&mut cx, &mut home, &mut state, back);
        frame(&mut cx, &mut home, &mut state, width);
        assert!(!home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        assert_eq!(region.tabs().len(), 1, "Back keeps the tab");

        // In a narrow desktop window, choosing another page in the left bar steps the overlay aside.
        let width = 1000.0;
        frame(&mut cx, &mut home, &mut state, width);
        region.open(&mut cx, &MiniAppsAction::Open);
        frame(&mut cx, &mut home, &mut state, width);
        assert!(home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        let chosen = cx.capture_actions(|cx| cx.action(NavigationBarAction::TabSelected(SelectedTab::Contacts)));
        deliver(&mut cx, &mut home, &mut state, chosen);
        frame(&mut cx, &mut home, &mut state, width);
        assert!(!home.view.widget(&mut cx, ids!(mini_apps_side)).visible());
        assert!(home.view.widget(&mut cx, ids!(Desktop.home_screen_page_flip)).visible());
    }

    /// Every rect drawn under `list`, sub lists included.
    fn drawn_rects(cx: &Cx, list: DrawListId, out: &mut Vec<Rect>) {
        let draw_list = &cx.draw_lists[list];
        for index in 0..draw_list.draw_items.len() {
            let item = &draw_list.draw_items[index];
            if let Some(sub) = item.sub_list() {
                drawn_rects(cx, sub, out);
                continue;
            }
            let (Some(call), Some(instances)) = (item.draw_call(), item.instances.as_ref()) else { continue };
            let mapping = &cx.draw_shaders.shaders[call.draw_shader_id.index].mapping;
            let (Some(pos), Some(size)) = (mapping.rect_pos, mapping.rect_size) else { continue };
            for instance in instances.chunks_exact(mapping.instances.total_slots) {
                out.push(Rect {
                    pos: dvec2(instance[pos] as f64, instance[pos + 1] as f64),
                    size: dvec2(instance[size] as f64, instance[size + 1] as f64),
                });
            }
        }
    }

    #[test]
    fn the_mini_apps_panel_fits_a_minimum_width_tab() {
        let width = 1440.0;
        let (mut cx, mut home, mut state) = home(width);
        state.mini_apps_width = Some(SIDE_MIN);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        for _ in 0..3 {
            frame(&mut cx, &mut home, &mut state, width);
        }
        let docked = rect(&mut cx, &home, ids!(mini_apps_side));
        assert!((docked.size.x - (SIDE_MIN + HANDLE)).abs() < 1.0, "{docked:?}");
        let catalog = rect(&mut cx, &home, ids!(mini_apps_side.catalog_list));
        assert!(catalog.size.x > 0.0 && catalog.size.y > 0.0, "the catalog shows: {catalog:?}");
        // Draw once more and inspect what was submitted for the region.
        let root;
        {
            cx.redraw_all();
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, HEIGHT));
            let mut list = DrawList2d::new(&mut cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            root = cx2d.get_current_draw_list_id().unwrap();
            cx2d.begin_root_turtle(dvec2(width, HEIGHT), Layout::flow_overlay());
            home.draw_walk_all(&mut cx2d, &mut Scope::with_data(&mut state), Walk {width: Size::Fixed(width), height: Size::Fixed(HEIGHT), ..Walk::default()});
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        let mut rects = Vec::new();
        drawn_rects(&cx, root, &mut rects);
        let inside: Vec<_> = rects.iter()
            .filter(|r| r.size.x > 0.0 && r.size.y > 0.0 && r.pos.x >= docked.pos.x + HANDLE && r.pos.y >= catalog.pos.y && r.pos.y < catalog.pos.y + catalog.size.y)
            .collect();
        assert!(inside.len() > 10, "the catalog's cards were drawn: {}", inside.len());
        for r in inside {
            assert!(r.pos.x + r.size.x <= docked.pos.x + docked.size.x + 1.0, "nothing runs past the tab's right edge: {r:?} in {docked:?}");
        }
    }

    #[test]
    fn the_docked_width_is_remembered_and_never_squeezes_the_chat() {
        let (mut cx, mut home, mut state) = home(1600.0);
        frame(&mut cx, &mut home, &mut state, 1600.0);
        frame(&mut cx, &mut home, &mut state, 1600.0);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        state.mini_apps_width = Some(560.0);
        frame(&mut cx, &mut home, &mut state, 1600.0);
        assert!((rect(&mut cx, &home, ids!(mini_apps_side)).size.x - (560.0 + HANDLE)).abs() < 1.0);
        // Narrower, the remembered width gives way so the chat keeps its minimum.
        let width = 1280.0;
        frame(&mut cx, &mut home, &mut state, width);
        let docked = rect(&mut cx, &home, ids!(mini_apps_side));
        let dock = rect(&mut cx, &home, ids!(dock));
        assert!(docked.size.x - HANDLE >= SIDE_MIN && docked.size.x < 560.0, "{docked:?}");
        assert!(dock.size.x - 300.0 >= CHAT_MIN - 0.5, "{dock:?}");
        assert_eq!(state.mini_apps_width, Some(560.0), "the person's choice is kept for wider windows");
    }

    #[test]
    fn native_reader_and_editor_reuse_tabs_and_survive_docking() {
        use crate::article_app::{ArticleAction, ArticlePanelWidgetRefExt};
        use crate::shared::web_browser::WebBrowserAction;
        use crate::miniapps::window::MiniAppsWindowHostWidgetRefExt;
        let (mut cx, mut home, mut state) = home(1440.0);
        frame(&mut cx, &mut home, &mut state, 1440.0);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        region.web_action(&mut cx, &WebBrowserAction::Open("https://example.org/first".parse().unwrap()));
        let reader = region.reader(&mut cx);
        let browser = reader.browser_id();
        assert_eq!(
            browser.is_some(),
            cfg!(any(target_os = "macos", target_os = "ios", target_os = "android", target_os = "windows")),
            "the web tab has a native browser only on supported platforms"
        );
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android", target_os = "windows")))]
        assert_eq!(
            reader.borrow().unwrap().label(&mut cx, ids!(web_status)).text(),
            crate::i18n::tr("In-app browsing is unavailable on this platform. Use Open in browser."),
            "unsupported platforms explain how to open web links externally"
        );
        region.article_action(&mut cx, &ArticleAction::Open);
        let editor_id = region.selected().unwrap();
        let editor = region.surface(&mut cx, editor_id);
        let title = editor.text_input(&mut cx, ids!(article_title));
        assert!(title.borrow().is_some(), "the real native editor is hosted in the tab");
        title.set_text(&mut cx, "Unsent article draft");
        let article = editor.widget(&mut cx, ids!(native_content.panel)).as_article_panel();
        article.read_markdown(&mut cx, "Retained document", "# Body\n\nKeep this content.").unwrap();
        assert_eq!(region.tabs().len(), 3);
        region.web_action(&mut cx, &WebBrowserAction::Open("https://example.org/second".parse().unwrap()));
        assert_eq!(region.reader(&mut cx).widget_uid(), reader.widget_uid());
        assert_eq!(reader.session().tabs.len(), 2);
        region.article_action(&mut cx, &ArticleAction::Open);
        assert_eq!(region.selected(), Some(editor_id));
        assert_eq!(article.reader_title(), "Retained document", "reopening must not reset the document model");
        assert_eq!(title.text(), "Unsent article draft");
        cx.with_vm(|vm| {
            let value = script_eval!(vm, {mod.widgets.MiniAppsSurface {}});
            editor.clone().script_apply(vm, &Apply::ScriptReapply, &mut Scope::empty(), value);
        });
        assert_eq!(article.reader_title(), "Retained document", "restyling retains the native document");
        let host = cx.with_vm(|vm| {
            let value = script_eval!(vm, {mod.widgets.MiniAppsWindowHost {}});
            WidgetRef::script_from_value(vm, value)
        }).as_mini_apps_window_host();
        let original_tabs = region.tabs();
        let browser_before_move = reader.browser_id();
        let popped = region.take_workspace(&mut cx).unwrap();
        host.adopt_workspace(&mut cx, popped);
        assert!(region.tabs().is_empty(), "all tabs move together");
        assert_eq!(host.workspace(&mut cx).tabs(), original_tabs);
        assert_eq!(host.surface_key(&mut cx), Some(SurfaceKey::Articles));
        assert_eq!(host.reader(&mut cx).widget_uid(), reader.widget_uid());
        assert_eq!(host.reader(&mut cx).browser_id(), browser_before_move);
        assert_eq!(host.reader(&mut cx).session().tabs.len(), 2);
        // Opening another web page while the editor is selected must route to
        // the floating reader, keeping every existing browser tab alive.
        host.web_action(&mut cx, &WebBrowserAction::Open("https://example.org/third".parse().unwrap()));
        assert_eq!(host.workspace(&mut cx).tabs(), original_tabs);
        assert_eq!(host.reader(&mut cx).session().tabs.len(), 3);
        let third_browser = host.reader(&mut cx).browser_id();
        host.article_action(&mut cx, &ArticleAction::Open);
        assert_eq!(title.text(), "Unsent article draft");
        let returned = host.take_for_dock(&mut cx, host.window_id().unwrap()).unwrap();
        region.restore_workspace(&mut cx, returned);
        assert_eq!(region.tabs(), original_tabs);
        assert_eq!(region.selected(), Some(editor_id));
        assert_eq!(region.reader(&mut cx).browser_id(), third_browser);
        assert_eq!(region.reader(&mut cx).session().tabs.len(), 3);
        assert_eq!(region.surface(&mut cx, region.selected().unwrap()).widget_uid(), editor.widget_uid());
        assert_eq!(title.text(), "Unsent article draft");
        region.web_action(&mut cx, &WebBrowserAction::Close);
        assert_eq!(region.tabs().len(), 2);
        assert!(!region.tabs().iter().any(|id| region.key(&mut cx, *id) == Some(SurfaceKey::Reader)));
        assert!(reader.browser_id().is_none(), "closing destroys the browser {browser:?}");
        region.article_action(&mut cx, &ArticleAction::Close);
        assert_eq!(region.tabs().len(), 1, "closing native tabs keeps the mini-app catalog");
    }

    #[test]
    fn pop_out_moves_all_running_tabs_into_one_window_and_back() {
        let width = 1440.0;
        let (mut cx, mut home, mut state) = home(width);
        frame(&mut cx, &mut home, &mut state, width);
        frame(&mut cx, &mut home, &mut state, width);
        let region = side(&mut cx, &home);
        region.open(&mut cx, &MiniAppsAction::Open);
        region.hagency_action(&mut cx, &HagencyAction::OpenProjects);
        frame(&mut cx, &mut home, &mut state, width);
        let tabs = region.tabs();
        let management = region.surface(&mut cx, tabs[1]);
        let panel = management.panel(&mut cx);

        let catalog_panel = region.surface(&mut cx, tabs[0]).panel(&mut cx);
        // The pop-out button asks the app shell to move the whole workspace.
        let pop_out = home.view.widget(&mut cx, ids!(mini_apps_side.frame.header.pop_out)).widget_uid();
        let clicked = cx.capture_actions(|cx| cx.widget_action(pop_out, NavigationBarButtonAction::Clicked));
        let emitted = cx.capture_actions(|cx| home.handle_event(cx, &Event::Actions(clicked), &mut Scope::with_data(&mut state)));
        assert!(emitted.iter().any(|a| matches!(a.downcast_ref(), Some(crate::miniapps::side::MiniAppsSideAction::PopOut))));

        // Every live surface moves, preserving tab IDs, order and selection.
        let host = cx.with_vm(|vm| {
            let value = script_eval!(vm, { mod.widgets.MiniAppsWindowHost {} });
            WidgetRef::script_from_value(vm, value)
        });
        use crate::miniapps::window::MiniAppsWindowHostWidgetRefExt;
        let host_widget = host.clone();
        let host = host.as_mini_apps_window_host();
        let workspace = region.take_workspace(&mut cx).expect("all tabs");
        host.adopt_workspace(&mut cx, workspace);
        assert_eq!(host.workspace(&mut cx).tabs(), tabs);
        assert_eq!(host.workspace(&mut cx).selected(), Some(tabs[1]));
        assert_eq!(host.surface(&mut cx).widget_uid(), management.widget_uid());
        assert_eq!(host.surface_key(&mut cx), Some(SurfaceKey::Management));
        assert!(region.tabs().is_empty());
        assert!(panel.is_open(), "popping out keeps the app running and its lease");

        // The explicit Dock back button moves the same running surface home.
        let first_window = host.window_id().unwrap();
        let dock_back = host_widget.widget(&mut cx, ids!(mini_apps_window.dock_back)).widget_uid();
        let clicked = cx.capture_actions(|cx| cx.widget_action(dock_back, ButtonAction::Clicked(KeyModifiers::default())));
        let emitted = cx.capture_actions(|cx| host_widget.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()));
        assert!(emitted.iter().any(|a| matches!(a.downcast_ref(), Some(crate::miniapps::window::MiniAppsWindowAction::DockBack(id)) if *id == first_window)));
        assert!(host.take_for_dock(&mut cx, WindowId(999, 1)).is_none());
        let returned = host.take_for_dock(&mut cx, first_window).expect("dock every running tab");
        assert!(host.take_for_dock(&mut cx, first_window).is_none(), "duplicate clicks do not transfer twice");
        region.restore_workspace(&mut cx, returned);
        assert_eq!(region.tabs(), tabs);
        assert_eq!(region.selected(), Some(tabs[1]));
        assert!(panel.is_open(), "docking keeps its lease and app state");
        assert_eq!(region.key(&mut cx, region.selected().unwrap()), Some(SurfaceKey::Management));

        // Reopen before the previous window's close notification arrives.
        let workspace = region.take_workspace(&mut cx).unwrap();
        host.adopt_workspace(&mut cx, workspace);
        let second_window = host.window_id().unwrap();
        assert_ne!(second_window, first_window);
        host_widget.handle_event(&mut cx, &Event::WindowClosed(WindowClosedEvent {window_id: first_window}), &mut Scope::empty());
        assert_eq!(host.window_id(), Some(second_window));
        assert!(host.take_for_dock(&mut cx, first_window).is_none());
        assert!(panel.is_open(), "an old window's close event cannot stop the moved app");
        let returned = host.take_for_dock(&mut cx, second_window).unwrap();
        region.restore_workspace(&mut cx, returned);
        host_widget.handle_event(&mut cx, &Event::WindowClosed(WindowClosedEvent {window_id: second_window}), &mut Scope::empty());
        assert_eq!(host.window_id(), None);
        assert!(panel.is_open(), "closing the empty native window preserves the docked app");
        let workspace = region.take_workspace(&mut cx).unwrap();
        host.adopt_workspace(&mut cx, workspace);

        // Reopening a background catalog focuses its existing floating tab.
        host.action(&mut cx, &MiniAppsAction::Open);
        assert_eq!(host.workspace(&mut cx).tabs(), tabs);
        assert!(region.tabs().is_empty());
        assert_eq!(host.surface_key(&mut cx), Some(SurfaceKey::Catalog));

        // Opening a chat from one tab must not close the other tabs or drafts.
        let show_chat = cx.capture_actions(|cx| cx.action(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats));
        host_widget.handle_event(&mut cx, &Event::Actions(show_chat), &mut Scope::empty());
        assert!(host.is_open());
        assert_eq!(host.workspace(&mut cx).tabs(), tabs);

        // Closing the whole window stops all its apps, including background tabs.
        let window_panel = host.surface(&mut cx).panel(&mut cx);
        assert!(window_panel.is_open());
        host.close(&mut cx);
        assert!(!window_panel.is_open());
        assert!(!panel.is_open());
        assert!(!catalog_panel.is_open());
    }
}
