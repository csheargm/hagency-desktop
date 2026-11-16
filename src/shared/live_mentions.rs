//! Composer-time mention feedback (pure rules).
//!
//! Send-time resolution (`plain_mentions`) already turns an unambiguous typed
//! `@name` into a real mention, but the user could not see that before
//! sending. These rules let the composer show it while typing:
//! - a live pill: `@name` + a terminator becomes the same pill the "@" picker
//!   inserts, when exactly one joined member matches;
//! - a not-in-room hint: `@name` matching no member but one of this user's
//!   own Agents offers to add that Agent.
use std::ops::Range;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use serde_json::Value;
use super::mention_text::mention_label;
use super::plain_mentions::{mention_token, typed_token_ranges, typed_tokens, unique_match, PlainMentionCandidate};

/// A typed `@name` the composer should replace with a pill right now.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct LivePill {
    /// The typed `@name`, without the terminator.
    pub range: Range<usize>,
    pub label: String,
    pub user: OwnedUserId,
    /// The lowercased name; once the user breaks this pill it is never re-pilled.
    pub key: String,
}

/// Whether typing `c` right after a name ends it. `:` may start a full Matrix
/// ID, `` ` `` may close a code span, and `-._` can be part of a name.
pub(super) fn is_terminator(c: char) -> bool {
    c.is_whitespace() || (c.is_ascii_punctuation() && !matches!(c, '-' | '_' | '.' | '@' | ':' | '`'))
}

/// The pill to insert after the user typed `inserted` ending at `cursor`.
/// Only a single typed terminator converts (never paste or deletion), only an
/// unambiguous member, and never a name in `declined`.
pub(super) fn live_pill(text: &str, cursor: usize, inserted: &str, members: &[PlainMentionCandidate<'_>], declined: &[String]) -> Option<LivePill> {
    let mut chars = inserted.chars();
    let (Some(end), None) = (chars.next(), chars.next()) else { return None };
    if !is_terminator(end) || !text.get(..cursor)?.ends_with(end) {
        return None;
    }
    let before = &text[..cursor - end.len_utf8()];
    let at = before.rfind('@')?;
    // An odd number of backticks before the name means it sits in a code span.
    if before[..at].matches('`').count() % 2 == 1 {
        return None;
    }
    let token = mention_token(&text[at..], before[..at].chars().next_back())?;
    if at + 1 + token.len() != before.len() {
        return None;
    }
    let key = token.to_lowercase();
    if declined.contains(&key) {
        return None;
    }
    let member = unique_match(token, members)?;
    let label = mention_label(member.user_id, member.display_name.unwrap_or(""));
    Some(LivePill { range: at..before.len(), label, user: member.user_id.to_owned(), key })
}

/// Drops live pills whose user is no longer bound in the draft (backspaced,
/// edited, undone) and returns their names, which then stay plain text.
pub(super) fn broken_pills(live: &mut Vec<(OwnedUserId, String)>, bound: &[&UserId]) -> Vec<String> {
    let mut broken = Vec::new();
    live.retain(|(user, key)| {
        let intact = bound.contains(&user.as_ref());
        if !intact { broken.push(key.clone()); }
        intact
    });
    broken
}

/// One of this user's own Agents, as the Agents listing names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnAgent {
    pub name: String,
    pub puppet: OwnedUserId,
}

/// The active Agents of a `Command::Agents` listing owned by `owner`.
pub(crate) fn own_agents(listing: &Value, owner: &UserId) -> Vec<OwnAgent> {
    if listing["ownerMxid"].as_str() != Some(owner.as_str()) {
        return Vec::new();
    }
    crate::hagency::bring_agent::active_agents_newest_first(listing).into_iter().filter_map(|row| {
        let puppet = OwnedUserId::try_from(row["puppetMxid"].as_str()?).ok()?;
        let name = row["displayName"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(puppet.localpart()).to_owned();
        Some(OwnAgent { name, puppet })
    }).collect()
}

/// The first typed `@name` that matches no room member but exactly one of
/// `agents` (by name or puppet localpart) whose puppet is not in the room.
pub(super) fn agent_not_in_room<'a>(text: &str, members: &[PlainMentionCandidate<'_>], agents: &'a [OwnAgent]) -> Option<&'a OwnAgent> {
    typed_tokens(text).into_iter().find_map(|token| {
        let same = |name: &str| name.trim().to_lowercase() == token.to_lowercase();
        if members.iter().any(|m| m.display_name.is_some_and(same) || same(m.user_id.localpart())) {
            return None;
        }
        let mut hits = agents.iter().filter(|a| same(&a.name) || same(a.puppet.localpart()));
        let agent = hits.next()?;
        let unique = hits.next().is_none();
        (unique && !members.iter().any(|m| m.user_id == agent.puppet)).then_some(agent)
    })
}

/// The typed `@name` (including `@`) that names `agent`, so the composer can
/// turn it into a real mention once the hint added that Agent to the room.
pub(super) fn agent_mention_range(text: &str, agent: &OwnAgent) -> Option<Range<usize>> {
    let same = |name: &str, token: &str| name.trim().to_lowercase() == token.to_lowercase();
    typed_token_ranges(text).into_iter()
        .find(|range| same(&agent.name, &text[range.clone()]) || same(agent.puppet.localpart(), &text[range.clone()]))
        .map(|range| range.start - 1..range.end)
}

#[cfg(test)]
#[path = "live_mentions_tests.rs"]
mod tests;
