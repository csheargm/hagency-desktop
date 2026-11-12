//! A cache of whether we've verified other users, indexed by user ID.
//!
//! The cache is only accessible from the main UI thread.

use crossbeam_queue::SegQueue;
use makepad_widgets::{Cx, SignalToUI};
use matrix_sdk::{
    encryption::identities::UserIdentity,
    ruma::{OwnedUserId, UserId},
};
use std::{cell::RefCell, collections::BTreeMap};

use crate::{
    matrix_context::SessionPin,
    sliding_sync::{submit_async_request, MatrixRequest},
};

thread_local! {
    /// Whether we've verified each user that the UI has asked about.
    /// The key is a user ID,
    /// and the value is that user's verification state, or `None` while it's being fetched.
    static USER_VERIFICATION_CACHE: RefCell<BTreeMap<OwnedUserId, Option<UserVerificationState>>> = const { RefCell::new(BTreeMap::new()) };
    static CACHE_SESSION: RefCell<Option<SessionPin>> = const { RefCell::new(None) };
}

/// Retire already-rendered trust as well as pending updates on SDK replacement,
/// including a re-login with the very same Matrix user and device IDs.
fn refresh_cache_session() -> bool {
    let current = crate::sliding_sync::get_client()
        .and_then(|client| SessionPin::capture(&client))
        .filter(SessionPin::is_current);
    CACHE_SESSION.with_borrow_mut(|session| {
        if *session == current {
            return false;
        }
        *session = current;
        USER_VERIFICATION_CACHE.with_borrow_mut(|cache| cache.clear());
        true
    })
}

/// Whether we've verified a user's cryptographic identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserVerificationState {
    /// We haven't verified this user, or they haven't set up cross-signing.
    Unverified,
    Verified,
    /// We verified this user before, but they've since reset their identity.
    VerificationViolation,
}
impl From<&UserIdentity> for UserVerificationState {
    fn from(identity: &UserIdentity) -> Self {
        if identity.is_verified() {
            Self::Verified
        } else if identity.has_verification_violation() {
            Self::VerificationViolation
        } else {
            Self::Unverified
        }
    }
}

/// Returns the given user's verification state if it's cached,
/// otherwise fetches it in the background and returns `None`.
pub fn get_or_fetch_user_verification_state(
    _cx: &mut Cx,
    user_id: &UserId,
) -> Option<UserVerificationState> {
    refresh_cache_session();
    if CACHE_SESSION.with_borrow(Option::is_none) {
        return None;
    }
    USER_VERIFICATION_CACHE.with_borrow_mut(|cache| {
        if let Some(state) = cache.get(user_id) {
            return *state;
        }
        cache.insert(user_id.to_owned(), None);
        submit_async_request(MatrixRequest::GetUserVerificationState {
            user_id: user_id.to_owned(),
        });
        None
    })
}

/// The queue of verification state updates waiting to be processed by the UI thread's event handler.
static PENDING_USER_VERIFICATION_UPDATES: SegQueue<(SessionPin, UserVerificationUpdate)> =
    SegQueue::new();

/// Enqueues a new verification state update and signals the UI that an update is available.
pub(crate) fn enqueue_user_verification_update(
    session: SessionPin,
    update: UserVerificationUpdate,
) {
    PENDING_USER_VERIFICATION_UPDATES.push((session, update));
    SignalToUI::set_ui_signal();
}

pub enum UserVerificationUpdate {
    /// The latest verification states of these users.
    States(Vec<(OwnedUserId, UserVerificationState)>),
    /// Our own identity changed, which can change whether we've verified anyone else.
    OwnIdentityChanged,
}

/// Emitted after users' verification states were updated in the cache,
/// so that every widget showing those users can redraw them.
///
/// This is NOT a widget action.
#[derive(Debug)]
pub struct UserVerificationStatesUpdated;

