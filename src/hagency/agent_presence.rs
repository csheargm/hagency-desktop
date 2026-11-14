//! Whether this account has at least one Agent, as last listed. First-run
//! guidance (the welcome call-to-action, the rail dot) keys on it; it is a
//! hint for the UI, never an authorization.
use std::sync::atomic::{AtomicU8, Ordering};

const UNKNOWN: u8 = 0;
const NONE: u8 = 1;
const SOME: u8 = 2;
static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Record the latest Agents listing for the signed-in account.
pub fn set_has_agent(has_agent: bool) {
    STATE.store(if has_agent { SOME } else { NONE }, Ordering::Relaxed);
}
/// Bumped by `reset`, so a listing started for the previous account is ignored.
static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Forget the listing (account changed, signed out) and allow a fresh one.
pub fn reset() {
    EPOCH.fetch_add(1, Ordering::Relaxed);
    STATE.store(UNKNOWN, Ordering::Relaxed);
    REQUESTED.store(false, Ordering::Relaxed);
}
/// Record a listing only if no account change happened since it started.
fn settle(epoch: u64, has_agent: bool) {
    if EPOCH.load(Ordering::Relaxed) == epoch { set_has_agent(has_agent); }
}
/// `Some(true)` once an Agent was listed, `Some(false)` once a listing came
/// back empty, `None` before any listing.
pub fn has_agent() -> Option<bool> {
    match STATE.load(Ordering::Relaxed) {
        SOME => Some(true),
        NONE => Some(false),
        _ => None,
    }
}
/// First-run guidance shows only when the account is known to have no Agent.
pub fn needs_first_agent() -> bool {
    has_agent() == Some(false)
}

/// First run for guidance purposes: the account is not known to have an Agent.
pub fn first_run() -> bool {
    has_agent() != Some(true)
}

static REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Whether a listing should be started now: once, and only while unknown.
pub fn should_request(has_agent: Option<bool>) -> bool {
    has_agent.is_none() && !REQUESTED.swap(true, Ordering::Relaxed)
}
/// Ask the owner engine for the Agents once, so screens that never open the
/// Agent panel (a brand-new account on the welcome screen) still learn whether
/// an Agent exists. The listing's reply sets the flag like the panel does.
pub fn refresh() {
    if !should_request(has_agent()) {
        return;
    }
    let epoch = EPOCH.load(Ordering::Relaxed);
    crate::sliding_sync::spawn_async_task(async move {
        match crate::hagency::backend::Service::for_current() {
            Ok(service) => match service.execute(::hagency::native_owner::Command::Agents).await {
                Ok(listed) => settle(epoch, listed["agents"].as_array().is_some_and(|a| !a.is_empty())),
                Err(code) => {
                    // Unknown stays unknown; the welcome screen keeps offering
                    // the Agent and the panel's own listing will settle it.
                    makepad_widgets::log!("Agent presence listing failed: {code}");
                    REQUESTED.store(false, Ordering::Relaxed);
                }
            },
            Err(code) => { makepad_widgets::log!("Agent presence: no owner service yet: {code}"); REQUESTED.store(false, Ordering::Relaxed); }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The flags are process-wide; tests touching them take turns.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn guidance_only_after_an_empty_listing() {
        let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert_eq!(has_agent(), None);
        assert!(!needs_first_agent());
        set_has_agent(false);
        assert!(needs_first_agent());
        set_has_agent(true);
        assert!(!needs_first_agent());
        assert!(!first_run());
        reset();
        assert!(first_run());
    }
    #[test]
    fn account_switch_forgets_presence_and_ignores_the_old_listing() {
        let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_has_agent(true);
        REQUESTED.store(true, Ordering::Relaxed);
        let old = EPOCH.load(Ordering::Relaxed);
        reset();
        assert_eq!(has_agent(), None);
        assert!(should_request(None), "the new account lists again");
        settle(old, true);
        assert_eq!(has_agent(), None, "late answer for the old account");
    }
    #[test]
    fn listing_is_requested_once_and_only_while_unknown() {
        let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        REQUESTED.store(false, Ordering::Relaxed);
        assert!(should_request(None));
        assert!(!should_request(None));
        REQUESTED.store(false, Ordering::Relaxed);
        assert!(!should_request(Some(true)));
        assert!(!should_request(Some(false)));
    }
}
