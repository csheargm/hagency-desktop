use makepad_widgets::ScriptVm;

pub mod user_profile;
pub mod user_profile_cache;
#[cfg(test)]
pub(crate) mod search_test_server;

pub fn script_mod(vm: &mut ScriptVm) {
    user_profile::script_mod(vm);
}

pub mod user_verification_cache;
