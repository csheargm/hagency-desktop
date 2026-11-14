//! An avatar or display name changed on another device.
//!
//! The screens that keep a copy of the user's own profile (the profile icon,
//! Settings) hear of changes made on this device through
//! [`AccountDataAction::AvatarChanged`] and its kin. A change made on another
//! device reaches this one only as a new member event of the user's in each
//! joined room: the timeline showed it, those copies never did. This watches for
//! such events, reads the global profile again (a member event can also carry a
//! per-room avatar or name), and posts
//! [`AccountDataAction::ProfileChangedElsewhere`] for the screens to compare.

use std::sync::Mutex;

use makepad_widgets::{log, Cx};
use matrix_sdk::{
    ruma::{
        api::client::profile::{AvatarUrl, DisplayName},
        events::room::member::{MembershipState, OriginalSyncRoomMemberEvent},
        MilliSecondsSinceUnixEpoch, OwnedMxcUri, OwnedUserId, UserId,
    },
    Client,
};

use crate::sliding_sync::AccountDataAction;

/// A member event older than this comes from history (a first sync or a
/// back-paginated timeline), not from a change made now; the profile shown was
/// read from the server at sign-in.
const RECENT_MS: u64 = 10 * 60 * 1000;

type Profile = (Option<OwnedMxcUri>, Option<String>);

#[derive(Default)]
struct Watch {
    user: Option<OwnedUserId>,
    /// The profile the server gave the last time it was read.
    last: Option<Profile>,
    fetching: bool,
    /// A further change arrived while the server was being asked.
    again: bool,
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// Watch this client's sync for the user's own profile changes.
pub fn attach(client: &Client) {
    client.add_event_handler(|event: OriginalSyncRoomMemberEvent, client: Client| async move {
        let Some(own) = client.user_id().map(ToOwned::to_owned) else {
            return;
        };
        let prev = event.unsigned.prev_content.as_ref();
        let changed = changes_own_profile(
            &own,
            event.state_key.as_str(),
            &event.content.membership,
            prev.map(|p| (&p.avatar_url, &p.displayname)),
            (&event.content.avatar_url, &event.content.displayname),
            u64::from(event.origin_server_ts.0),
            u64::from(MilliSecondsSinceUnixEpoch::now().0),
        );
        if !changed {
            return;
        }
        let shown = (event.content.avatar_url.clone(), event.content.displayname.clone());
        if !begin(&own, &shown) {
            return;
        }
        loop {
            let read = client.account().fetch_user_profile_of(&own).await;
            let profile = match read {
                Ok(response) => Some((
                    response.get_static::<AvatarUrl>().ok().flatten(),
                    response.get_static::<DisplayName>().ok().flatten(),
                )),
                Err(error) => {
                    log!("Couldn't read the own profile after a member event: {error}");
                    None
                }
            };
            match finish(&own, profile) {
                Step::Post((avatar_url, display_name)) => {
                    Cx::post_action(AccountDataAction::ProfileChangedElsewhere { avatar_url, display_name });
                    if !again(&own) {
                        break;
                    }
                }
                Step::Again => {}
                Step::Done => break,
            }
        }
    });
}

/// Whether one member event changes the user's own avatar or name now.
fn changes_own_profile(
    own: &UserId,
    state_key: &str,
    membership: &MembershipState,
    prev: Option<(&Option<OwnedMxcUri>, &Option<String>)>,
    now: (&Option<OwnedMxcUri>, &Option<String>),
    sent_ms: u64,
    now_ms: u64,
) -> bool {
    state_key == own.as_str()
        && *membership == MembershipState::Join
        && now_ms.saturating_sub(sent_ms) <= RECENT_MS
        && prev.is_some_and(|prev| prev != now)
}

/// Start reading the profile, unless the server already gave what the event
/// shows or a read is under way (which then reads once more when done).
fn begin(own: &UserId, shown: &Profile) -> bool {
    let mut guard = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    let watch = guard.get_or_insert_with(Watch::default);
    if watch.user.as_deref() != Some(own) {
        *watch = Watch { user: Some(own.to_owned()), ..Watch::default() };
    }
    if watch.last.as_ref() == Some(shown) {
        return false;
    }
    if watch.fetching {
        watch.again = true;
        return false;
    }
    watch.fetching = true;
    true
}

enum Step {
    /// The server's profile differs from the last one it gave.
    Post(Profile),
    /// Nothing new, but a change arrived during the read: read again.
    Again,
    Done,
}

fn finish(own: &UserId, profile: Option<Profile>) -> Step {
    let mut guard = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    let Some(watch) = guard.as_mut().filter(|w| w.user.as_deref() == Some(own)) else {
        return Step::Done;
    };
    if let Some(profile) = profile && watch.last.as_ref() != Some(&profile) {
        watch.last = Some(profile.clone());
        return Step::Post(profile);
    }
    if std::mem::take(&mut watch.again) {
        return Step::Again;
    }
    watch.fetching = false;
    Step::Done
}

/// After posting: read again if a change arrived meanwhile, else stop.
fn again(own: &UserId) -> bool {
    let mut guard = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    let Some(watch) = guard.as_mut().filter(|w| w.user.as_deref() == Some(own)) else {
        return false;
    };
    if std::mem::take(&mut watch.again) {
        return true;
    }
    watch.fetching = false;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_mxc_uri, user_id};

