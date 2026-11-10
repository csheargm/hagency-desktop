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
    sliding_sync::{submit_async_request, MatrixRequest},
};

thread_local! {
    /// Whether we've verified each user that the UI has asked about.
    /// The key is a user ID,
    /// and the value is that user's verification state, or `None` while it's being fetched.
    static USER_VERIFICATION_CACHE: RefCell<BTreeMap<OwnedUserId, Option<UserVerificationState>>> = const { RefCell::new(BTreeMap::new()) };
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
static PENDING_USER_VERIFICATION_UPDATES: SegQueue<(u64, UserVerificationUpdate)> = SegQueue::new();

/// Enqueues a new verification state update and signals the UI that an update is available.
pub fn enqueue_user_verification_update(epoch: u64, update: UserVerificationUpdate) {
    PENDING_USER_VERIFICATION_UPDATES.push((epoch, update));
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
    let mut has_updates = false;
    USER_VERIFICATION_CACHE.with_borrow_mut(|cache| {
        while let Some((epoch, update)) = PENDING_USER_VERIFICATION_UPDATES.pop() {
            has_updates |= apply_update(cache, crate::account_session::epoch(), epoch, update);
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
}

fn apply_update(
    cache: &mut BTreeMap<OwnedUserId, Option<UserVerificationState>>,
    current_epoch: u64,
    update_epoch: u64,
    update: UserVerificationUpdate,
) -> bool {
    if current_epoch != update_epoch {
        return false;
    }
    match update {
        UserVerificationUpdate::States(states) => {
            cache.extend(states.into_iter().map(|(user, state)| (user, Some(state))))
        }
        UserVerificationUpdate::OwnIdentityChanged => cache.clear(),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_reset_replaces_trust_but_old_account_updates_are_ignored() {
        let user = ruma::user_id!("@bob:test").to_owned();
        let mut cache = BTreeMap::new();
        let update = |state| UserVerificationUpdate::States(vec![(user.clone(), state)]);
        assert!(apply_update(
            &mut cache,
            10,
            10,
            update(UserVerificationState::Verified)
        ));
        assert!(!apply_update(
            &mut cache,
            10,
            9,
            update(UserVerificationState::Unverified)
        ));
        assert_eq!(cache[&user], Some(UserVerificationState::Verified));
        apply_update(
            &mut cache,
            10,
            10,
            update(UserVerificationState::VerificationViolation),
        );
        assert_eq!(
            cache[&user],
            Some(UserVerificationState::VerificationViolation)
        );
        apply_update(
            &mut cache,
            10,
            10,
            UserVerificationUpdate::OwnIdentityChanged,
        );
        assert!(cache.is_empty());
        assert!(!apply_update(
            &mut cache,
            11,
            10,
            update(UserVerificationState::Verified)
        ));
        assert!(cache.is_empty());
    }
}
