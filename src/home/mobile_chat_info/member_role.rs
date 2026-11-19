//! Room roles on the Members tab (ADR 0030), in plain words over Matrix
//! power levels: the room's **Owner** (power level 100, or a room v12
//! creator) and **Maintainers** (power level 50). Maintainers change the
//! room's agent work level and resume it; only the owner sets merge rules
//! and makes or removes maintainers. "Make maintainer" / "Remove maintainer"
//! sit behind a row's "⋯", for the room's owner only, never on the owner,
//! oneself, an Agent or a Hagency service account; they change exactly that
//! person's entry in the `users` map of the server's power levels.
use matrix_sdk::{Room, ruma::{Int, OwnedUserId, UserId, events::StateEventType, events::room::power_levels::RoomPowerLevels}};
use serde_json::Value;
use std::collections::HashMap;

/// The owner's level and the maintainer level.
pub const OWNER_LEVEL: i64 = 100;
pub use ::hagency::issue_watch::safeguards::room_autonomy::MAINTAINER_LEVEL;

/// A member's role, when they have one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Maintainer,
}

impl Role {
    /// The tag on the member's row.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Owner => "Owner",
            Self::Maintainer => "Maintainer",
        }
    }
}

/// The roles in a room and whether this account is its owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    pub of: HashMap<OwnedUserId, Role>,
    /// This account may make and remove maintainers.
    pub i_am_owner: bool,
}

/// `user`'s role under `levels`.
pub fn role_of(levels: &RoomPowerLevels, user: &UserId) -> Option<Role> {
    let level = levels.for_user(user);
    if level >= Int::new_saturating(OWNER_LEVEL) {
        Some(Role::Owner)
    } else if level >= Int::new_saturating(MAINTAINER_LEVEL) {
        Some(Role::Maintainer)
    } else {
        None
    }
}

/// The role a plain power `level` gives (no room-creator rule).
pub fn role_at(level: i64) -> Option<Role> {
    if level >= OWNER_LEVEL {
        Some(Role::Owner)
    } else if level >= MAINTAINER_LEVEL {
        Some(Role::Maintainer)
    } else {
        None
    }
}

/// Whether `user` is a person (not an Agent puppet or a Hagency service account).
fn is_person(user: &UserId) -> bool {
    !user.localpart().starts_with(hagency_core::puppet::RESERVED_LOCALPART_PREFIX) && !crate::shared::mention_candidates::is_hagency_service_account(user)
}

/// The roles of `members` in `room`, from the room's power levels as the
/// homeserver has them now: the sync store can still hold the levels from
/// before a role change, so the list kept showing the old role (on-screen
/// bug, Nov 18). The store's levels are the fallback when the server read fails.
pub async fn roles(room: &Room, members: &[OwnedUserId]) -> Roles {
    let server = crate::home::call_power_levels::server_levels(room).await.ok()
        .and_then(|content| crate::home::call_power_levels::typed_levels(room, content));
    let levels = match server {
        Some(levels) => levels,
        None => match room.power_levels().await {
            Ok(levels) => levels,
            // No readable levels: nobody gets a role tag or the role menu (fail closed).
            Err(_) => return Roles::default(),
        },
    };
    roles_under(&levels, members, room.own_user_id())
}

/// The roles of `members` under `levels`, and whether `me` is the owner.
pub fn roles_under(levels: &RoomPowerLevels, members: &[OwnedUserId], me: &UserId) -> Roles {
    let of = members.iter().filter_map(|m| role_of(levels, m).map(|r| (m.clone(), r))).collect();
    let i_am_owner = role_of(levels, me) == Some(Role::Owner) && levels.user_can_send_state(me, StateEventType::RoomPowerLevels);
    Roles { of, i_am_owner }
}

/// What the owner may do to `target`'s role: `Some(true)` make maintainer,
/// `Some(false)` remove maintainer, `None` nothing.
pub fn change_for(roles: &Roles, me: Option<&UserId>, target: &UserId) -> Option<bool> {
    if !roles.i_am_owner || me == Some(target) || !is_person(target) {
        return None;
    }
    match roles.of.get(target) {
        Some(Role::Owner) => None,
        Some(Role::Maintainer) => Some(false),
        None => Some(true),
    }
}

