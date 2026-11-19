//! The timeline line for a power-level change in plain words (ADR 0030):
//! when the change is one person's level moving into or out of a room role
//! ("made bird a maintainer.", "removed bird as maintainer.", "made bird an
//! owner."), and nothing else in the levels changed; otherwise the caller
//! keeps the generic "set the power levels for this room." line. A change to
//! only Hagency's own event levels (the one-click setup) reads "updated who
//! may change agent work settings." Read from
//! the event's content and previous content as JSON, so any field that differs
//! besides one `users` entry falls back to the generic line.
use crate::home::mobile_chat_info::member_role::{Role, role_at};
use matrix_sdk::ruma::events::{StateEventContentChange, room::power_levels::RoomPowerLevelsEventContent};
use serde_json::{Map, Value};

/// The line for `change` when it gave or took one person's role, with
/// `name_of` giving that person's shown name from their Matrix ID.
pub fn role_change_preview(change: &StateEventContentChange<RoomPowerLevelsEventContent>, format_as_html: bool, name_of: &mut dyn FnMut(&str) -> String) -> Option<String> {
    let StateEventContentChange::Original { content, prev_content: Some(prev) } = change else { return None };
    // Unserializable content cannot be compared: the generic line stays.
    let (new, prev) = (serde_json::to_value(content).ok()?, serde_json::to_value(prev).ok()?);
    if only_hagency_event_levels(&prev, &new) {
        return Some(crate::i18n::tr(HAGENCY_LEVELS_LINE).to_owned());
    }
    let (who, before, after) = single_role_change(&prev, &new)?;
    let name = name_of(&who);
    let name = if format_as_html { htmlize::escape_text(&name).into_owned() } else { name };
    Some(role_change_text(before, after, &name))
}

/// Hagency's own state event types (agent work, pause, merge rules, linked repositories).
const HAGENCY_EVENT_PREFIX: &str = "io.hagency.";
/// The line for a change to who may send Hagency's own events (the one-click
/// "Let agents work" setup sets them), after the sender's name.
const HAGENCY_LEVELS_LINE: &str = "updated who may change agent work settings.";

/// Whether `prev` to `new` changed only the levels of Hagency's own event types.
pub fn only_hagency_event_levels(prev: &Value, new: &Value) -> bool {
    prev != new && without_hagency_events(prev) == without_hagency_events(new)
}

fn without_hagency_events(content: &Value) -> Value {
    let mut copy = content.clone();
    if let Some(events) = copy.get_mut("events").and_then(Value::as_object_mut) {
        events.retain(|kind, _| !kind.starts_with(HAGENCY_EVENT_PREFIX));
    }
    copy
}

/// The one person whose role changed between `prev` and `new`, with the
/// old and new role, when that is the whole change.
pub fn single_role_change(prev: &Value, new: &Value) -> Option<(String, Option<Role>, Option<Role>)> {
    if without_users(prev) != without_users(new) {
        return None;
    }
    let (old_users, new_users) = (users(prev), users(new));
    let mut changed = old_users.keys().chain(new_users.keys())
        .filter(|id| level(&old_users, id, prev) != level(&new_users, id, new));
    let first = changed.next()?.clone();
    // Each differing user is seen once from each map; any other user means more than one change.
    if changed.any(|id| *id != first) {
        return None;
    }
    let (before, after) = (role_at(level(&old_users, &first, prev)), role_at(level(&new_users, &first, new)));
    (before != after).then_some((first, before, after))
}

/// The line after the sender's name for that change, with `name` the person's shown name.
pub fn role_change_text(before: Option<Role>, after: Option<Role>, name: &str) -> String {
    use crate::i18n::format;
    let key = match (before, after) {
        (_, Some(Role::Owner)) => "made {name} an owner.",
        (_, Some(Role::Maintainer)) => "made {name} a maintainer.",
        (Some(Role::Owner), None) => "removed {name} as owner.",
        (_, None) => "removed {name} as maintainer.",
    };
    format(key, &[("name", name.to_owned())])
}

fn without_users(content: &Value) -> Value {
    let mut copy = content.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("users");
    }
    copy
}

fn users(content: &Value) -> Map<String, Value> {
    content["users"].as_object().cloned().unwrap_or_default()
}

/// `id`'s effective level: their entry, else the room's `users_default` (0 when absent).
fn level(users: &Map<String, Value>, id: &str, content: &Value) -> i64 {
    users.get(id).or(content.get("users_default")).and_then(as_level).unwrap_or(0)
}

/// A level as a number or, in old rooms, a numeric string.
fn as_level(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_persons_role_change_reads_in_plain_words() {
        let before = json!({"users": {"@anna:x": 100}, "ban": 50});
        let made = json!({"users": {"@anna:x": 100, "@bird:x": 50}, "ban": 50});
        let (who, old, new) = single_role_change(&before, &made).unwrap();
        assert_eq!((who.as_str(), old, new), ("@bird:x", None, Some(Role::Maintainer)));
        assert_eq!(role_change_text(old, new, "bird"), "made bird a maintainer.");
        let (_, old, new) = single_role_change(&made, &json!({"users": {"@anna:x": 100, "@bird:x": 0}, "ban": 50})).unwrap();
        assert_eq!(role_change_text(old, new, "bird"), "removed bird as maintainer.");
        let (_, old, new) = single_role_change(&made, &json!({"users": {"@anna:x": 100, "@bird:x": 100}, "ban": 50})).unwrap();
        assert_eq!(role_change_text(old, new, "bird"), "made bird an owner.");
        assert_eq!(role_change_text(Some(Role::Owner), None, "bird"), "removed bird as owner.");
    }

    #[test]
    fn any_other_change_keeps_the_generic_line() {
        let before = json!({"users": {"@anna:x": 100}, "ban": 50});
        assert!(single_role_change(&before, &json!({"users": {"@anna:x": 100, "@bird:x": 50}, "ban": 100})).is_none(), "another field changed");
        assert!(single_role_change(&before, &json!({"users": {"@anna:x": 100, "@bird:x": 50, "@cat:x": 50}, "ban": 50})).is_none(), "two people changed");
        assert!(single_role_change(&before, &json!({"users": {"@anna:x": 100, "@bird:x": 10}, "ban": 50})).is_none(), "no role changed");
        assert!(single_role_change(&before, &before).is_none(), "nothing changed");
    }

    /// The one-click setup changes only io.hagency.* event levels: a plain line, not the generic one.
    #[test]
    fn a_change_to_hagency_event_levels_reads_in_plain_words() {
        let before = json!({"users": {"@anna:x": 100}, "events": {"m.room.name": 50}});
        let setup = json!({"users": {"@anna:x": 100}, "events": {"m.room.name": 50, "io.hagency.room_autonomy": 50, "io.hagency.room_pause": 0}});
        assert!(only_hagency_event_levels(&before, &setup));
        assert_eq!(crate::i18n::tr(HAGENCY_LEVELS_LINE), "updated who may change agent work settings.");
        assert!(!only_hagency_event_levels(&before, &before), "nothing changed");
        let mut other = setup.clone();
        other["events"]["m.room.name"] = json!(100);
        assert!(!only_hagency_event_levels(&before, &other), "a Matrix event level changed too");
        let mut person = setup.clone();
        person["users"]["@bird:x"] = json!(50);
        assert!(!only_hagency_event_levels(&before, &person), "a person's level changed too");
    }
}
