//! What an owner's Agent does for the members of a group room it was added
//! to (ADR 0025), as the owner API's approval settings report it
//! (`approvals.roomShares`, kept in the owner's ledger). A room the owner
//! saved nothing for has the shared-team-member defaults. This module only
//! words and edits them; the ledger enforces them.
use serde_json::{Value, json};

/// The owner's choices for one Agent in one room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomShare {
    pub answer: bool,
    pub review: bool,
    pub fix: bool,
}

/// The defaults the ledger applies when nothing was saved (Chris, 2026-11-17).
pub(crate) const DEFAULT: RoomShare = RoomShare { answer: true, review: true, fix: false };

/// The choices for `room` in an `approvals` reply; `None` while it is not loaded.
pub(crate) fn for_room(approvals: &Value, room: &str) -> Option<RoomShare> {
    let saved = approvals.get("roomShares")?.as_array()?;
    let row = saved.iter().find(|row| row["room"] == room);
    Some(row.map_or(DEFAULT, |row| RoomShare {
        answer: row["answer"] == true,
        review: row["review"] == true,
        fix: row["fix"] == true,
    }))
}

/// The `{roomShare}` change that saves `share` for `room`.
pub(crate) fn change(room: &str, share: RoomShare) -> Value {
    json!({"roomShare": {"room": room, "answer": share.answer, "review": share.review, "fix": share.fix}})
}

/// "Shared in dev room: answers questions, reviews PRs; fixes need Betty's OK".
pub(crate) fn summary(share: RoomShare, room: &str, owner: &str) -> String {
    use crate::i18n::{format, tr};
    let parts = [(share.answer, "answers questions", "questions"), (share.review, "reviews PRs", "reviews"), (share.fix, "fixes issues", "fixes")];
    let does: Vec<&str> = parts.iter().filter(|p| p.0).map(|p| tr(p.1)).collect();
    let needs: Vec<&str> = parts.iter().filter(|p| !p.0).map(|p| tr(p.2)).collect();
    let separator = tr(", ");
    let args = |extra: Vec<(&'static str, String)>| {
        let mut args = vec![("room", room.to_owned()), ("owner", owner.to_owned())];
        args.extend(extra);
        args
    };
    match (does.is_empty(), needs.is_empty()) {
        (true, _) => format("Shared in {room}: everything needs {owner}'s OK", &args(vec![])),
        (false, true) => format("Shared in {room}: {does}", &args(vec![("does", does.join(separator))])),
        (false, false) => format(
            "Shared in {room}: {does}; {needs} need {owner}'s OK",
            &args(vec![("does", does.join(separator)), ("needs", needs.join(separator))]),
        ),
    }
}

/// One summary line per group room of an Agent's status reply (`rooms`,
/// `approvals`); none while the approvals are not loaded.
pub(crate) fn summaries(status: &Value, owner: &str) -> Vec<String> {
    crate::hagency::agent_panel::mapping::watch_rooms(status)
        .into_iter()
        .filter_map(|(id, name)| for_room(&status["approvals"], &id).map(|share| summary(share, &name, owner)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_answer_and_review_but_fixes_need_the_owner() {
        let approvals = json!({"roomShares": []});
        let share = for_room(&approvals, "!dev:s").unwrap();
        assert_eq!(share, DEFAULT);
        assert_eq!(summary(share, "dev room", "Betty"), "Shared in dev room: answers questions, reviews PRs; fixes need Betty's OK");
        assert_eq!(for_room(&Value::Null, "!dev:s"), None, "unknown until loaded");
    }

    #[test]
    fn a_saved_room_is_worded_as_saved() {
        let approvals = json!({"roomShares": [{"room": "!dev:s", "answer": true, "review": false, "fix": true}]});
        let share = for_room(&approvals, "!dev:s").unwrap();
        assert_eq!(share, RoomShare { answer: true, review: false, fix: true });
        assert_eq!(summary(share, "dev", "Betty"), "Shared in dev: answers questions, fixes issues; reviews need Betty's OK");
        assert_eq!(for_room(&approvals, "!other:s"), Some(DEFAULT), "other rooms keep the defaults");
        let all = RoomShare { answer: true, review: true, fix: true };
        assert_eq!(summary(all, "dev", "Betty"), "Shared in dev: answers questions, reviews PRs, fixes issues");
        let none = RoomShare { answer: false, review: false, fix: false };
        assert_eq!(summary(none, "dev", "Betty"), "Shared in dev: everything needs Betty's OK");
    }

    #[test]
    fn the_change_names_every_choice() {
        let saved = change("!dev:s", RoomShare { answer: false, review: true, fix: false });
        assert_eq!(saved, json!({"roomShare": {"room": "!dev:s", "answer": false, "review": true, "fix": false}}));
    }

    #[test]
    fn every_group_room_of_the_status_gets_a_line() {
        let status = json!({
            "rooms": [
                {"roomId": "!dev:s", "name": "dev room", "state": "active"},
                {"roomId": "!old:s", "name": "old", "state": "left"}
            ],
            "approvals": {"roomShares": []}
        });
        assert_eq!(summaries(&status, "Betty"), vec!["Shared in dev room: answers questions, reviews PRs; fixes need Betty's OK".to_owned()]);
        assert!(summaries(&json!({"rooms": status["rooms"]}), "Betty").is_empty(), "elsewhere: approvals unknown");
    }
}
