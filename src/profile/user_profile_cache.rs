//! A cache of user profiles and room membership info, indexed by user ID.
//!
//! The cache is only accessible from the main UI thread.

use crossbeam_queue::SegQueue;
use makepad_widgets::{warning, Cx, SignalToUI};
use matrix_sdk::{room::RoomMember, ruma::{OwnedRoomId, OwnedUserId, UserId}};
use std::{cell::RefCell, collections::{btree_map::Entry, BTreeMap}};

use crate::{shared::avatar::AvatarState, sliding_sync::{submit_async_request, MatrixRequest}};

use super::user_profile::UserProfile;

thread_local! {
    /// A cache of each user's profile and the rooms they are a member of, indexed by user ID.
    ///
    /// To be of any use, this cache must only be accessed by the main UI thread.
    static USER_PROFILE_CACHE: RefCell<BTreeMap<OwnedUserId, UserProfileCacheEntry>> = const { RefCell::new(BTreeMap::new()) };
}
enum UserProfileCacheEntry {
    /// A request has been issued and we're waiting for it to complete.
    Requested,
    /// A completed failure; retry when sync recovers instead of on every draw.
    Failed,
    /// The profile has been successfully loaded from the server.
    Loaded {
        user_profile: UserProfile,
        rooms: BTreeMap<OwnedRoomId, RoomMemberEntry>,
    },
}

/// An entry in a loaded profile's map of per-room member info.
pub enum RoomMemberEntry {
    /// A request has been issued and we're waiting for it to complete.
    Requested,
    /// The room member info has been successfully loaded from the server.
    Loaded(RoomMember),
    /// The request completed but didn't return any member info.
    /// This means that the user isn't in that room, or the lookup failed.
    ///
    /// This won't be retried until the next transition from offline --> online.
    Failed,
}
impl RoomMemberEntry {
    /// Returns the loaded room member info, if any.
    pub fn loaded(&self) -> Option<&RoomMember> {
        match self {
            RoomMemberEntry::Loaded(member) => Some(member),
            RoomMemberEntry::Requested | RoomMemberEntry::Failed => None,
        }
    }
}

/// Removes all `Requested` and `Failed` entries from the cache, allowing them to be re-fetched.
///
/// This should be called when the app transitions from offline back to online,
/// because any in-flight requests that were submitted while offline have likely
/// failed without updating the cache, leaving stale `Requested` entries that
/// permanently block re-fetching.
pub fn clear_all_pending_requests() {
    USER_PROFILE_CACHE.with_borrow_mut(|cache| {
        cache.retain(|_, entry| matches!(entry, UserProfileCacheEntry::Loaded { .. }));
        for entry in cache.values_mut() {
            if let UserProfileCacheEntry::Loaded { rooms, .. } = entry {
                rooms.retain(|_, member| matches!(member, RoomMemberEntry::Loaded(_)));
            }
        }
    });
}

/// The queue of user profile updates waiting to be processed by the UI thread's event handler.
static PENDING_USER_PROFILE_UPDATES: SegQueue<UserProfileUpdate> = SegQueue::new();

/// Enqueues a new user profile update and signals the UI that an update is available.
pub fn enqueue_user_profile_update(update: UserProfileUpdate) {
    PENDING_USER_PROFILE_UPDATES.push(update);
    SignalToUI::set_ui_signal();
}

/// A user profile update, which can include changes to a user's full profile
/// and/or room membership info.
pub enum UserProfileUpdate {
    /// A fully-fetched user profile, with info about the user's membership in a given room.
    Full {
        new_profile: UserProfile,
        room_id: OwnedRoomId,
        room_member: RoomMember,
    },
    /// An update to the user's room membership info only, without any profile changes.
    RoomMemberOnly {
        room_id: OwnedRoomId,
        room_member: RoomMember,
    },
    /// An update to the user's profile only, without changes to room membership info.
    UserProfileOnly(UserProfile),
    /// The global profile request completed without usable profile data.
    UserProfileFailed { user_id: OwnedUserId },
    /// A room-specific user profile request failed, meaning the user isn't in that room.
    RoomMemberFailed {
        user_id: OwnedUserId,
        room_id: OwnedRoomId,
    },
}
impl UserProfileUpdate {
    pub fn from_fetch_error(user_id: OwnedUserId, error: &matrix_sdk::Error) -> Self {
        use matrix_sdk::ruma::api::error::ErrorKind;
        if matches!(error.client_api_error_kind(), Some(ErrorKind::NotFound)) {
            // Some homeservers return 404 when no display name or avatar was set.
            Self::UserProfileOnly(UserProfile {
                user_id, username: None, avatar_state: AvatarState::Known(None),
            })
        } else {
            Self::UserProfileFailed { user_id }
        }
    }

