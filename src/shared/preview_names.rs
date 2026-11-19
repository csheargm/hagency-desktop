//! Raw Matrix user IDs in a one-line preview (the rooms list's latest
//! message) shown the way the timeline's mention pill shows them
//! (fix/loop-line-content): the member's display name, else the localpart,
//! never the full ID ("🔎 Review requested — yue-claude please review …").
use std::borrow::Cow;
use matrix_sdk::{Room, ruma::{OwnedUserId, UserId}};

/// The most IDs one preview resolves, so a pasted member list stays cheap.
const MAX_NAMED: usize = 8;

/// The Matrix user IDs written in `body`, each once, in order.
pub fn user_ids_in(body: &str) -> Vec<OwnedUserId> {
    let edge = |c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '"' | '\'' | '<' | '>' | '`');
    let mut out: Vec<OwnedUserId> = Vec::new();
    for word in body.split_whitespace() {
        let word = word.trim_start_matches(|c: char| edge(c) && c != '@').trim_end_matches(edge);
        if let Ok(id) = UserId::parse(word) && !out.contains(&id) && out.len() < MAX_NAMED {
            out.push(id);
        }
    }
    out
}

/// The longest name a preview writes in, in characters.
const MAX_NAME_CHARS: usize = 64;

/// A display name as one short line: every run of whitespace or control
/// characters is one space (GPT review: a name "Payment approved\nAlice"
/// became the preview's first line). Empty means the caller uses the localpart.
fn one_line_name(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect();
    words.join(" ").chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_owned()
}

/// The byte ranges of `body`'s inline code spans, backticks included, found
/// line by line the way the preview's Markdown step reads each trimmed line.
fn code_spans(body: &str) -> Vec<std::ops::Range<usize>> {
    use pulldown_cmark::{Event, Parser};
    let mut spans = Vec::new();
    let mut start = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let offset = start + line.len() - trimmed.len();
        for (event, range) in Parser::new(trimmed).into_offset_iter() {
            if let Event::Code(_) = event { spans.push(offset + range.start..offset + range.end); }
        }
        start += line.len();
    }
    spans
}

/// `body` (Markdown) with each ID of `names` as its pill label, written as
/// literal text: one line, capped, its punctuation escaped outside a code
/// span; inside one (where a backslash shows) the plain name, without
/// backticks so the span stays closed.
pub fn with_names<'a>(body: &'a str, names: &[(OwnedUserId, Option<String>)]) -> Cow<'a, str> {
    let mut sorted: Vec<_> = names.iter().filter(|(id, _)| body.contains(id.as_str())).collect();
    if sorted.is_empty() { return Cow::Borrowed(body); }
    // Longest first, so an ID that begins another is never cut out of it.
    sorted.sort_by_key(|(id, _)| std::cmp::Reverse(id.as_str().len()));
    let spans = code_spans(body);
    let (mut out, mut i) = (String::with_capacity(body.len()), 0);
    while let Some(c) = body[i..].chars().next() {
        let Some((id, name)) = sorted.iter().find(|(id, _)| body[i..].starts_with(id.as_str())) else {
            out.push(c);
            i += c.len_utf8();
            continue;
        };
        let name = name.as_deref().map(one_line_name);
        let label = super::mention_text::user_label(id, name.as_deref());
        if spans.iter().any(|span| span.contains(&i)) {
            let plain: String = label.chars().filter(|&c| c != '`').collect();
            // A name of only backticks must not empty the span: the localpart instead.
            out.push_str(if plain.trim().is_empty() { id.localpart() } else { &plain });
        }
        else { out.push_str(&super::mention_text::escape_punctuation(label)); }
        i += id.as_str().len();
    }
    Cow::Owned(out)
}

