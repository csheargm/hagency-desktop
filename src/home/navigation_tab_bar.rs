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
//! 1. Profile/Settings
//! 2. Home (All Rooms)
//! 3. Contacts, Agent, Moments, Mini apps
//! 4. File Transfer (opens the same private File Transfer chat as before)
//! 5. ----- separator -----
//!      SpacesBar content
//! 6. ----- separator -----
//! 7. Account (appearance is in Settings → Preferences)
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
    avatar_cache::{self, AvatarCacheEntry},
    login::login_screen::LoginAction,
    logout::logout_confirm_modal::LogoutAction,
    profile::{
        user_profile::UserProfile,
        user_profile_cache::{self, UserProfileUpdate},
    },
    home::home_screen::effective_is_desktop,
    settings::app_preferences::{AppPreferencesAction, ViewModeOverride},
    shared::{
        avatar::{AvatarState, AvatarWidgetExt},
        navigation_bar_button::{NavigationBarButton, NavigationBarButtonWidgetExt},
        verification_badge::VerificationBadgeWidgetExt,
    },
    sliding_sync::{current_user_id, AccountDataAction},
    utils::{self, RoomNameId},
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    // The base style definition for icon buttons in the NavigationTabBar.
    //
    // Dark navy nav-rail item (robrix2 visual spec §5.6 / RBX_NAV_* tokens):
    // transparent when idle so the navy rail shows through, a navy "pill" on
    // hover/active, plus a teal accent bar on the left edge of the *active* item
    // to echo the app-wide teal selection language. The icon itself is recolored
    // white when selected via the animator below (DrawSvg has no per-state color).
    mod.widgets.NavigationTabButton = mod.widgets.NavigationBarButton {
        width: Fill,
        // Normally (NAVIGATION_TAB_BAR_SIZE - 12) tall and never taller. In a short
        // window every rail button gives up height evenly, down to a 44-point
        // target, before the bottom buttons would be pushed off the rail.
        height: Fill{weight: 0.0 basis: FitBound.Abs((NAVIGATION_TAB_BAR_SIZE - 12)) shrink: 1.0},
        min_height: FitBound.Abs(44.0),
        padding: (SPACE_XS),
        margin: Inset{top: 2, bottom: 2, left: (SPACE_XS), right: (SPACE_XS)},
        align: Align{x: 0.5, y: 0.5}
        flow: Down,

        draw_bg +: {
            color_hover: (RBX_NAV_ITEM_HOVER_BG)
            color_active: (RBX_NAV_ITEM_ACTIVE_BG)
            accent_color: instance((RBX_ACCENT))
            border_radius: (RBX_RADIUS_SM)

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(
                    self.border_inset.x + self.border_size,
                    self.border_inset.y + self.border_size,
                    self.rect_size.x - (self.border_inset.x + self.border_inset.z + self.border_size * 2.0),
                    self.rect_size.y - (self.border_inset.y + self.border_inset.w + self.border_size * 2.0),
                    max(1.0, self.border_radius)
                )
                // `fill_keep` leaves the pill in the sdf shape, and `box` unions with it,
                // so the accent bar below would flood the whole pill. `fill`/`stroke` reset it.
                if self.border_size > 0.0 {
                    sdf.fill_keep(self.get_color())
                    sdf.stroke(self.border_color, self.border_size)
                } else {
                    sdf.fill(self.get_color())
                }
                // Teal selection bar on the left edge, shown only when active.
                let bar_inset = 12.0
                sdf.box(
                    0.0,
                    bar_inset,
                    3.0,
                    self.rect_size.y - bar_inset * 2.0,
                    1.5
                )
                sdf.fill(mix(vec4(0.0, 0.0, 0.0, 0.0), self.accent_color, self.active))
                return sdf.result;
            }
        }

        icon := Icon {
            margin: 0,
            icon_walk: Walk {
                margin: 0,
                width: (RBX_ICON_LG),
                height: (RBX_ICON_LG)
            }
            draw_icon +: {
                color: (RBX_NAV_FG)
            }
        }

        // Same hover/active tracks as the base NavigationBarButton, plus the
        // icon recolor: white (RBX_NAV_FG_ACTIVE) when selected, muted
        // RBX_NAV_FG otherwise.
        animator: Animator {
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.15}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {active: 0.0} icon: { draw_icon: { color: (RBX_NAV_FG) } } }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {active: 1.0} icon: { draw_icon: { color: (RBX_NAV_FG_ACTIVE) } } }
                }
            }
        }
    }

    mod.widgets.ProfileIcon = #(ProfileIcon::register_widget(vm)) {
        ..mod.widgets.NavigationBarButton

        // ProfileIcon emits its own dynamic tooltip (with verification badge info)
        // from Rust, so leave the built-in tooltip text empty.
        tooltip_text: ""

        // Use the same size/shape bounds as other buttons in the NavigationTabBar
        width: Fill,
        height: (NAVIGATION_TAB_BAR_SIZE - 8)
        padding: 0,
        margin: Inset{top: 2, bottom: 2, left: (SPACE_XS), right: (SPACE_XS)},
        align: Align{ x: 0.5, y: 0.5 }

        draw_bg +: {
            color_hover: (RBX_NAV_ITEM_HOVER_BG)
            color_active: (RBX_NAV_ITEM_ACTIVE_BG)
            border_radius: (RBX_RADIUS_SM)
        }

        avatar_with_badge := View {
            width: (NAVIGATION_TAB_BAR_SIZE - 12)
            height: (NAVIGATION_TAB_BAR_SIZE - 12)
            flow: Overlay
            align: Align { x: 0.5, y: 0.5 }

            our_own_avatar := Avatar {
                width: (mod.widgets.NAVIGATION_TAB_BAR_AVATAR_SIZE)
                height: (mod.widgets.NAVIGATION_TAB_BAR_AVATAR_SIZE)
                // If no avatar picture, use white text on a dark background.
                text_view +: {
                    draw_bg.color: (COLOR_FG_DISABLED),
                    text +: {
                        draw_text +: {
                            text_style: theme.font_regular { font_size: mod.widgets.NAVIGATION_TAB_BAR_AVATAR_FONT_SIZE },
                            color: (COLOR_PRIMARY),
                        }
                    }
                }
            }

            // A Fill-sized View that aligns the badge (which is Fit-sized)
            // to the top-right corner of the wrapper. Since the wrapper is
            // larger than the avatar, the badge ends up sitting near the
            // avatar's outer top-right corner, half-overlapping the avatar.
            View {
                width: Fill,
                height: Fill,
                align: Align { x: 1.0, y: 0.0 }
                margin: Inset { left: 0, bottom: 0, top: 2, right: 2 }
                verification_badge := VerificationBadge {}
            }
        }
    }

    mod.widgets.HomeButton = mod.widgets.NavigationTabButton {
        tooltip_text: #(crate::i18n::tr("All Rooms"))
        icon +: {
            draw_icon +: { svg: (ICON_HOME) }
        }
    }

    // The bottom rail item that opens the account menu (ported from robrix2). A plain
    // icon button, NOT a second ProfileIcon: the menu is an action, not the user's
    // avatar, and this keeps the rail's icon language consistent with Home and the tools.
    // The top avatar keeps opening Settings directly.
    mod.widgets.AccountSwitcherButton = mod.widgets.NavigationTabButton {
        tooltip_text: #(crate::i18n::tr("Account"))
        icon +: {
            draw_icon +: { svg: (mod.widgets.ICON_PEOPLE) }
        }
    }

    // Built on `NavigationTabButton` so it shares the size/padding and
    // hover animation. Its toggling is independent of navigation selection,
    // so the parent never calls `set_selected` on it.
    mod.widgets.ToggleSpacesBarButton = mod.widgets.NavigationTabButton {
        tooltip_text: "Toggle Spaces"
        icon +: {
            draw_icon +: { svg: (ICON_SQUARES) }
        }
    }

    mod.widgets.Separator = LineH {
        margin: Inset{top: (SPACE_SM), bottom: (SPACE_SM), left: (SPACE_MD), right: (SPACE_MD)}
        draw_bg.color: (RBX_NAV_DIVIDER)
    }

    mod.widgets.MobileTabButton = NavigationBarButton {
        width: Fill height: 56 flow: Down spacing: 4
        padding: Inset{top: 7 bottom: 4} align: Align{x: 0.5 y: 0.5}
        draw_bg +: {color_hover: #x00000000 color_active: #x00000000 border_radius: 0}
        icon := Icon {
            icon_walk: Walk{width: 24 height: 24}
            draw_icon.color: mod.widgets.RINX_INK
        }
        label := Label {
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (8 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }

    mod.widgets.NavigationTabBar = #(NavigationTabBar::register_widget(vm)) {
        // Dark navy anchor rail (robrix2 visual spec §2/§5.6). SolidView fills its
        // column edge-to-edge (no rounded-SDF anti-aliased border), so the navy is
        // perfectly flush to the window's left edge AND to the rooms list.
        Desktop := SolidView {
            new_batch: true,
            flow: Down,
            align: Align{x: 0.5}
            padding: Inset{
                top: (SPACE_SM),
                bottom: (SPACE_SM + mod.widgets.SAFE_INSET_PAD_BOTTOM),
                left: (mod.widgets.SAFE_INSET_PAD_LEFT),
            }
            width: (mod.widgets.NAVIGATION_TAB_BAR_SIZE + mod.widgets.SAFE_INSET_PAD_LEFT),
            height: Fill

            show_bg: true
            draw_bg.color: (RBX_NAV_BG)

            CachedWidget {
                profile_icon := mod.widgets.ProfileIcon {}
            }
            CachedWidget {
                home_button := mod.widgets.HomeButton {}
            }
            contacts_button := mod.widgets.NavigationTabButton {
                tooltip_text: #(crate::i18n::tr("Contacts"))
                icon.draw_icon.svg: ICON_PEOPLE
            }
            agent_button := mod.widgets.NavigationTabButton {
                tooltip_text: #(crate::i18n::tr("Agent"))
                spacing: 2
                icon +: {icon_walk: Walk{width: 22 height: 22} draw_icon.svg: ICON_ROBOT}
                label := Label {
                    text: #(crate::i18n::tr("Agent")) i18n_text: "Agent"
                    draw_text +: {color: RBX_NAV_FG text_style: theme.font_regular{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
                }
            }
            moments_button := mod.widgets.NavigationTabButton {
                tooltip_text: #(crate::i18n::tr("Moments"))
                icon.draw_icon.svg: ICON_GLOBE
            }
            octoscript_apps_button := mod.widgets.NavigationTabButton {
                tooltip_text: #(crate::i18n::tr("Mini apps"))
                icon.draw_icon.svg: ICON_SQUARES
            }
            // The one desktop entry to File Transfer (mobile: Discover).
            file_transfer_button := mod.widgets.NavigationTabButton {
                tooltip_text: #(crate::i18n::tr("File Transfer"))
                icon.draw_icon.svg: ICON_FILE
            }

            // The tools end at File Transfer, as far above this line as
            // Account sits below the lower one.
            top_separator := mod.widgets.Separator {}

            CachedWidget {
                root_spaces_bar := mod.widgets.SpacesBar {}
            }

            bottom_separator := mod.widgets.Separator {}

            // Bottom-left account menu button. The spaces bar above is height: Fill,
            // which pins this to the very bottom of the rail.
            CachedWidget {
                account_switcher_button := mod.widgets.AccountSwitcherButton {}
            }
        }

        Mobile := SolidView {
            new_batch: true flow: Right align: Align{y: 0.5}
            width: Fill height: (56 + mod.widgets.SAFE_INSET_PAD_BOTTOM)
            padding: Inset{bottom: (mod.widgets.SAFE_INSET_PAD_BOTTOM)}
            draw_bg.color: mod.widgets.RINX_PAGE
            chats_tab := mod.widgets.MobileTabButton {
                icon.draw_icon.svg: crate_resource("self://resources/icons/double_chat.svg")
                label.text: #(crate::i18n::tr("Chats")) label.i18n_text: "Chats"
            }
            contacts_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: ICON_PEOPLE label.text: #(crate::i18n::tr("Contacts")) label.i18n_text: "Contacts"}
            discover_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: ICON_GLOBE label.text: #(crate::i18n::tr("Discover")) label.i18n_text: "Discover"}
            me_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: crate_resource("self://resources/icons/person.svg") label.text: #(crate::i18n::tr("Me")) label.i18n_text: "Me"}
        }
    }
}

/// The icon in the NavigationTabBar that shows the user's avatar.
///
/// This widget serves as both the visual user-avatar indicator AND the
/// entry point to the SettingsScreen — clicking it opens settings and
/// marks this button as the currently-selected navigation tab.
///
/// `ProfileIcon` derefs into [`NavigationBarButton`], so it inherits the
/// hover/selected background animations and emits
/// `NavigationBarButtonAction::Clicked` on tap (handled by `NavigationTabBar`
/// to navigate to the Settings screen). Its dynamic tooltip (which includes
/// verification badge state) is emitted by this widget itself rather than
/// using `NavigationBarButton`'s built-in `tooltip_text`.
#[derive(Script, Widget)]
pub struct ProfileIcon {
    #[rust]
    appearance: ThemeSnapshot,
    #[deref]
    inner: NavigationBarButton,
    #[rust]
    own_profile: Option<UserProfile>,
}

impl ScriptHook for ProfileIcon {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        self.appearance = crate::theme::snapshot_for_vm(vm);
    }
    fn on_after_reload(&mut self, vm: &mut ScriptVm) {
        vm.with_cx_mut(|cx| {
            if self.own_profile.is_none() {
                self.own_profile = get_own_profile(cx);
            }
        });
    }
}

impl Widget for ProfileIcon {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.own_profile.is_none() {
            self.own_profile = get_own_profile(cx);
        }

        // A UI Signal indicates that a user profile or avatar may have been updated.
        if let Event::Signal = event {
            let mut needs_redraw = false;
            // Refetch our profile if we don't have it yet.
            if self.own_profile.is_none() {
                user_profile_cache::process_user_profile_updates(cx);
                self.own_profile = get_own_profile(cx);
                needs_redraw = true;
            }
            // If we're waiting for an avatar image, process avatar updates.
            if let Some(p) = self.own_profile.as_mut() && p.avatar_state.uri().is_some() {
                avatar_cache::process_avatar_updates(cx);
                let new_data = p.avatar_state.update_from_cache(cx);
                needs_redraw |= new_data.is_some();
                if new_data.is_some() {
                    user_profile_cache::enqueue_user_profile_update(
                        UserProfileUpdate::UserProfileOnly(p.clone())
                    );
                }
            }
            if needs_redraw {
                self.inner.redraw(cx);
            }
        }

        // Handle actions related to the currently-logged-in user account,
        // such as changing their avatar, display name, etc.
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(LoginAction::LoginSuccess) = action.downcast_ref() {
                    self.own_profile = get_own_profile(cx);
                    self.inner.redraw(cx);
                    continue;
                }

                if let Some(LogoutAction::ClearAppState { .. }) = action.downcast_ref() {
                    self.own_profile = None;
                    self.inner.redraw(cx);
                    continue;
                }

                // Handle account data changes (e.g., avatar updated/removed)
                match action.downcast_ref() {
                    Some(AccountDataAction::AvatarChanged(None)) => {
                        // Update both this widget's local profile info and the user profile cache.
                        if let Some(p) = self.own_profile.as_mut() {
                            p.avatar_state = AvatarState::Known(None);
                            user_profile_cache::enqueue_user_profile_update(
                                UserProfileUpdate::UserProfileOnly(p.clone())
                            );
                            self.inner.redraw(cx);
                        }
                        continue;
                    }
                    Some(AccountDataAction::AvatarChanged(Some(new_uri))) => {
                        if let Some(p) = self.own_profile.as_mut() {
                            p.avatar_state = AvatarState::Known(Some(new_uri.clone()));
                            p.avatar_state.update_from_cache(cx);
                            user_profile_cache::enqueue_user_profile_update(
                                UserProfileUpdate::UserProfileOnly(p.clone())
                            );
                            self.inner.redraw(cx);
                        }
                        continue;
                    }
                    Some(AccountDataAction::AvatarChangeFailed(_)) => {
                        // this is only handled in the account settings screen
                        continue;
                    }
                    Some(AccountDataAction::DisplayNameChanged(new_display_name)) => {
                        if let Some(p) = self.own_profile.as_mut() {
                            p.username = new_display_name.clone();
                            user_profile_cache::enqueue_user_profile_update(
                                UserProfileUpdate::UserProfileOnly(p.clone())
                            );
                            self.inner.redraw(cx);
                        }
                        continue;
                    }
                    Some(AccountDataAction::DisplayNameChangeFailed(_)) => {
                        // this is only handled in the account settings screen
                        continue;
                    }
                    _ => {}
                }
            }
        }

        // Forward to the inner NavigationBarButton, which handles hover/selected
        // animations and emits `NavigationBarButtonAction::Clicked` on tap.
        self.inner.handle_event(cx, event, scope);

        // Emit ProfileIcon's own dynamic tooltip (which includes verification
        // badge state). This is in addition to (not instead of) the inner
        // button's hit handling: calling `event.hits()` twice on the same area
        // is safe in Makepad — both calls return the same hit.
        let area = self.inner.view.area();
        match event.hits(cx, area) {
            Hit::FingerLongPress(_) | Hit::FingerHoverIn(_) => {
                let (verification_str, bg_color) = self.inner.view
                    .verification_badge(cx, ids!(verification_badge))
                    .tooltip_content();
                let text = self.own_profile.as_ref().map_or_else(
                    || String::from("Not logged in (or disconnected).\n\nClick/tap to access all settings."),
                    |p| crate::i18n::format("Logged in {verification_str}as \"{0}\".\n\nClick/tap to access all settings.", &[("verification_str", (verification_str).to_string()), ("0", (p.displayable_name()).to_string())]),
                );
                let mut options = CalloutTooltipOptions {
                    position: if effective_is_desktop(cx) { TooltipPosition::Right } else { TooltipPosition::Top },
                    ..Default::default()
                };
                if let Some(c) = bg_color {
                    options.bg_color = Some(c);
                }
                cx.widget_action(
                    self.widget_uid(),
                    TooltipAction::HoverIn {
                        text,
                        widget_rect: area.rect(cx),
                        options,
                    },
                );
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.widget_uid(),  TooltipAction::HoverOut);
            }
            _ => { }
        };
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let our_own_avatar = self.inner.view.avatar(cx, ids!(our_own_avatar));
        let Some(own_profile) = self.own_profile.as_ref() else {
            // If we don't have a profile, default to an unknown avatar.
            our_own_avatar.show_text(
                cx,
                Some(self.appearance.role("color.content.disabled")),
                None, // don't make this avatar clickable; we handle clicks on this ProfileIcon widget directly.
                "",
            );
            return self.inner.draw_walk(cx, scope, walk);
        };

        let mut drew_avatar = false;
        if let Some(avatar_image) = own_profile.avatar_state.image() {
            drew_avatar = our_own_avatar.show_image(
                cx,
                None, // don't make this avatar clickable; we handle clicks on this ProfileIcon widget directly.
                |cx, img| utils::load_avatar_image(&img, cx, avatar_image),
            ).is_ok();
        }
        if !drew_avatar {
            our_own_avatar.show_user_text(cx, &own_profile.user_id, own_profile.displayable_name());
        }

        self.inner.draw_walk(cx, scope, walk)
    }
}