    /// Returns the user ID associated with this update.
    #[allow(unused)]
    pub fn user_id(&self) -> &UserId {
        match self {
            UserProfileUpdate::Full { new_profile, .. } => &new_profile.user_id,
            UserProfileUpdate::RoomMemberOnly { room_member, .. } => room_member.user_id(),
            UserProfileUpdate::UserProfileOnly(profile) => &profile.user_id,
            UserProfileUpdate::RoomMemberFailed { user_id, .. }
            | UserProfileUpdate::UserProfileFailed { user_id } => user_id,
        }
    }

    /// Applies this update to the given user profile info cache.
    fn apply_to_cache(self, cache: &mut BTreeMap<OwnedUserId, UserProfileCacheEntry>) {
        match self {
            UserProfileUpdate::Full { new_profile, room_id, room_member } => {
                match cache.entry(new_profile.user_id.clone()) {
                    Entry::Occupied(mut entry) => match entry.get_mut() {
                        e @ (UserProfileCacheEntry::Requested | UserProfileCacheEntry::Failed) => {
                            *e = UserProfileCacheEntry::Loaded {
                                user_profile: new_profile,
                                rooms: {
                                    let mut room_members_map = BTreeMap::new();
                                    room_members_map.insert(room_id, RoomMemberEntry::Loaded(room_member));
                                    room_members_map
                                },
                            };
                        }
                        UserProfileCacheEntry::Loaded { user_profile, rooms } => {
                            *user_profile = new_profile;
                            rooms.insert(room_id, RoomMemberEntry::Loaded(room_member));
                        }
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(UserProfileCacheEntry::Loaded {
                            user_profile: new_profile,
                            rooms: {
                                let mut room_members_map = BTreeMap::new();
                                room_members_map.insert(room_id, RoomMemberEntry::Loaded(room_member));
                                room_members_map
                            },
                        });
                    }
                }
            }
            UserProfileUpdate::RoomMemberOnly { room_id, room_member } => {
                match cache.entry(room_member.user_id().to_owned()) {
                    Entry::Occupied(mut entry) => match entry.get_mut() {
                        e @ (UserProfileCacheEntry::Requested | UserProfileCacheEntry::Failed) => {
                            // A member fetch can complete while the global profile is unavailable.
                            *e = UserProfileCacheEntry::Loaded {
                                user_profile: UserProfile {
                                    user_id: room_member.user_id().to_owned(),
                                    username: None,
                                    avatar_state: AvatarState::Known(room_member.avatar_url().map(|url| url.to_owned())),
                                },
                                rooms: {
                                    let mut room_members_map = BTreeMap::new();
                                    room_members_map.insert(room_id, RoomMemberEntry::Loaded(room_member));
                                    room_members_map
                                },
                            };
                        }
                        UserProfileCacheEntry::Loaded { rooms, .. } => {
                            rooms.insert(room_id, RoomMemberEntry::Loaded(room_member));
                        }
                    }
                    Entry::Vacant(entry) => {
                        // This shouldn't happen, but we can still technically handle it correctly.
                        warning!("BUG: User profile cache entry not found for user {} when handling RoomMemberOnly update", room_member.user_id());
                        entry.insert(UserProfileCacheEntry::Loaded {
                            user_profile: UserProfile {
                                user_id: room_member.user_id().to_owned(),
                                username: None,
                                avatar_state: AvatarState::Known(room_member.avatar_url().map(|url| url.to_owned())),
                            },
                            rooms: {
                                let mut room_members_map = BTreeMap::new();
                                room_members_map.insert(room_id, RoomMemberEntry::Loaded(room_member));
                                room_members_map
                            },
                        });
                    }
                }
            }
            UserProfileUpdate::UserProfileOnly(new_profile) => {
                match cache.entry(new_profile.user_id.clone()) {
                    Entry::Occupied(mut entry) => match entry.get_mut() {
                        e @ (UserProfileCacheEntry::Requested | UserProfileCacheEntry::Failed) => {
                            *e = UserProfileCacheEntry::Loaded {
                                user_profile: new_profile,
                                rooms: BTreeMap::new(),
                            };
                        }
                        UserProfileCacheEntry::Loaded { user_profile, .. } => {
                            *user_profile = new_profile;
                        }
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(UserProfileCacheEntry::Loaded {
                            user_profile: new_profile,
                            rooms: BTreeMap::new(),
                        });
                    }
                }
            }
            UserProfileUpdate::UserProfileFailed { user_id } => {
                // An older failed request must not erase a successfully loaded profile.
                if let Some(entry @ UserProfileCacheEntry::Requested) = cache.get_mut(&user_id) {
                    *entry = UserProfileCacheEntry::Failed;
                }
            }
            UserProfileUpdate::RoomMemberFailed { user_id, room_id } => {
                if let Some(UserProfileCacheEntry::Loaded { rooms, .. }) = cache.get_mut(&user_id) {
                    // Don't overwrite actual member profile data that does exist.
                    let member = rooms.entry(room_id).or_insert(RoomMemberEntry::Failed);
                    if matches!(member, RoomMemberEntry::Requested) {
                        *member = RoomMemberEntry::Failed;
                    }
                }
            }
        }
    }
}

