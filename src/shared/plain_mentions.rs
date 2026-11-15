//! Typed "@name" text becomes a real mention on send.
//!
//! Agents wake only on real mentions (a pill link plus an `m.mentions` entry),
//! so a plain "@fixer please …" was ignored unless the user went through the
//! "@" picker. At send time each plain `@token` is resolved against the room's
//! joined members; only an unambiguous match is converted. Zero or several
//! matches stay plain text: guessing could ping (or impersonate) the wrong user.

use matrix_sdk::ruma::{OwnedUserId, UserId};
use super::mention_text::{mention_label, mention_link};

/// A joined member a plain `@token` may resolve to.
pub(super) struct PlainMentionCandidate<'a> {
    pub user_id: &'a UserId,
    pub display_name: Option<&'a str>,
}

/// Rewrites unambiguous plain `@token`s in `markdown` into mention links.
/// Returns the new Markdown and the users it now mentions.
pub(super) fn resolve_plain_mentions(markdown: &str, members: &[PlainMentionCandidate<'_>]) -> (String, Vec<OwnedUserId>) {
    let mut output = String::with_capacity(markdown.len());
    let mut users = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find(['`', '[', '@', 'h']) {
        output.push_str(&rest[..start]);
        let previous = output.chars().last();
        let tail = &rest[start..];
        let skip = protected_len(tail, previous);
        if skip > 0 {
            output.push_str(&tail[..skip]);
            rest = &tail[skip..];
            continue;
        }
        let token = mention_token(tail, previous);
        match token.and_then(|token| unique_match(token, members)) {
            Some(member) => {
                let label = mention_label(member.user_id, member.display_name.unwrap_or(""));
                output.push_str(&mention_link(&label, member.user_id));
                users.push(member.user_id.to_owned());
                rest = &tail[1 + token.map_or(0, str::len)..];
            }
            None => {
                let ch_len = tail.chars().next().map_or(1, char::len_utf8);
                output.push_str(&tail[..ch_len]);
                rest = &tail[ch_len..];
            }
        }
    }
    output.push_str(rest);
    (output, users)
}

/// Length of a region that must be copied verbatim: code, an existing link, or a URL.
fn protected_len(tail: &str, previous: Option<char>) -> usize {
    if let Some(after) = tail.strip_prefix("```") {
        return after.find("```").map_or(tail.len(), |end| 3 + end + 3);
    }
    if let Some(after) = tail.strip_prefix('`') {
        let fence = 1 + after.chars().take_while(|c| *c == '`').count();
        let marker = &tail[..fence];
        return tail[fence..].find(marker).map_or(fence, |end| fence + end + fence);
    }
    if tail.starts_with('[') {
        return existing_link_len(tail).unwrap_or(0);
    }
    let at_word_start = previous.is_none_or(|c| !c.is_alphanumeric());
    if at_word_start && (tail.starts_with("http://") || tail.starts_with("https://")) {
        return tail.find(char::is_whitespace).unwrap_or(tail.len());
    }
    0
}

/// Length of a `[label](target)` link at the start of `tail`, if there is one.
fn existing_link_len(tail: &str) -> Option<usize> {
    let mut escaped = false;
    let close = tail.char_indices().skip(1).find(|&(_, c)| {
        let found = c == ']' && !escaped;
        escaped = c == '\\' && !escaped;
        found
    })?.0;
    let after = tail[close + 1..].strip_prefix('(')?;
    after.find(')').map(|end| close + 2 + end + 1)
}

/// The name after an `@` that starts a word, without trailing punctuation.
/// Full Matrix IDs (`@name:server`) and emails (`a@b.c`) are not tokens.
fn mention_token(tail: &str, previous: Option<char>) -> Option<&str> {
    let after = tail.strip_prefix('@')?;
    if previous.is_some_and(|c| c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '/')) {
        return None;
    }
    let end = after.find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '.' | '_'))).unwrap_or(after.len());
    let token = after[..end].trim_end_matches(['.', '-', '_']);
    let next = &after[token.len()..];
    let is_user_id = next.starts_with(':') && next[1..].starts_with(|c: char| !c.is_whitespace());
    (!token.is_empty() && !is_user_id).then_some(token)
}

/// The single member whose display name or localpart equals `token` (ignoring case).
fn unique_match<'m, 'a>(token: &str, members: &'m [PlainMentionCandidate<'a>]) -> Option<&'m PlainMentionCandidate<'a>> {
    let mut matches = members.iter().filter(|member| {
        let same = |name: &str| name.trim().to_lowercase() == token.to_lowercase();
        member.display_name.is_some_and(same) || same(member.user_id.localpart())
    });
    let first = matches.next()?;
    matches.all(|other| other.user_id == first.user_id).then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    fn members() -> Vec<PlainMentionCandidate<'static>> {
        vec![
            PlainMentionCandidate { user_id: user_id!("@_hagency_agt_fx1:a.server"), display_name: Some("fixer") },
            PlainMentionCandidate { user_id: user_id!("@_hagency_agt_bc2:a.server"), display_name: Some("ben-claude") },
            PlainMentionCandidate { user_id: user_id!("@alex:a.server"), display_name: Some("Sam") },
            PlainMentionCandidate { user_id: user_id!("@sam:b.server"), display_name: None },
        ]
    }
    fn resolve(text: &str) -> (String, Vec<OwnedUserId>) { resolve_plain_mentions(text, &members()) }
    const FIXER: &str = "[\\@fixer](https://matrix.to/#/@_hagency_agt_fx1:a.server)";

    #[test]
    fn single_match_converts_by_display_name_and_localpart() {
        let (md, users) = resolve("@Fixer please look");
        assert_eq!(md, format!("{FIXER} please look"));
        assert_eq!(users, vec![user_id!("@_hagency_agt_fx1:a.server").to_owned()]);
        let (md, users) = resolve("hi @_hagency_agt_bc2");
        assert!(md.contains("](https://matrix.to/#/@_hagency_agt_bc2:a.server)"), "{md}");
        assert_eq!(users.len(), 1);
    }

    #[test]
    fn ambiguous_and_unknown_stay_plain() {
        // "Sam" is one user's display name and another's localpart.
        assert_eq!(resolve("@sam hi @nobody"), ("@sam hi @nobody".to_string(), vec![]));
    }

    #[test]
    fn existing_pill_code_and_email_untouched() {
        for text in [FIXER.to_string(), "[@fixer](https://matrix.to/#/@x:a.server)".into(), "`@fixer`".into(), "```\n@fixer\n```".into(),
                     "mail fixer@a.server".into(), "see https://x.org/@fixer".into(), "@fixer:a.server".into()] {
            assert_eq!(resolve(&text), (text.clone(), vec![]), "{text}");
        }
    }

    #[test]
    fn multiple_mentions_and_trailing_punctuation() {
        let (md, users) = resolve("@fixer, @ben-claude: and @fixer.");
        assert_eq!(md, format!("{FIXER}, [\\@ben\\-claude](https://matrix.to/#/@_hagency_agt_bc2:a.server): and {FIXER}."));
        assert_eq!(users.len(), 3);
    }
}
