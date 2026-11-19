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
    let edge = |c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '"' | '\'' | '<' | '>');
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

/// `body` (Markdown) with each ID of `names` as its pill label, written as
/// literal text: one line, capped, its punctuation escaped.
pub fn with_names<'a>(body: &'a str, names: &[(OwnedUserId, Option<String>)]) -> Cow<'a, str> {
    let mut sorted: Vec<_> = names.iter().collect();
    // Longest first, so an ID that begins another is never cut out of it.
    sorted.sort_by_key(|(id, _)| std::cmp::Reverse(id.as_str().len()));
    let mut out = Cow::Borrowed(body);
    for (id, name) in sorted {
        if out.contains(id.as_str()) {
            let name = name.as_deref().map(one_line_name);
            let label = super::mention_text::user_label(id, name.as_deref());
            out = Cow::Owned(out.replace(id.as_str(), &super::mention_text::escape_punctuation(label)));
        }
    }
    out
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

    #[test]
    fn an_id_that_begins_a_longer_one_does_not_cut_it() {
        let short = OwnedUserId::try_from("@a:hs.test").unwrap();
        let long = OwnedUserId::try_from("@a:hs.test.org").unwrap();
        let named = with_names("@a:hs.test.org and @a:hs.test", &[(short, Some("Ann".into())), (long, Some("Al".into()))]);
        assert_eq!(named, "Al and Ann");
    }
}
