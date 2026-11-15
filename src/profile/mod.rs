use makepad_widgets::ScriptVm;

pub mod member_summary;
pub mod profile_moderation;
pub mod profile_route;
pub mod profile_side_panel;
pub mod profile_view_fill;
pub mod own_profile_watch;
pub mod rooms_in_common;
pub mod user_profile;
pub mod user_profile_cache;
pub mod user_rooms;
#[cfg(test)]
pub(crate) mod search_test_server;

pub fn script_mod(vm: &mut ScriptVm) {
    // The profile view embeds the rooms-in-common list: register it first.
    user_rooms::script_mod(vm);
    // The side panel embeds the pane's `UserProfileView`: register it first.
    user_profile::script_mod(vm);
    profile_side_panel::script_mod(vm);
}

pub mod user_verification_cache;