/// Processes all pending user profile updates in the queue.
///
/// This function requires passing in a reference to `Cx`,
/// which isn't used, but acts as a guarantee that this function
/// must only be called by the main UI thread.
pub fn process_user_profile_updates(_cx: &mut Cx) {
    USER_PROFILE_CACHE.with_borrow_mut(|cache| {
        while let Some(update) = PENDING_USER_PROFILE_UPDATES.pop() {
            // Insert the updated info into the cache
            update.apply_to_cache(cache);
        }
    });
}

/// Invokes the given closure with cached user profile info for the given user ID
/// (optionally in the given room) if it exists in the cache, otherwise does nothing.
///
/// This function requires passing in a reference to `Cx`,
/// which isn't used, but acts as a guarantee that this function
/// must only be called by the main UI thread.
pub fn with_user_profile<F, R>(
    _cx: &mut Cx,
    user_id: OwnedUserId,
    room_id: Option<&OwnedRoomId>,
    fetch_if_missing: bool,
    f: F,
) -> Option<R>
where
    F: FnOnce(&UserProfile, &BTreeMap<OwnedRoomId, RoomMemberEntry>) -> R,
{
    USER_PROFILE_CACHE.with_borrow_mut(|cache|
        match cache.entry(user_id) {
            Entry::Occupied(mut entry) => {
                let user_id = entry.key().clone();
                match entry.get_mut() {
                    UserProfileCacheEntry::Loaded { user_profile, rooms } => {
                        if let Some(id) = room_id.filter(|_| fetch_if_missing) {
                            if let Entry::Vacant(room_entry) = rooms.entry(id.clone()) {
                                room_entry.insert(RoomMemberEntry::Requested);
                                submit_async_request(MatrixRequest::GetUserProfile {
                                    user_id,
                                    room_id: Some(id.clone()),
                                    local_only: false,
                                });
                            }
                        }
                        Some(f(user_profile, rooms))
                    }
                    UserProfileCacheEntry::Requested | UserProfileCacheEntry::Failed => {
                        // log!("User {} profile request is already in flight....", entry.key());
                        None
                    }
                }
            }
            Entry::Vacant(entry) => {
                if fetch_if_missing {
                    // log!("Did not find User {} in cache, fetching from server.", entry.key());
                    // TODO: use the extra `via` parameters from `matrix_to_uri.via()`.
                    submit_async_request(MatrixRequest::GetUserProfile {
                        user_id: entry.key().clone(),
                        room_id: room_id.cloned(),
                        local_only: false,
                    });
                    entry.insert(UserProfileCacheEntry::Requested);
                }
                None
            }
        }
    )
}


