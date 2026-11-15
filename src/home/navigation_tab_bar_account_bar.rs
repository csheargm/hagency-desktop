//! The compact account row (avatar, name, user ID) that opens the account menu.
//! Split out of `navigation_tab_bar.rs` (R4) with no behavior change.

use makepad_widgets::*;
use crate::{
    avatar_cache::{self, AvatarCacheEntry},
    home::account_menu::AccountMenuAction,
    profile::{user_profile::UserProfile, user_profile_cache::{self, UserProfileUpdate}},
    shared::{avatar::{AvatarState, AvatarWidgetExt}, navigation_bar_button::NavigationBarButtonWidgetExt},
    sliding_sync::current_user_id,
};

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
