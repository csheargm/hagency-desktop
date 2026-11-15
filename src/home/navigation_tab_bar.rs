//! The NavigationTabBar shows a bar of icon buttons that allow the user to
//! navigate or switch between various top-level views in Rinx.
//!
//! The bar is positioned either within the left side bar (in the wide "Desktop" view mode)
//! or along the bottom of the app window (in the narrow "Mobile" view mode).
//!
//! All the buttons in this bar — including the `ProfileIcon` and the entries
//! in the embedded `SpacesBar` — are instances of the unified
//! [`NavigationBarButton`](crate::shared::navigation_bar_button::NavigationBarButton)
//! base widget, which provides hover and "selected" background animations.
//!
//! Mobile uses four persistent roots: Chats, Contacts, Discover, and Me.
//! Room navigation pushes above these roots and preserves the selected tab.
//!
//! The order in Desktop view (vertically from top to bottom) is:
//! 1. Chats (all rooms) and Agents, the two primary items
//! 2. (empty space) — or, while "More" is open, the SpacesBar followed by
//!    Contacts, Moments, Mini apps and File Transfer (see `navigation_tab_bar_more`)
//! 3. More
//! 4. ----- separator -----
//! 5. Settings (the user's avatar) and Account (appearance is in Settings → Preferences)
//!
//! "New or Join" is not a rail button: it is the "+" beside the chat list's
//! search field (see `rooms_sidebar`). Explore Rooms, which its menu opens,
//! keeps Home marked, since it is reached from the chat list.
//!

use crate::theme::Snapshot as ThemeSnapshot;
use makepad_widgets::*;
use serde::{Deserialize, Serialize};
use crate::{
    home::account_menu::{is_desktop_layout, AccountMenuAction},
    settings::app_preferences::ViewModeOverride,
    shared::navigation_bar_button::NavigationBarButtonWidgetExt,
    utils::RoomNameId,
};

#[path = "navigation_tab_bar_dsl.rs"] mod dsl;
#[path = "navigation_tab_bar_profile_icon.rs"] mod profile_icon;
#[path = "navigation_tab_bar_selection.rs"] mod selection;
#[path = "navigation_tab_bar_account_bar.rs"] mod account_bar;
#[path = "navigation_tab_bar_more.rs"] mod more;
#[cfg(test)]
#[path = "navigation_tab_bar_tests.rs"] mod rail_tests;

pub use account_bar::{account_bar_script_mod, get_own_profile, HagencyAccountBar};
pub use profile_icon::ProfileIcon;
use profile_icon::ProfileIconWidgetExt;

/// Registers the rail's widgets: the avatar button first, then the rail itself.
pub fn script_mod(vm: &mut ScriptVm) {
    profile_icon::script_mod(vm);
    dsl::script_mod(vm);
}

/// The tab bar with buttons that navigate through top-level app pages.
///
/// * In the "desktop" (wide) layout, this is a vertical bar on the left.
/// * In the "mobile" (narrow) layout, this is a horizontal bar on the bottom.
#[derive(Script, Widget)]
pub struct NavigationTabBar {
    #[rust]
    appearance: ThemeSnapshot,
    #[deref]
    view: AdaptiveView,

    /// Whether the group behind "More" is open.
    #[rust] more_open: bool,

    /// The most recently applied view-mode override,
    #[rust] applied_view_mode: ViewModeOverride,

    /// The tab currently visually marked as selected.
    #[rust] selected_tab: SelectedTab,
    #[rust] mobile_palette: Option<(WidgetUid, SelectedTab)>,
    /// Whether the Agent tab currently shows the pending-permission alert.
    #[rust] approval_alert: Option<bool>,
}

impl ScriptHook for NavigationTabBar {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        self.appearance = crate::theme::snapshot_for_vm(vm);
        self.mobile_palette = None;
        self.approval_alert = None;
    }
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        vm.with_cx_mut(|cx| {
            self.apply_selected_tab(cx, None);
        });
    }

    fn on_after_reload(&mut self, vm: &mut ScriptVm) {
        vm.with_cx_mut(|cx| {
            self.mobile_palette = None;
            self.apply_selected_tab(cx, None);
        });
    }
}