/// Returns the given user's displayable name (optionally in the given room),
/// using the user's account-wide displayable name as a fallback.
///
/// If either the `user_id` or `room_id` wasn't found in the cache,
/// and if `fetch_if_missing` is true, then this function will submit a request
/// to asynchronously fetch the user's room membership info from the server.
///
/// This function requires passing in a reference to `Cx`,
/// which isn't used, but acts as a guarantee that this function
/// must only be called by the main UI thread.
pub fn get_user_display_name_for_room(
    cx: &mut Cx,
    user_id: OwnedUserId,
    room_id: Option<&OwnedRoomId>,
    fetch_if_missing: bool,
) -> CachedName {
    let opt = with_user_profile(cx, user_id, room_id, fetch_if_missing, |profile, rooms| {
        room_id.and_then(|id| rooms.get(id)).and_then(RoomMemberEntry::loaded).map_or_else(
            || CachedName::FoundInProfile(profile.username.clone()),
            |rm| CachedName::FoundInRoom(rm.display_name().map(|n| n.to_owned())),
        )
    });
    opt.unwrap_or(CachedName::NotFound)
}

/// A user's display name in our cache.
pub enum CachedName {
    /// The user's display name was found for the specified room (most accurate).
    /// If `None`, they did not set a display name for that room.
    FoundInRoom(Option<String>),
    /// The user's display name was found in their general account profile.
    /// If `None`, they have not set a display name at all.
    FoundInProfile(Option<String>),
    /// No info about the user was found in the cache.
    NotFound,
}
impl CachedName {
    pub fn was_found(&self) -> bool {
        matches!(self, Self::FoundInRoom(_) | Self::FoundInProfile(_))
    }

    pub fn into_option(self) -> Option<String> {
        self.into()
    }

    pub fn as_deref(&self) -> Option<&str> {
        match self {
            CachedName::FoundInRoom(name)
            | CachedName::FoundInProfile(name) => name.as_deref(),
            CachedName::NotFound => None,
        }
    }
}
impl From<CachedName> for Option<String> {
    fn from(cached_name: CachedName) -> Self {
        match cached_name {
            CachedName::FoundInRoom(name) => name,
            CachedName::FoundInProfile(name) => name,
            CachedName::NotFound => None,
        }
    }
}

/// Clears cached user profile.
/// This function requires passing in a reference to `Cx`,
/// which acts as a guarantee that these thread-local caches are cleared on the main UI thread, 
pub fn clear_user_profile_cache(_cx: &mut Cx) {
    while PENDING_USER_PROFILE_UPDATES.pop().is_some() {}
    // Clear user profile cache
    USER_PROFILE_CACHE.with_borrow_mut(|cache| {
        cache.clear();
    });
}


#[cfg(test)]
mod tests_room_member_entry {
    use super::*;
    use matrix_sdk::ruma::{room_id, user_id};

    #[test]
    fn global_failure_completes_pending_lookup_without_erasing_loaded_data() {
        let user = user_id!("@alice:matrix.org").to_owned();
        let mut cache = BTreeMap::from([(user.clone(), UserProfileCacheEntry::Requested)]);
        UserProfileUpdate::UserProfileFailed {user_id: user.clone()}.apply_to_cache(&mut cache);
        assert!(matches!(cache[&user], UserProfileCacheEntry::Failed));
        UserProfileUpdate::UserProfileOnly(UserProfile {
            user_id: user.clone(), username: Some("Alice".into()), avatar_state: AvatarState::Known(None),
        }).apply_to_cache(&mut cache);
        UserProfileUpdate::UserProfileFailed {user_id: user.clone()}.apply_to_cache(&mut cache);
        assert!(matches!(&cache[&user], UserProfileCacheEntry::Loaded {user_profile, ..}
            if user_profile.username.as_deref() == Some("Alice")));
        cache.clear();
        UserProfileUpdate::UserProfileFailed {user_id: user}.apply_to_cache(&mut cache);
        assert!(cache.is_empty(), "A late failure after logout must not repopulate the cache");
    }

    #[test]
    fn reconnect_allows_failed_and_pending_profiles_to_be_requested_again() {
        USER_PROFILE_CACHE.with_borrow_mut(|cache| {
            cache.clear();
            cache.insert(user_id!("@pending:test").to_owned(), UserProfileCacheEntry::Requested);
            cache.insert(user_id!("@failed:test").to_owned(), UserProfileCacheEntry::Failed);
            cache.insert(user_id!("@loaded:test").to_owned(), loaded_entry(BTreeMap::new()));
        });
        clear_all_pending_requests();
        USER_PROFILE_CACHE.with_borrow_mut(|cache| {
            assert_eq!(cache.len(), 1);
            assert!(matches!(cache.get(user_id!("@loaded:test")), Some(UserProfileCacheEntry::Loaded {..})));
            cache.clear();
        });
    }

