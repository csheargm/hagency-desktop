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
const OWNER_LEVEL: i64 = 100;
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

/// Whether `user` is a person (not an Agent puppet or a Hagency service account).
fn is_person(user: &UserId) -> bool {
    !user.localpart().starts_with(hagency_core::puppet::RESERVED_LOCALPART_PREFIX) && !crate::shared::mention_candidates::is_hagency_service_account(user)
}

/// The roles of `members` in `room`, from the room's power levels.
pub async fn roles(room: &Room, members: &[OwnedUserId]) -> Roles {
    let Ok(levels) = room.power_levels().await else { return Roles::default() };
    let me = room.own_user_id();
    let of = members.iter().filter_map(|m| role_of(&levels, m).map(|r| (m.clone(), r))).collect();
    let i_am_owner = role_of(&levels, me) == Some(Role::Owner) && levels.user_can_send_state(me, StateEventType::RoomPowerLevels);
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

/// Make or remove `target` as a maintainer of `room`, on the server's power levels.
pub async fn set_maintainer(room: &Room, target: &UserId, make: bool) -> Result<(), String> {
    use matrix_sdk::ruma::api::client::state::get_state_event_for_key::v3 as get_state;
    if !is_person(target) {
        return Err(crate::i18n::tr("Agents and service accounts cannot be maintainers.").to_owned());
    }
    // One change at a time from this desktop; a change from another client between
    // this read and the write can still be lost (Matrix has no conditional state write).
    static CHANGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _changing = CHANGING.lock().await;
    let request = get_state::Request::new(room.room_id().to_owned(), StateEventType::RoomPowerLevels, String::new());
    let reply = room.client().send(request).await.map_err(|e| e.to_string())?;
    let current: Value = serde_json::from_str(reply.event_or_content.get()).map_err(|e| e.to_string())?;
    // Rechecked on the server's levels, not the list's (GPT review): this account is
    // the room's owner, and the target's effective level is below the owner's.
    let levels = crate::home::call_power_levels::typed_levels(room, current.clone()).ok_or_else(|| crate::i18n::tr("The room's power levels are unreadable.").to_owned())?;
    let me = room.own_user_id();
    if role_of(&levels, me) != Some(Role::Owner) || !levels.user_can_send_state(me, StateEventType::RoomPowerLevels) {
        return Err(crate::i18n::tr("Only the room's owner can make or remove maintainers.").to_owned());
    }
    if role_of(&levels, target) == Some(Role::Owner) {
        return Err(crate::i18n::tr("The room's owner keeps their role.").to_owned());
    }
    let updated = with_role(current, target, make)?;
    room.send_state_event_raw("m.room.power_levels", "", updated).await.map(|_| ())
        .map_err(|e| crate::i18n::format("Could not change the role: {error}", &[("error", e.to_string())]))
}

/// The button text for a change.
pub fn button_text(make: bool) -> &'static str {
    if make { "Make maintainer" } else { "Remove maintainer" }
}

/// Posted when a role change finished, so the Members list reads the room again.
#[derive(Clone, Debug)]
pub struct RoleChanged {
    pub room_id: matrix_sdk::ruma::OwnedRoomId,
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
}