impl Widget for NavigationTabBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            for (id, tab) in [
                (ids!(chats_tab), SelectedTab::Home),
                (ids!(contacts_tab), SelectedTab::Contacts),
                (ids!(discover_tab), SelectedTab::Discover),
                (ids!(me_tab), SelectedTab::Me),
            ] {
                if self.view.navigation_bar_button(cx, id).clicked(actions) {
                    cx.action(NavigationBarAction::GoToTab(tab));
                }
            }
            // Handle clicks on each of the navigation tab buttons.
            // Each click both updates the visual selection and emits the
            // corresponding `NavigationBarAction` for downstream handling.
            if self.view.navigation_bar_button(cx, ids!(home_button)).clicked(actions) {
                self.apply_selected_tab(cx, Some(SelectedTab::Home));
                cx.action(NavigationBarAction::GoToHome);
            }
            else {
                // ProfileIcon's inner NavigationBarButton emits the click action,
                // and ProfileIcon derefs into it, so the same `clicked()` check works.
                let profile_icon_ref = self.view.profile_icon(cx, ids!(profile_icon));
                let profile_clicked = profile_icon_ref
                    .borrow()
                    .is_some_and(|p| p.inner.clicked(actions));
                if profile_clicked {
                    self.apply_selected_tab(cx, Some(SelectedTab::Settings));
                    cx.action(NavigationBarAction::OpenSettings);
                }
            }

            // The bottom account button opens the AccountMenu, anchored so the card's
            // BOTTOM-left sits at the button's bottom-right (the App grows it upward).
            if is_desktop_layout(cx)
                && self.view.navigation_bar_button(cx, ids!(account_switcher_button)).clicked(actions)
            {
                let rect = self.view.widget(cx, ids!(account_switcher_button)).area().rect(cx);
                cx.action(AccountMenuAction::Open {
                    pos: dvec2(rect.pos.x + rect.size.x + 4.0, rect.pos.y + rect.size.y),
                });
            }

            if self.view.navigation_bar_button(cx, ids!(agent_button)).clicked(actions) {
                cx.action(crate::octoscript_apps::MiniAppsAction::OpenAgent);
            }
            self.handle_more_clicks(cx, actions);

            self.handle_nav_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<crate::app::AppState>() {
            self.selected_tab = state.selected_tab.clone();
        }
        self.apply_selected_tab(cx, None);
        let step = self.view.draw_walk(cx, scope, walk);
        // AdaptiveView creates its variant during draw. Apply again after that
        // first draw so a new mobile bar doesn't remain visually unselected
        // until a tap or an unrelated sync signal arrives.
        self.apply_selected_tab(cx, None);
        self.apply_approval_alert(cx);
        self.apply_more_open(cx);
        // Every rail tooltip follows the interface language, including after a switch.
        let primary: [(&[LiveId; 1], &str); 4] = [
            (ids!(home_button), "Chats"),
            (ids!(agent_button), "Agents"),
            (ids!(more_button), "More"),
            (ids!(account_switcher_button), "Account"),
        ];
        for (id, english) in primary.into_iter().chain(more::MORE_ITEMS) {
            if let Some(mut button) = self.view.navigation_bar_button(cx, id).borrow_mut() {
                button.set_tooltip_text(crate::i18n::tr(english));
            }
        }
        step
    }
}


/// Which top-level view is currently shown, and which navigation tab is selected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectedTab {
    #[default]
    Home,
    Contacts,
    Discover,
    Me,
    AddRoom,
    Settings,
    // AlertsInbox,
    Space { space_name_id: RoomNameId },
}
impl SelectedTab {
    /// Updates this tab's space name if it refers to the same space.
    ///
    /// Returns `true` if the name was changed.
    pub fn update_space_name(&mut self, new_space_name: &RoomNameId) -> bool {
        let SelectedTab::Space { space_name_id } = self else { return false };
        if space_name_id.room_id() != new_space_name.room_id()
            || space_name_id.display_name() == new_space_name.display_name()
        {
            return false;
        }
        *space_name_id = new_space_name.clone();
        true
    }
}


/// Actions for navigating through the top-level views of the app,
/// e.g., when the user clicks/taps on a button in the NavigationTabBar.
///
/// ## Tip: you only want to handle `TabSelected`
/// The most important variant is `TabSelected`, which is most likely the action
/// that you want to handle in other widgets, if you care about which
/// top-level navigation tab is currently selected.
/// This is because the `TabSelected` variant will always occur even if the
/// other actions do not occur --- for example, if the user chooses to jump
/// to a different view (or back to a previous view) without explicitly clicking
/// a navigation tab button, e.g., via a keyboard shortcut, or programmatically.
///
/// Only one widget, the `HomeScreen`, should emit the `TabSelected` action.
/// All other widgets should handle only that action in order to ensure
/// consistent behavior.
///
/// ## More details
/// There are 3 kinds of actions within this one enum:
/// 1. "Leading-edge" ("request") actions emitted by the NavigationTabBar
///    when the user selects a particular button/space.
///    * Includes `GoToHome`, `GoToAddRoom`, `GoToSpace`, `OpenSettings`, `CloseSettings`.
/// 2. "Trailing-edge" ("response") actions that are emitted by the `HomeScreen` widget
///    in response to a leading-edge action.
///    * This includes only the `TabSelected` variant.
///    * This is what all other widgets should handle if they want/need to respond
///      to changes in the top-level app-wide navigation selection.
/// 3. Other actions that aren't requests/responses to navigate to a different view.
///    * This only includes the `ToggleSpacesBar` variant.
#[derive(Debug, PartialEq, Eq)]
pub enum NavigationBarAction {
    /// Select a top-level root while keeping its page state.
    GoToTab(SelectedTab),
    /// Open the existing contact search to start a conversation.
    NewChat,
    /// Go to the main rooms content view.
    GoToHome,
    /// Go to the Explore Rooms view (join, search or preview rooms).
    /// On desktop it comes from the "New or Join" menu beside the chat list's search.
    GoToAddRoom,
    /// Leave Explore Rooms and restore the tab that opened it.
    CloseAddRoom,
    /// Go to the Settings view (open the `SettingsScreen`).
    OpenSettings,
    /// Open the current user's mobile Personal Information page.
    OpenOwnProfile,
    /// Close the Settings view (`SettingsScreen`), returning to the previous view.
    CloseSettings,
    /// Go the space screen for the given space.
    GoToSpace { space_name_id: RoomNameId },

    // TODO: add GoToAlertsInbox, once we add that button/screen

    /// The given tab was selected as the active top-level view.
    /// This is needed to ensure that the proper tab is marked as selected. 
    TabSelected(SelectedTab),
    /// Toggle whether the SpacesBar is shown, i.e., show/hide it.
    /// This is only applicable in the Mobile view mode, because the SpacesBar
    /// is always shown in Desktop view mode.
    ToggleSpacesBar,
}