    #[test]
    fn profile_not_found_is_an_empty_profile_but_server_errors_are_failures() {
        use ruma::api::{client::uiaa::UiaaResponse, error::{Error, ErrorBody, ErrorKind, FromHttpResponseError, StandardErrorBody}};
        let user = user_id!("@alice:matrix.org").to_owned();
        let missing = Error::new(matrix_sdk::reqwest::StatusCode::NOT_FOUND,
            ErrorBody::Standard(StandardErrorBody::new(ErrorKind::NotFound, "No profile".into())));
        let error: matrix_sdk::Error = matrix_sdk::HttpError::Api(Box::new(
            FromHttpResponseError::Server(UiaaResponse::MatrixError(missing)),
        )).into();
        assert!(matches!(UserProfileUpdate::from_fetch_error(user.clone(), &error),
            UserProfileUpdate::UserProfileOnly(UserProfile {username: None, avatar_state: AvatarState::Known(None), ..})));
        let error = matrix_sdk::Error::UnknownError("connection interrupted".into());
        assert!(matches!(UserProfileUpdate::from_fetch_error(user, &error), UserProfileUpdate::UserProfileFailed {..}));
    }

    fn loaded_entry(rooms: BTreeMap<OwnedRoomId, RoomMemberEntry>) -> UserProfileCacheEntry {
        UserProfileCacheEntry::Loaded {
            user_profile: UserProfile {
                user_id: user_id!("@alice:matrix.org").to_owned(),
                username: Some("Alice".into()),
                avatar_state: AvatarState::Unknown,
            },
            rooms,
        }
    }

    fn apply_failed(cache: &mut BTreeMap<OwnedUserId, UserProfileCacheEntry>) {
        UserProfileUpdate::RoomMemberFailed {
            user_id: user_id!("@alice:matrix.org").to_owned(),
            room_id: room_id!("!room:matrix.org").to_owned(),
        }.apply_to_cache(cache);
    }

    fn room_entry(cache: &BTreeMap<OwnedUserId, UserProfileCacheEntry>) -> Option<&RoomMemberEntry> {
        match cache.get(user_id!("@alice:matrix.org"))? {
            UserProfileCacheEntry::Loaded { rooms, .. } => rooms.get(room_id!("!room:matrix.org")),
            UserProfileCacheEntry::Requested | UserProfileCacheEntry::Failed => None,
        }
    }

    #[test]
    fn pending_room_entry_becomes_failed() {
        let mut rooms = BTreeMap::new();
        rooms.insert(room_id!("!room:matrix.org").to_owned(), RoomMemberEntry::Requested);
        let mut cache = BTreeMap::new();
        cache.insert(user_id!("@alice:matrix.org").to_owned(), loaded_entry(rooms));

        apply_failed(&mut cache);
        assert!(matches!(room_entry(&cache), Some(RoomMemberEntry::Failed)));
    }

    #[test]
    fn missing_room_entry_is_inserted_as_failed() {
        let mut cache = BTreeMap::new();
        cache.insert(user_id!("@alice:matrix.org").to_owned(), loaded_entry(BTreeMap::new()));

        apply_failed(&mut cache);
        assert!(matches!(room_entry(&cache), Some(RoomMemberEntry::Failed)));
    }

    #[test]
    fn failure_for_an_uncached_user_is_ignored() {
        let mut cache = BTreeMap::new();
        apply_failed(&mut cache);
        assert!(cache.is_empty());
    }

    #[test]
    fn a_still_pending_account_wide_entry_is_left_alone() {
        let mut cache = BTreeMap::new();
        cache.insert(user_id!("@alice:matrix.org").to_owned(), UserProfileCacheEntry::Requested);

        apply_failed(&mut cache);
        assert!(matches!(cache.get(user_id!("@alice:matrix.org")), Some(UserProfileCacheEntry::Requested)));
    }

    #[test]
    fn failed_is_not_a_loaded_member() {
        assert!(RoomMemberEntry::Failed.loaded().is_none());
        assert!(RoomMemberEntry::Requested.loaded().is_none());
    }
}