/// The display names in `room` of the IDs written in `body`.
pub async fn names_in(room: Option<&Room>, body: &str) -> Vec<(OwnedUserId, Option<String>)> {
    let mut out = Vec::new();
    for id in user_ids_in(body) {
        // An unreadable member is shown by its localpart (`with_names`), never its full ID.
        let name = match room {
            Some(room) => room.get_member_no_sync(&id).await.ok().flatten().and_then(|m| m.display_name().map(str::to_owned)),
            None => None,
        };
        out.push((id, name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const AGENT: &str = "@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:localhost:8452";

    #[test]
    fn ids_are_found_beside_punctuation_once_each() {
        let body = format!("🔎 Review requested — {AGENT} please review PR #131 (cc {AGENT}, @anna:hs.test).");
        let ids: Vec<String> = user_ids_in(&body).iter().map(|u| u.to_string()).collect();
        assert_eq!(ids, [AGENT, "@anna:hs.test"]);
        assert!(user_ids_in("mail a@b.test, or @ alone").is_empty());
    }

    #[test]
    fn an_id_becomes_its_name_else_its_localpart() {
        let agent = OwnedUserId::try_from(AGENT).unwrap();
        let body = format!("🔎 Review requested — {AGENT} please review PR #131");
        assert_eq!(with_names(&body, &[(agent.clone(), Some("yue-claude".into()))]), "🔎 Review requested — yue\\-claude please review PR #131");
        assert_eq!(with_names(&body, &[(agent, None)]), "🔎 Review requested — \\_hagency\\_agt\\_01m7k84kgg5a5mtm2epqhbpjb5 please review PR #131", "escaped for the Markdown step");
        assert_eq!(with_names("no ids here", &[]), "no ids here");
    }

    fn preview(name: &str) -> String {
        let agent = OwnedUserId::try_from(AGENT).unwrap();
        let msg = matrix_sdk::ruma::events::room::message::MessageType::text_plain(format!("🔎 Review requested — {AGENT} please review PR #131"));
        crate::thread_reply_preview::escaped_plain_line(&msg, &[(agent, Some(name.to_owned()))]).unwrap()
    }

    /// GPT review, Nov 19: names went into the Markdown step unsanitized.
    #[test]
    fn a_name_is_one_literal_line_and_never_hides_the_message() {
        assert_eq!(preview("Payment approved\nAlice"), "🔎 Review requested — Payment approved Alice please review PR #131");
        assert_eq!(preview("Payment\r\n\t\u{7}approved"), "🔎 Review requested — Payment approved please review PR #131");
        assert_eq!(preview("**x**"), "🔎 Review requested — **x** please review PR #131");
        assert_eq!(preview("[a](b)"), "🔎 Review requested — [a](b) please review PR #131");
        assert_eq!(preview("<b>"), "🔎 Review requested — &lt;b&gt; please review PR #131");
        assert_eq!(preview("# _y_"), "🔎 Review requested — # _y_ please review PR #131");
        assert_eq!(preview(" \n\t "), "🔎 Review requested — _hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5 please review PR #131", "an empty name is the localpart");
        let long = preview(&"n".repeat(200));
        assert!(long.contains(&format!("— {} please", "n".repeat(MAX_NAME_CHARS))), "{long}");
    }

    /// GPT review, Nov 19: a name in backticks showed "yue\\-claude" in the rooms list.
    #[test]
    fn a_name_in_a_code_span_is_plain_and_outside_one_escaped() {
        let agent = OwnedUserId::try_from(AGENT).unwrap();
        let names = [(agent, Some("yue-claude".into()))];
        let body = format!("ping `{AGENT}` and {AGENT}");
        assert_eq!(with_names(&body, &names), "ping `yue-claude` and yue\\-claude");
        let msg = matrix_sdk::ruma::events::room::message::MessageType::text_plain(body);
        assert_eq!(crate::thread_reply_preview::escaped_plain_line(&msg, &names).unwrap(), "ping yue-claude and yue-claude");
        let tick = [(OwnedUserId::try_from(AGENT).unwrap(), Some("a`b".into()))];
        assert_eq!(with_names(&format!("`{AGENT}` x"), &tick), "`ab` x", "a backtick in the name cannot close the span");
        assert_eq!(with_names(&format!("\\`{AGENT}`"), &names), "\\`yue\\-claude`", "an escaped backtick opens no span");
    }

    /// The rooms-list path from the body alone: IDs found in it, then named.
    fn preview_of(body: &str, name: &str) -> String {
        let names: Vec<_> = user_ids_in(body).into_iter().map(|id| (id, Some(name.to_owned()))).collect();
        let msg = matrix_sdk::ruma::events::room::message::MessageType::text_plain(body);
        crate::thread_reply_preview::escaped_plain_line(&msg, &names).unwrap()
    }

    /// GPT review, Nov 19: "`@yue:hs.test`" kept its backticks on the token,
    /// so the ID was never resolved; a name of only backticks emptied the span.
    #[test]
    fn a_compact_code_span_mention_is_resolved_and_never_emptied() {
        assert_eq!(user_ids_in("ping `@yue:hs.test`").iter().map(|u| u.to_string()).collect::<Vec<_>>(), ["@yue:hs.test"]);
        assert_eq!(preview_of("ping `@yue:hs.test` now", "yue-claude"), "ping yue-claude now");
        assert_eq!(preview_of("ping `@yue:hs.test` now", "```"), "ping yue now", "the localpart, not an empty span");
        assert_eq!(preview_of("ping `@yue:hs.test` now", "` `"), "ping yue now");
    }

    #[test]
    fn an_id_that_begins_a_longer_one_does_not_cut_it() {
        let short = OwnedUserId::try_from("@a:hs.test").unwrap();
        let long = OwnedUserId::try_from("@a:hs.test.org").unwrap();
        let named = with_names("@a:hs.test.org and @a:hs.test", &[(short, Some("Ann".into())), (long, Some("Al".into()))]);
        assert_eq!(named, "Al and Ann");
    }
}
