//! The rail's avatar button: shows the signed-in user's avatar and opens Settings.
//! Split out of `navigation_tab_bar.rs` (R4) with no behavior change.

use crate::theme::Snapshot as ThemeSnapshot;
use makepad_widgets::*;
use super::get_own_profile;
use crate::{
    avatar_cache,
    login::login_screen::LoginAction,
    logout::logout_confirm_modal::LogoutAction,
    profile::{
        user_profile::UserProfile,
        user_profile_cache::{self, UserProfileUpdate},
    },
    home::home_screen::effective_is_desktop,
    shared::{
        avatar::{AvatarState, AvatarWidgetExt},
        navigation_bar_button::NavigationBarButton,
        verification_badge::VerificationBadgeWidgetExt,
    },
    sliding_sync::AccountDataAction,
    utils,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

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
    pub inner: NavigationBarButton,
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
                    // Changed on another device: this copy only knew this device's changes.
                    Some(AccountDataAction::ProfileChangedElsewhere { avatar_url, display_name }) => {
                        if let Some(p) = self.own_profile.as_mut() {
                            let mut changed = false;
                            if p.avatar_state.uri() != avatar_url.as_ref() {
                                p.avatar_state = AvatarState::Known(avatar_url.clone());
                                p.avatar_state.update_from_cache(cx);
                                changed = true;
                            }
                            if p.username != *display_name {
                                p.username = display_name.clone();
                                changed = true;
                            }
                            if changed {
                                user_profile_cache::enqueue_user_profile_update(
                                    UserProfileUpdate::UserProfileOnly(p.clone())
                                );
                                self.inner.redraw(cx);
                            }
                        }
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