/// The power-level content with only `target`'s entry changed: 50 to make
/// a maintainer, 0 to remove one. Refuses a target at the owner's level.
pub fn with_role(mut levels: Value, target: &UserId, make: bool) -> Result<Value, String> {
    let users = levels.as_object_mut().ok_or("power levels are not an object")?
        .entry("users").or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut().ok_or("power levels users is not an object")?;
    let current = users.get(target.as_str()).and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())).unwrap_or(0);
    if current >= OWNER_LEVEL {
        return Err(crate::i18n::tr("The room's owner keeps their role.").to_owned());
    }
    users.insert(target.to_string(), Value::from(if make { MAINTAINER_LEVEL } else { 0 }));
    Ok(levels)
}

/// Make or remove `target` as a maintainer of `room`, on the server's power
/// levels, through the desktop's one power-level writer (re-read before the
/// write, checked after it; see [`change_levels`](crate::home::call_power_levels::change_levels)).
pub async fn set_maintainer(room: &Room, target: &UserId, make: bool) -> Result<(), String> {
    if !is_person(target) {
        return Err(crate::i18n::tr("Agents and service accounts cannot be maintainers.").to_owned());
    }
    // A refusal is the user's answer as it is; any other failure says the role did not change.
    let mut refused = None;
    let outcome = crate::home::call_power_levels::change_levels(room, |current| {
        role_change(room, current, target, make).map(Some).map_err(|why| {
            refused = Some(why.clone());
            anyhow::anyhow!(why)
        })
    }).await;
    match (outcome, refused) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(why)) => Err(why),
        (Err(e), None) => Err(crate::i18n::format("Could not change the role: {error}", &[("error", e.to_string())])),
    }
}

/// `current` (the server's levels) with `target`'s role changed, rechecked on
/// those levels, not the list's (GPT review): this account is the room's
/// owner, and the target's effective level is below the owner's.
fn role_change(room: &Room, current: Value, target: &UserId, make: bool) -> Result<Value, String> {
    let levels = crate::home::call_power_levels::typed_levels(room, current.clone()).ok_or_else(|| crate::i18n::tr("The room's power levels are unreadable.").to_owned())?;
    let me = room.own_user_id();
    if role_of(&levels, me) != Some(Role::Owner) || !levels.user_can_send_state(me, StateEventType::RoomPowerLevels) {
        return Err(crate::i18n::tr("Only the room's owner can make or remove maintainers.").to_owned());
    }
    if role_of(&levels, target) == Some(Role::Owner) {
        return Err(crate::i18n::tr("The room's owner keeps their role.").to_owned());
    }
    with_role(current, target, make)
}

/// The button text for a change.
pub fn button_text(make: bool) -> &'static str {
    if make { "Make maintainer" } else { "Remove maintainer" }
}

/// Posted when a role change finished, or when the room's power levels
/// changed in sync (any member's change), so the Members list reads the roles again.
#[derive(Clone, Debug)]
pub struct RoleChanged {
    pub room_id: matrix_sdk::ruma::OwnedRoomId,
}

/// The roles read again for a room's listed members ([`spawn_reread`]).
#[derive(Clone, Debug)]
pub struct RolesLoaded {
    pub room_id: matrix_sdk::ruma::OwnedRoomId,
    pub owner: OwnedUserId,
    pub roles: Roles,
    /// Whom this account may remove now (`None`: members unreadable, keep the list's).
    pub removable: Option<std::collections::HashSet<OwnedUserId>>,
    request: u64,
}