    fn reset() {
        *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[test]
    fn only_a_recent_change_of_the_own_joined_member_counts() {
        let own = user_id!("@me:example.org");
        let old = Some(owned_mxc_uri!("mxc://example.org/old"));
        let new = Some(owned_mxc_uri!("mxc://example.org/new"));
        let name = Some("Me".to_owned());
        let now = 1_000_000_000;
        let join = MembershipState::Join;
        let check = |key: &str, m: &MembershipState, prev: Option<(&Option<OwnedMxcUri>, &Option<String>)>, sent: u64| {
            changes_own_profile(own, key, m, prev, (&new, &name), sent, now)
        };
        assert!(check("@me:example.org", &join, Some((&old, &name)), now - 1_000));
        // Someone else, an unchanged profile, a first join, a leave, or history.
        assert!(!check("@you:example.org", &join, Some((&old, &name)), now));
        assert!(!check("@me:example.org", &join, Some((&new, &name)), now));
        assert!(!check("@me:example.org", &join, None, now));
        assert!(!check("@me:example.org", &MembershipState::Leave, Some((&old, &name)), now));
        assert!(!check("@me:example.org", &join, Some((&old, &name)), now - RECENT_MS - 1));
        // A changed name alone counts too.
        let renamed = Some("Me again".to_owned());
        assert!(changes_own_profile(own, "@me:example.org", &join, Some((&new, &name)), (&new, &renamed), now, now));
    }

    #[test]
    fn one_change_seen_in_many_rooms_reads_the_server_once() {
        reset();
        let own = user_id!("@me:example.org");
        let new: Profile = (Some(owned_mxc_uri!("mxc://example.org/new")), Some("Me".to_owned()));
        assert!(begin(own, &new));
        // Further rooms' events while the read runs: no second read yet.
        assert!(!begin(own, &new));
        // The read returns the new profile: post it, then read once more for
        // the events that arrived meanwhile, which changes nothing.
        assert!(matches!(finish(own, Some(new.clone())), Step::Post(_)));
        assert!(again(own));
        assert!(matches!(finish(own, Some(new.clone())), Step::Done));
        // Later rooms' events show what the server gave: no read at all.
        assert!(!begin(own, &new));
        // A failed read ends the run, and the next event can start one.
        let newer: Profile = (Some(owned_mxc_uri!("mxc://example.org/newer")), Some("Me".to_owned()));
        assert!(begin(own, &newer));
        assert!(matches!(finish(own, None), Step::Done));
        assert!(begin(own, &newer));
        assert!(matches!(finish(own, Some(newer)), Step::Post(_)));
        assert!(!again(own));
        // Another account starts over.
        let other = user_id!("@other:example.org");
        assert!(begin(other, &new));
        reset();
    }
}
