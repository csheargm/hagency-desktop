//! "Review by a different model" (ADR 0025): a project room's setting, kept
//! as room state any admin may change, ON unless an admin turned it off.
//! When the tracker asks for a review of a pull request a Claude Agent
//! opened, it picks a reviewer in the room running another runtime (Codex),
//! and the reverse for Codex; with none in the room it asks any other Agent,
//! another owner's first, and says so once in the room (and that it could not
//! check every Agent, when it could not).
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
    /// One of this owner's own Agents: its GitHub verdicts post as this owner's login.
    pub own: bool,
}

/// The reviewer for a pull request opened by an Agent running `author`.
/// Own Agents come first among different-model reviewers (their approval is
/// in this ledger); among the same-model fallbacks another owner's Agent
/// comes first, the more independent review. `complete`: every Agent joined
/// to the room was seen with what its profile or roster row says, so "No
/// Codex reviewer" is true. `None` when nobody else can review.
pub(crate) fn pick(author: Option<&str>, candidates: &[Candidate], prefer_different: bool, complete: bool) -> Option<Pick> {
    let own_first = ordered(candidates, true);
    let first = *own_first.first()?;
    let Some(author) = author.filter(|_| prefer_different) else { return Some(quiet(first)) };
    if let Some(other) = own_first.iter().find(|c| c.runtime.is_some_and(|r| r != author)) {
        return Some(quiet(other));
    }
    let fallback = *ordered(candidates, false).first()?;
    let template = if complete { NO_OTHER_MODEL } else { NOT_ALL_SEEN };
    let args = [("wanted", wanted(author).to_owned()), ("name", fallback.name.clone())];
    Some(Pick { reviewer: fallback.puppet.clone(), notice: Some(crate::i18n::format(template, &args)), own: fallback.own })
}

/// `candidates` with this owner's Agents first (`own_first`) or last.
fn ordered(candidates: &[Candidate], own_first: bool) -> Vec<&Candidate> {
    candidates.iter().filter(|c| c.own == own_first).chain(candidates.iter().filter(|c| c.own != own_first)).collect()
}

fn quiet(candidate: &Candidate) -> Pick {
    Pick { reviewer: candidate.puppet.clone(), notice: None, own: candidate.own }
}

/// Other Agents joined to the room, from what their profiles say
/// (`Err`: unreadable), sorted so every check picks the same one; `false`
/// when some profile could not be read, so the room was not fully seen.
pub(crate) fn from_profiles(mut read: Vec<(OwnedUserId, String, Result<Option<&'static str>, String>)>) -> (Vec<Candidate>, bool) {
    read.sort_by(|a, b| a.0.cmp(&b.0));
    let complete = read.iter().all(|(_, _, runtime)| runtime.is_ok());
    let candidates = read.into_iter().map(|(puppet, name, runtime)| Candidate { puppet, name, runtime: runtime.ok().flatten(), own: false }).collect();
    (candidates, complete)
}

pub const NO_OTHER_MODEL: &str = "No {wanted} reviewer in the room; asked {name}";
/// Said instead when some Agent in the room could not be checked.
pub const NOT_ALL_SEEN: &str = "Could not check every Agent in the room for a {wanted} reviewer; asked {name}";

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
        assert_eq!(pick(Some("claude"), &room, true, true), Some(Pick { reviewer: room[1].puppet.clone(), notice: None, own: false }), "the other owner's Codex Agent");
        let reverse = [agent("anna-codex", Some("codex"), true), agent("bo-claude", Some("claude"), false)];
        assert_eq!(pick(Some("codex"), &reverse, true, true).unwrap().reviewer, reverse[1].puppet, "and the reverse");
        let both = [agent("bo-codex", Some("codex"), false), agent("yue-codex", Some("codex"), true)];
        assert_eq!(pick(Some("claude"), &both, true, true).unwrap().reviewer, both[1].puppet, "own first among different-model reviewers");
    }

    #[test]
    fn with_no_other_model_another_owners_agent_is_asked_first_and_it_says_so() {
        let room = [agent("anna-claude", Some("claude"), true), agent("bo-claude", None, false)];
        let picked = pick(Some("claude"), &room, true, true).unwrap();
        assert_eq!(picked.reviewer, room[1].puppet, "a same-model review by another owner is the more independent one");
        assert_eq!(picked.notice.as_deref(), Some("No Codex reviewer in the room; asked bo-claude"));
        assert!(!picked.own);
        let codex_only = [agent("bo-codex", Some("codex"), true)];
        assert_eq!(pick(Some("codex"), &codex_only, true, true).unwrap().notice.as_deref(), Some("No Claude reviewer in the room; asked bo-codex"));
    }

    /// Nov 18, dev room: anna-claude opened PR #109; the roster read timed
    /// out, and the room said "No Codex reviewer" while bird-codex (another
    /// owner, another server) was joined.
    #[test]
    fn a_joined_agent_of_another_server_is_seen_from_its_profile() {
        let mut room = vec![agent("yue-claude", Some("claude"), true)];
        let bird = OwnedUserId::try_from("@_hagency_agt_bird:localhost:8452").unwrap();
        let (members, complete) = from_profiles(vec![(bird.clone(), "bird-codex".into(), Ok(Some("codex")))]);
        room.extend(members);
        assert_eq!(pick(Some("claude"), &room, true, complete), Some(Pick { reviewer: bird, notice: None, own: false }));
    }

    #[test]
    fn an_unchecked_agent_never_makes_the_room_say_no_reviewer() {
        let mut room = vec![agent("yue-claude", Some("claude"), true)];
        let (members, complete) = from_profiles(vec![(OwnedUserId::try_from("@_hagency_agt_b:beta").unwrap(), "bird-codex".into(), Err("timeout".into()))]);
        assert!(!complete);
        room.extend(members);
        let picked = pick(Some("claude"), &room, true, complete).unwrap();
        assert_eq!(picked.notice.as_deref(), Some("Could not check every Agent in the room for a Codex reviewer; asked bird-codex"));
        let alone = [agent("yue-claude", Some("claude"), true)];
        assert_eq!(pick(Some("claude"), &alone, true, false).unwrap().notice.as_deref(), Some("Could not check every Agent in the room for a Codex reviewer; asked yue-claude"));
    }

    #[test]
    fn profiles_are_sorted_so_every_check_picks_the_same_one() {
        let id = |s: &str| OwnedUserId::try_from(s).unwrap();
        let (members, complete) = from_profiles(vec![(id("@z:b"), "z".into(), Ok(None)), (id("@a:b"), "a".into(), Ok(Some("claude")))]);
        assert!(complete);
        assert_eq!(members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "z"]);
        assert!(members.iter().all(|c| !c.own));
    }

    #[test]
    fn turned_off_or_unknown_it_asks_the_first_agent_quietly() {
        let room = [agent("anna-claude", Some("claude"), true), agent("bo-codex", Some("codex"), false)];
        assert_eq!(pick(Some("claude"), &room, false, true), Some(Pick { reviewer: room[0].puppet.clone(), notice: None, own: room[0].own }));
        assert_eq!(pick(None, &room, true, false), Some(Pick { reviewer: room[0].puppet.clone(), notice: None, own: room[0].own }));
        assert_eq!(pick(Some("claude"), &[], true, true), None, "nobody else: a person reviews");
    }
}