/// The newest role read; an older one finishing later is dropped.
static LATEST_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl RolesLoaded {
    /// Whether no newer role read has started since this one.
    pub fn is_latest(&self) -> bool {
        self.request == LATEST_READ.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Reads `room`'s roles again for `members` in the background, without
/// reloading the list (no scroll jump on another member's change).
pub fn spawn_reread(room: Room, members: Vec<OwnedUserId>) {
    let request = LATEST_READ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    crate::sliding_sync::spawn_async_task(async move {
        let roles = roles(&room, &members).await;
        // A role change can also change whom this account may remove (GPT review).
        let removable = match room.members(matrix_sdk::RoomMemberships::JOIN | matrix_sdk::RoomMemberships::BAN).await {
            Ok(listed) => Some(super::member_remove::removable(&room, &listed).await),
            // Unreadable members: the list keeps its removal rights until its next full read.
            Err(_) => None,
        };
        let (room_id, owner) = (room.room_id().to_owned(), room.own_user_id().to_owned());
        makepad_widgets::Cx::post_action(RolesLoaded { room_id, owner, roles, removable, request });
    });
}

/// Make or remove `target` as maintainer in the background; the result shows as a popup.
pub fn spawn_change(room: Room, target: OwnedUserId, make: bool, name: String) {
    use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
    crate::sliding_sync::spawn_async_task(async move {
        match set_maintainer(&room, &target, make).await {
            Ok(()) => {
                let text = if make { "{name} is now a maintainer of this room." } else { "{name} is no longer a maintainer of this room." };
                enqueue_popup_notification(crate::i18n::format(text, &[("name", name)]), PopupKind::Success, Some(5.0));
            }
            Err(error) => enqueue_popup_notification(error, PopupKind::Error, None),
        }
        makepad_widgets::Cx::post_action(RoleChanged { room_id: room.room_id().to_owned() });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(id: &str) -> OwnedUserId { OwnedUserId::try_from(id).unwrap() }

    #[test]
    fn only_the_owner_makes_or_removes_maintainers_and_never_agents_or_the_owner() {
        let mut of = HashMap::new();
        of.insert(user("@own:x"), Role::Owner);
        of.insert(user("@mo:x"), Role::Maintainer);
        let owner = Roles { of: of.clone(), i_am_owner: true };
        assert_eq!(change_for(&owner, Some(&user("@own:x")), &user("@anna:x")), Some(true), "make a member a maintainer");
        assert_eq!(change_for(&owner, Some(&user("@own:x")), &user("@mo:x")), Some(false), "remove a maintainer");
        assert_eq!(change_for(&owner, Some(&user("@own:x")), &user("@own:x")), None, "not oneself");
        assert_eq!(change_for(&owner, Some(&user("@own:x")), &user("@_hagency_agt_bird:x")), None, "never an Agent");
        let maintainer = Roles { of, i_am_owner: false };
        assert_eq!(change_for(&maintainer, Some(&user("@mo:x")), &user("@anna:x")), None, "a maintainer cannot assign maintainers");
    }

    #[test]
    fn the_change_touches_only_that_persons_entry() {
        let levels = json!({"users": {"@own:x": 100, "@bob:x": 25}, "users_default": 0, "events": {"m.room.power_levels": 100}});
        let made = with_role(levels.clone(), &user("@anna:x"), true).unwrap();
        let mut expected = levels.clone();
        expected["users"]["@anna:x"] = json!(50);
        assert_eq!(made, expected);
        let removed = with_role(made, &user("@anna:x"), false).unwrap();
        assert_eq!(removed["users"]["@anna:x"], 0);
        assert_eq!(removed["users"]["@bob:x"], 25, "others untouched");
        assert!(with_role(levels, &user("@own:x"), false).is_err(), "the owner keeps their role");
        assert_eq!(Role::Maintainer.tag(), "Maintainer");
    }

    /// On-screen bug (Nov 18): bird at 50 after "Make maintainer" must read
    /// as a Maintainer, with "Remove maintainer" offered to the owner.
    #[test]
    fn roles_follow_the_rooms_current_power_levels() {
        let rules = matrix_sdk::ruma::room_version_rules::AuthorizationRules::V1;
        let content = json!({"users": {"@anna:x": 100, "@bird:x": 50, "@cat:x": 0}});
        let levels = crate::home::call_power_levels::levels_from(content, &rules, vec![]).unwrap();
        let members = [user("@anna:x"), user("@bird:x"), user("@cat:x")];
        let roles = roles_under(&levels, &members, &user("@anna:x"));
        assert!(roles.i_am_owner);
        assert_eq!(roles.of.get(&user("@anna:x")), Some(&Role::Owner));
        assert_eq!(roles.of.get(&user("@bird:x")), Some(&Role::Maintainer));
        assert_eq!(roles.of.get(&user("@cat:x")), None);
        assert_eq!(change_for(&roles, Some(&user("@anna:x")), &user("@bird:x")), Some(false), "Remove maintainer for bird");
        assert_eq!(button_text(false), "Remove maintainer");
        let removed = crate::home::call_power_levels::levels_from(json!({"users": {"@anna:x": 100}}), &rules, vec![]).unwrap();
        assert_eq!(roles_under(&removed, &members, &user("@anna:x")).of.get(&user("@bird:x")), None, "removed: no tag");
        assert_eq!((role_at(100), role_at(50), role_at(49)), (Some(Role::Owner), Some(Role::Maintainer), None));
    }
}
