//! "Review by a different model" (ADR 0024): a project room's setting, kept
//! as room state any admin may change, ON unless an admin turned it off.
//! When the tracker asks for a review of a pull request a Claude Agent
//! opened, it picks a reviewer in the room running another runtime (Codex),
//! and the reverse for Codex; with none in the room it asks any other Agent
//! and says so once in the room.
use matrix_sdk::ruma::OwnedUserId;
use serde_json::Value;

/// The room state event (empty state key) holding the setting.
pub const ROOM_REVIEW_STATE: &str = "org.hagency.room_review";
/// Its field: `false` turns the preference off; absent or anything else is on.
pub const DIFFERENT_MODEL: &str = "differentModel";

/// Whether the room prefers a different-model reviewer, from the state
/// event's content (`None`: never set, so the project-room default ON).
pub(crate) fn different_model(content: Option<&Value>) -> bool {
    content.is_none_or(|c| c[DIFFERENT_MODEL] != false)
}

/// One Agent in the room that could review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub puppet: OwnedUserId,
    /// What the room shows for it ("anna-claude").
    pub name: String,
    /// "claude", "codex" or "octos"; `None` when the room cannot see it.
    pub runtime: Option<&'static str>,
    /// One of this owner's own Agents (its approval is in this ledger).
    pub own: bool,
}

/// The reviewer chosen, and the room line to post when it is a fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pick {
    pub reviewer: OwnedUserId,
    pub notice: Option<String>,
}

/// The reviewer for a pull request opened by an Agent running `author`.
/// Own Agents come first among equals; `None` when nobody else can review.
pub(crate) fn pick(author: Option<&str>, candidates: &[Candidate], prefer_different: bool) -> Option<Pick> {
    let ordered: Vec<&Candidate> = candidates.iter().filter(|c| c.own).chain(candidates.iter().filter(|c| !c.own)).collect();
    let first = *ordered.first()?;
    let Some(author) = author.filter(|_| prefer_different) else {
        return Some(Pick { reviewer: first.puppet.clone(), notice: None });
    };
    if let Some(other) = ordered.iter().find(|c| c.runtime.is_some_and(|r| r != author)) {
        return Some(Pick { reviewer: other.puppet.clone(), notice: None });
    }
    let args = [("wanted", wanted(author).to_owned()), ("name", first.name.clone())];
    Some(Pick { reviewer: first.puppet.clone(), notice: Some(crate::i18n::format(NO_OTHER_MODEL, &args)) })
}

pub const NO_OTHER_MODEL: &str = "No {wanted} reviewer in the room; asked {name}";

/// The runtime a reviewer of `author`'s pull request should run, in words.
fn wanted(author: &str) -> &'static str {
    match author {
        "claude" => "Codex",
        "codex" => "Claude",
        _ => crate::i18n::tr("different-model"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, runtime: Option<&'static str>, own: bool) -> Candidate {
        Candidate { puppet: OwnedUserId::try_from(format!("@{id}:s")).unwrap(), name: id.into(), runtime, own }
    }

    #[test]
    fn the_setting_is_on_unless_an_admin_turned_it_off() {
        assert!(different_model(None), "a project room starts with it on");
        assert!(different_model(Some(&json!({}))));
        assert!(different_model(Some(&json!({"differentModel": true}))));
        assert!(!different_model(Some(&json!({"differentModel": false}))));
    }

    #[test]
    fn a_claude_pr_goes_to_a_codex_reviewer_on_any_server() {
        let room = [agent("anna-claude", Some("claude"), true), agent("bo-codex", Some("codex"), false)];
        assert_eq!(pick(Some("claude"), &room, true), Some(Pick { reviewer: room[1].puppet.clone(), notice: None }), "the other owner's Codex Agent");
        let reverse = [agent("anna-codex", Some("codex"), true), agent("bo-claude", Some("claude"), false)];
        assert_eq!(pick(Some("codex"), &reverse, true).unwrap().reviewer, reverse[1].puppet, "and the reverse");
    }

    #[test]
    fn with_no_other_model_it_falls_back_and_says_so() {
        let room = [agent("bo-claude", None, false), agent("anna-claude", Some("claude"), true)];
        let picked = pick(Some("claude"), &room, true).unwrap();
        assert_eq!(picked.reviewer, room[1].puppet, "own Agents first: their approval counts here");
        assert_eq!(picked.notice.as_deref(), Some("No Codex reviewer in the room; asked anna-claude"));
        let codex_only = [agent("bo-codex", Some("codex"), true)];
        assert_eq!(pick(Some("codex"), &codex_only, true).unwrap().notice.as_deref(), Some("No Claude reviewer in the room; asked bo-codex"));
    }

    #[test]
    fn turned_off_or_unknown_it_asks_the_first_agent_quietly() {
        let room = [agent("anna-claude", Some("claude"), true), agent("bo-codex", Some("codex"), false)];
        assert_eq!(pick(Some("claude"), &room, false), Some(Pick { reviewer: room[0].puppet.clone(), notice: None }));
        assert_eq!(pick(None, &room, true), Some(Pick { reviewer: room[0].puppet.clone(), notice: None }));
        assert_eq!(pick(Some("claude"), &[], true), None, "nobody else: a person reviews");
    }
}
