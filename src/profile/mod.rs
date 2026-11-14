use makepad_widgets::ScriptVm;

pub mod member_summary;
pub mod profile_moderation;
pub mod profile_route;
pub mod profile_side_panel;
pub mod profile_view_fill;
pub mod user_profile;
pub mod user_profile_cache;
#[cfg(test)]
pub(crate) mod search_test_server;

pub fn script_mod(vm: &mut ScriptVm) {
    // The side panel embeds the pane's `UserProfileView`: register it first.
    user_profile::script_mod(vm);
    profile_side_panel::script_mod(vm);
}

pub mod user_verification_cache;