impl ProfileIconRef {
    /// Visually marks this `ProfileIcon` as selected (or not).
    /// Forwards to [`NavigationBarButton::set_selected`].
    pub fn set_selected(&self, cx: &mut Cx, is_selected: bool) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.inner.set_selected(cx, is_selected);
    }
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

    #[rust] is_spaces_bar_shown: bool,

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

impl NavigationTabBar {
    /// Installs a variant selector on our root `AdaptiveView` that honors the
    /// given [`ViewModeOverride`] preference. `Automatic` falls back to the
    /// default width-based selector.
    fn apply_view_mode(&mut self, mode: ViewModeOverride) {
        self.view.set_variant_selector(mode.variant_selector());
        self.applied_view_mode = mode;
    }

    /// Updates which navigation tab button is visually marked as selected,
    /// enforcing mutual exclusion across all buttons (like a radio button group).
    ///
    /// If `tab` is `None`, the existing selection is re-applied without changing it.
    fn apply_selected_tab(&mut self, cx: &mut Cx, tab: Option<SelectedTab>) {
        if let Some(t) = tab {
            self.selected_tab = t;
        }
        let home    = self.view.navigation_bar_button(cx, ids!(home_button));
        let contacts = self.view.navigation_bar_button(cx, ids!(contacts_button));
        let profile = self.view.profile_icon(cx, ids!(profile_icon));
        // Explore Rooms keeps Home marked: on desktop it is opened from the chat
        // list's "New or Join" menu, so it belongs to Home (as mobile marks Discover).
        home.set_selected(cx, matches!(self.selected_tab, SelectedTab::Home | SelectedTab::AddRoom));
        contacts.set_selected(cx, self.selected_tab == SelectedTab::Contacts);
        profile.set_selected(cx, matches!(self.selected_tab, SelectedTab::Settings | SelectedTab::Me));
        let chats_uid = self.view.navigation_bar_button(cx, ids!(chats_tab)).widget_uid();
        let palette = (chats_uid, self.selected_tab.clone());
        let update_palette = self.mobile_palette.as_ref() != Some(&palette);
        for (id, selected) in [
            (ids!(chats_tab), matches!(self.selected_tab, SelectedTab::Home | SelectedTab::Space {..})),
            (ids!(contacts_tab), self.selected_tab == SelectedTab::Contacts),
            (ids!(discover_tab), matches!(self.selected_tab, SelectedTab::Discover | SelectedTab::AddRoom)),
            (ids!(me_tab), matches!(self.selected_tab, SelectedTab::Me | SelectedTab::Settings)),
        ] {
            let button = self.view.navigation_bar_button(cx, id);
            button.set_selected(cx, selected);
            if update_palette && !button.is_empty() {
                let color = if selected {
                    self.appearance.accent
                } else {
                    self.appearance.ink
                };
                let mut icon = self.view.icon(cx, &[id[0], id!(icon)]);
                script_apply_eval!(cx, icon, {draw_icon +: {color: #(color)}});
                self.view.label(cx, &[id[0], id!(label)]).set_text_color(cx, color);
            }
        }
        self.mobile_palette = Some(palette);
    }
}

impl NavigationTabBar {
    /// A permission request that waits for this account turns the Agent tab
    /// red, with a dot after its label, until the request is decided or gone.
    fn apply_approval_alert(&mut self, cx: &mut Cx) {
        self.apply_approval_alert_state(cx, crate::hagency::pending_approvals::total() > 0);
    }

    fn apply_approval_alert_state(&mut self, cx: &mut Cx, alert: bool) {
        let label = self.view.label(cx, ids!(agent_button.label));
        let mut icon = self.view.icon(cx, ids!(agent_button.icon));
        // AdaptiveView creates its desktop variant lazily; there is no Agent
        // icon in the mobile variant. Retry after the desktop widgets exist.
        if label.is_empty() || icon.is_empty() {
            self.approval_alert = None;
            return;
        }
        let text = if alert { format!("{} ●", crate::i18n::tr("Agent")) } else { crate::i18n::tr("Agent").to_owned() };
        if self.approval_alert == Some(alert) && label.text() == text {
            return;
        }
        self.approval_alert = Some(alert);
        let color = if alert { self.appearance.role("color.status.danger.foreground") } else { self.appearance.nav_ink() };
        script_apply_eval!(cx, icon, {draw_icon +: {color: #(color)}});
        label.set_text(cx, &text);
        label.set_text_color(cx, color);
        self.view.redraw(cx);
    }
}

impl Widget for NavigationTabBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            for (id, tab) in [
                (ids!(chats_tab), SelectedTab::Home),
                (ids!(contacts_tab), SelectedTab::Contacts),
                (ids!(contacts_button), SelectedTab::Contacts),
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

            if self.view.navigation_bar_button(cx, ids!(moments_button)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::Open {author: None});
            }
            if self.view.navigation_bar_button(cx,ids!(octoscript_apps_button)).clicked(actions) {cx.action(crate::octoscript_apps::MiniAppsAction::Open);}
            if self.view.navigation_bar_button(cx, ids!(agent_button)).clicked(actions) {
                cx.action(crate::octoscript_apps::MiniAppsAction::OpenAgent);
            }
            if self.view.navigation_bar_button(cx, ids!(file_transfer_button)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::FileTransfer);
            }
            if self.view.navigation_bar_button(cx, ids!(toggle_spaces_bar_button)).clicked(actions) {
                self.is_spaces_bar_shown = !self.is_spaces_bar_shown;
                cx.action(NavigationBarAction::ToggleSpacesBar);
            }

            for action in actions {
                // If another widget programmatically selected a new tab,
                // update our buttons' visual selection state accordingly.
                if let Some(NavigationBarAction::TabSelected(tab)) = action.downcast_ref() {
                    self.apply_selected_tab(cx, Some(tab.clone()));
                    continue;
                }

                // Upon login (mostly re-login), go back to the home tab
                // because the profile/settings tab will have been selected upon logout.
                if let Some(LoginAction::LoginSuccess) = action.downcast_ref() {
                    self.apply_selected_tab(cx, Some(SelectedTab::Home));
                    cx.action(NavigationBarAction::GoToHome);
                    continue;
                }

                if let Some(AppPreferencesAction::ViewModeChanged(new_mode)) = action.downcast_ref() {
                    if *new_mode != self.applied_view_mode {
                        self.apply_view_mode(*new_mode);
                        self.view.redraw(cx);
                    }
                }
            }
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
        // Every rail tooltip follows the interface language, including after a switch.
        for (id, tooltip) in [
            (ids!(home_button), crate::i18n::tr("All Rooms")),
            (ids!(contacts_button), crate::i18n::tr("Contacts")),
            (ids!(agent_button), crate::i18n::tr("Agent")),
            (ids!(moments_button), crate::i18n::tr("Moments")),
            (ids!(octoscript_apps_button), crate::i18n::tr("Mini apps")),
            (ids!(file_transfer_button), crate::i18n::tr("File Transfer")),
            (ids!(account_switcher_button), crate::i18n::tr("Account")),
        ] {
            if let Some(mut button) = self.view.navigation_bar_button(cx, id).borrow_mut() {
                button.set_tooltip_text(tooltip);
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


/// Returns the current user's profile and avatar, if available.
pub fn get_own_profile(cx: &mut Cx) -> Option<UserProfile> {
    let mut own_profile = None;
    if let Some(own_user_id) = current_user_id() {
        let avatar_uri_to_fetch = user_profile_cache::with_user_profile(
            cx,
            own_user_id,
            None,
            true,
            |new_profile, _rooms| {
                let avatar_uri_to_fetch = new_profile.avatar_state.uri().cloned();
                own_profile = Some(new_profile.clone());
                avatar_uri_to_fetch
            },
        );
        // If we have an avatar URI to fetch, try to fetch it.
        if let Some(Some(avatar_uri)) = avatar_uri_to_fetch {
            if let AvatarCacheEntry::Loaded(data) = avatar_cache::get_or_fetch_avatar(cx, &avatar_uri) {
                if let Some(p) = own_profile.as_mut() {
                    p.avatar_state = AvatarState::Loaded((avatar_uri.clone(), data).into());
                    // Update the user profile cache with the new avatar data.
                    user_profile_cache::enqueue_user_profile_update(
                        UserProfileUpdate::UserProfileOnly(p.clone())
                    );
                }
            }
        }
    }

    own_profile
}


// Registered before RoomsSideBar: this compact account row replaces the
// standalone desktop rail. The shared account menu owns account switching.
pub fn account_bar_script_mod(vm: &mut ScriptVm) {
    account_bar_design::script_mod(vm);
}

mod account_bar_design {
    use super::*;
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.HagencyAccountBar = #(HagencyAccountBar::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 8
        padding: Inset{left: 0 right: 0 top: 8 bottom: 0}
        account := NavigationBarButton {
            width: Fill height: 64 flow: Right spacing: 10
            padding: Inset{left: 8 right: 8 top: 6 bottom: 6}
            align: Align{y: 0.5}
            draw_bg +: {
                color_hover: mod.widgets.RINX_HOVER
                color_active: mod.widgets.RINX_FIELD
                border_radius: 7
            }
            avatar := Avatar {width: 30 height: 30}
            account_identity := View {
                width: Fill height: Fit flow: Down spacing: 3
                name := Label {
                    width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                user := Label {
                    width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                    draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (8 * mod.widgets.RINX_TEXT_SCALE)}}
                }
            }
            account_chevron := Label {text: "⌃" draw_text.color: mod.widgets.RINX_INK}
        }
    }
}

}

#[derive(Script, ScriptHook, Widget)]
pub struct HagencyAccountBar {
    #[deref] view: View,
}

impl Widget for HagencyAccountBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event
            && self.view.navigation_bar_button(cx, ids!(account)).clicked(actions)
        {
            let rect = self.view.widget(cx, ids!(account)).area().rect(cx);
            cx.action(AccountMenuAction::Open {
                pos: dvec2(rect.pos.x, rect.pos.y),
            });
        }
        if matches!(event, Event::Signal | Event::Actions(_)) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let collapsed = crate::home::home_screen::effective_is_desktop(cx)
            && cx.global::<crate::settings::app_preferences::AppPreferencesGlobal>().0.desktop_sidebar_collapsed;
        self.view.view(cx, ids!(account_identity)).set_visible(cx, !collapsed);
        self.view.label(cx, ids!(account_chevron)).set_visible(cx, !collapsed);
        self.view.layout.padding = if collapsed {Inset::default()} else {Inset{left:0.0,right:0.0,top:8.0,bottom:0.0}};
        if let Some(mut button) = self.view.navigation_bar_button(cx, ids!(account)).borrow_mut() {
            button.walk.height = Size::Fixed(if collapsed {44.0} else {64.0});
            button.layout.padding = if collapsed {Inset{left:7.0,right:7.0,top:7.0,bottom:7.0}} else {Inset{left:8.0,right:8.0,top:6.0,bottom:6.0}};
        }
        let profile = get_own_profile(cx);
        let account = current_user_id();
        let mxid = account.as_ref().map(|id| id.as_str()).unwrap_or("");
        let name = profile.as_ref().map(|p| p.displayable_name()).unwrap_or(mxid);
        if let Some(mut button) = self.view.navigation_bar_button(cx, ids!(account)).borrow_mut() {
            button.set_tooltip_text(if collapsed {format!("{name}\n{mxid}")} else {String::new()});
        }
        self.view.label(cx, ids!(name)).set_text(cx, name);
        self.view.label(cx, ids!(user)).set_text(cx, mxid);
        if let Some(id) = current_user_id() {
            self.view.avatar(cx, ids!(avatar)).show_user_text(cx, &id, name);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The desktop left rail, built and drawn offscreen.
#[cfg(test)]
mod rail_tests {
    use super::*;
    use crate::home::sidebar_action_menu::SidebarMenuAction;
    use crate::i18n::{text_for, Language};
    use crate::shared::navigation_bar_button::NavigationBarButtonAction;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    /// Every rail button below the avatar, top to bottom.
    const BUTTONS: [&[LiveId; 1]; 7] = [
        ids!(home_button), ids!(contacts_button), ids!(agent_button), ids!(moments_button), ids!(octoscript_apps_button),
        ids!(file_transfer_button), ids!(account_switcher_button),
    ];

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
        let event = DrawEvent::default();
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

    #[test]
    fn rail_ends_its_tools_at_file_transfer_without_new_or_join() {
        let (mut cx, mut bar) = rail();
        draw(&mut cx, &mut bar);
        assert_eq!(bar.view.active_variant(), Some(id!(Desktop)));
        // New or Join is the "+" beside the chat list's search field now.
        for gone in [ids!(new_or_join_button), ids!(add_room_button), ids!(appearance_button)] {
            assert!(bar.view.widget(&mut cx, gone).is_empty(), "{gone:?} is not on the rail");
        }

        // Mini apps, then File Transfer: full rail targets with tooltips in the
        // interface language (English in unit tests).
        let mut above = 0.0;
        for (button, english, chinese) in [
            (ids!(octoscript_apps_button), "Mini apps", "小应用"),
            (ids!(file_transfer_button), "File Transfer", "文件传输助手"),
        ] {
            let found = bar.view.navigation_bar_button(&mut cx, button);
            assert_eq!(found.borrow().expect("rail button exists").tooltip_text(), english);
            assert_eq!(text_for(Language::Chinese, english), chinese);
            let rect = found.area().rect(&cx);
            assert!(rect.size.x >= 44.0 && rect.size.y >= 44.0, "{english} is a full target: {rect:?}");
            assert!(rect.pos.y > above && rect.pos.x + rect.size.x <= 76.5, "{english} sits in order in the rail: {rect:?}");
            above = rect.pos.y;
        }

        // File Transfer opens the same private File Transfer as before; nothing else.
        let emitted = click(&mut cx, &mut bar, ids!(file_transfer_button));
        assert_eq!(emitted.iter().filter(|a| matches!(a.downcast_ref(), Some(crate::moments::ui::MomentsAction::FileTransfer))).count(), 1);
        assert!(!emitted.iter().any(|a| a.downcast_ref::<NavigationBarAction>().is_some() || a.downcast_ref::<SidebarMenuAction>().is_some()));
        // No rail button opens the New or Join menu any more.
        for button in BUTTONS {
            let emitted = click(&mut cx, &mut bar, button);
            assert!(!emitted.iter().any(|a| a.downcast_ref::<SidebarMenuAction>().is_some()), "{button:?} opens no sidebar menu");
        }
    }

    #[test]
    fn rail_groups_keep_even_spacing_without_new_or_join() {
        let (mut cx, mut bar) = rail();
        draw(&mut cx, &mut bar);
        // The tools sit 4 points apart, Home to File Transfer.
        let tools = [BUTTONS[0], BUTTONS[1], BUTTONS[2], BUTTONS[3], BUTTONS[4], BUTTONS[5]].map(|button| rect(&mut cx, &bar, button));
        for pair in tools.windows(2) {
            let gap = pair[1].pos.y - (pair[0].pos.y + pair[0].size.y);
            assert!((gap - 4.0).abs() < 0.5, "even steps through the tools: {pair:?}");
        }
        // File Transfer closes the group exactly as far above the upper line as
        // Account sits below the lower one, with no gap for removed shortcuts.
        let file_transfer = tools[5];
        let top = rect(&mut cx, &bar, ids!(top_separator));
        let bottom = rect(&mut cx, &bar, ids!(bottom_separator));
        let account = rect(&mut cx, &bar, ids!(account_switcher_button));
        let above_line = top.pos.y - (file_transfer.pos.y + file_transfer.size.y);
        let below_line = account.pos.y - (bottom.pos.y + bottom.size.y);
        assert!(above_line > 0.0 && (above_line - below_line).abs() < 0.5, "{above_line} above the upper line, {below_line} below the lower one");
        assert!(top.size.y > 0.0 && top.size.y < 4.0 && bottom.pos.y > top.pos.y, "two hairlines frame the spaces: {top:?} {bottom:?}");
    }

    #[test]
    fn short_windows_keep_every_rail_button_on_screen_and_at_least_44_points() {
        // A 600-point window still has room for full 64-point buttons; shorter
        // ones share the shortfall evenly instead of pushing Account off the rail.
        for (height, full) in [(800.0, true), (600.0, true), (560.0, false), (480.0, false)] {
            let (mut cx, mut bar) = rail();
            draw_at(&mut cx, &mut bar, height);
            let mut above = 0.0;
            for button in BUTTONS {
                let rect = rect(&mut cx, &bar, button);
                assert!(rect.size.y >= 44.0 && rect.size.x >= 44.0, "{height}: {button:?} {rect:?}");
                assert_eq!((rect.size.y - 64.0).abs() < 0.5, full, "{height}: {button:?} {rect:?}");
                assert!(rect.pos.y >= above && rect.pos.y + rect.size.y <= height, "{height}: {button:?} is on the rail: {rect:?}");
                above = rect.pos.y + rect.size.y;
            }
        }
        // Below that, targets stop at 44 points rather than shrinking further.
        let (mut cx, mut bar) = rail();
        draw_at(&mut cx, &mut bar, 400.0);
        for button in BUTTONS {
            let rect = rect(&mut cx, &bar, button);
            assert!((rect.size.y - 44.0).abs() < 0.5, "{button:?} {rect:?}");
        }
    }

    #[test]
    fn explore_rooms_keeps_all_rooms_marked() {
        let (mut cx, mut bar) = rail();
        draw(&mut cx, &mut bar);
        assert!(selected(&mut cx, &bar, ids!(home_button)));
        // The HomeScreen confirms each page. Explore Rooms is opened from the chat
        // list's New or Join menu, so All Rooms stays marked while it is open.
        for (tab, marked) in [
            (SelectedTab::AddRoom, ids!(home_button)),
            (SelectedTab::Contacts, ids!(contacts_button)),
            (SelectedTab::AddRoom, ids!(home_button)),
            (SelectedTab::Home, ids!(home_button)),
        ] {
            let confirmed = cx.capture_actions(|cx| cx.action(NavigationBarAction::TabSelected(tab.clone())));
            bar.handle_event(&mut cx, &Event::Actions(confirmed), &mut Scope::empty());
            for button in BUTTONS {
                assert_eq!(selected(&mut cx, &bar, button), button == marked, "{tab:?}: {button:?}");
            }
        }
    }
}