/// Processes all pending verification state updates,
/// emitting [`UserVerificationStatesUpdated`] if there were any.
///
/// The App calls this upon every Signal, before any widget handles that Signal.
pub fn process_user_verification_updates(cx: &mut Cx) {
    let mut has_updates = refresh_cache_session();
    USER_VERIFICATION_CACHE.with_borrow_mut(|cache| {
        while let Some((session, update)) = PENDING_USER_VERIFICATION_UPDATES.pop() {
            if session.is_current() {
                apply_update(cache, update);
                has_updates = true;
            }
        }
    });
    if has_updates {
        cx.action(UserVerificationStatesUpdated);
    }
}

/// Clears the cache of users' verification states.
pub fn clear_user_verification_cache(_cx: &mut Cx) {
    while PENDING_USER_VERIFICATION_UPDATES.pop().is_some() {}
    USER_VERIFICATION_CACHE.with_borrow_mut(|cache| cache.clear());
    CACHE_SESSION.with_borrow_mut(|session| *session = None);
}

fn apply_update(
    cache: &mut BTreeMap<OwnedUserId, Option<UserVerificationState>>,
    update: UserVerificationUpdate,
) {
    match update {
        UserVerificationUpdate::States(states) => {
            cache.extend(states.into_iter().map(|(user, state)| (user, Some(state))))
        }
        UserVerificationUpdate::OwnIdentityChanged => cache.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_reset_replaces_trust() {
        let user = ruma::user_id!("@bob:test").to_owned();
        let mut cache = BTreeMap::new();
        let update = |state| UserVerificationUpdate::States(vec![(user.clone(), state)]);
        apply_update(&mut cache, update(UserVerificationState::Verified));
        assert_eq!(cache[&user], Some(UserVerificationState::Verified));
        apply_update(
            &mut cache,
            update(UserVerificationState::VerificationViolation),
        );
        assert_eq!(
            cache[&user],
            Some(UserVerificationState::VerificationViolation)
        );
        apply_update(&mut cache, UserVerificationUpdate::OwnIdentityChanged);
        assert!(cache.is_empty());
    }

    #[test]
    fn same_account_relogin_drops_cached_trust_and_late_identity_updates() {
        const NAME: &str = "profile::user_verification_cache::tests::same_account_relogin_drops_cached_trust_and_late_identity_updates";
        if std::env::var("HAGENCY_TRUST_CACHE_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_TRUST_CACHE_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        use crate::settings::crypto_settings::test_support::{Server, install_link_client};
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let server = Server::with_handler(false, std::sync::Arc::new(|_| None));
            let client = server.room().await.client();
            install_link_client(Some(client.clone()));
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let user = ruma::user_id!("@bob:test").to_owned();
            let update = |state| UserVerificationUpdate::States(vec![(user.clone(), state)]);
            let old = SessionPin::capture(&client).unwrap();
            enqueue_user_verification_update(old.clone(), update(UserVerificationState::Verified));
            process_user_verification_updates(&mut cx);
            USER_VERIFICATION_CACHE.with_borrow(|cache| {
                assert_eq!(cache[&user], Some(UserVerificationState::Verified))
            });

            // Replacing even this exact SDK instance revokes the old pin.
            install_link_client(Some(client.clone()));
            enqueue_user_verification_update(old, update(UserVerificationState::Verified));
            process_user_verification_updates(&mut cx);
            USER_VERIFICATION_CACHE.with_borrow(|cache| assert!(cache.is_empty()));

            enqueue_user_verification_update(
                SessionPin::capture(&client).unwrap(),
                update(UserVerificationState::VerificationViolation),
            );
            process_user_verification_updates(&mut cx);
            USER_VERIFICATION_CACHE.with_borrow(|cache| {
                assert_eq!(
                    cache[&user],
                    Some(UserVerificationState::VerificationViolation)
                )
            });
            install_link_client(None);
            process_user_verification_updates(&mut cx);
            USER_VERIFICATION_CACHE.with_borrow(|cache| assert!(cache.is_empty()));
        });
    }
}
