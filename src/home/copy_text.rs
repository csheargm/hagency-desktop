//! What "Copy Text" puts on the clipboard: the text the timeline shows.
//!
//! A rich message's plain `body` is the sender's Markdown source, where a
//! mention pill is `[\@name](https://matrix.to/#/@id)` and an Agent reply ends
//! in a `🔗` permalink line the reply view hides. Copy turns those back into
//! what the user saw. A plain-text message (or a membership/redaction preview)
//! is shown literally, so it is copied literally.

/// The clipboard text for a message whose fallback `body` is given.
/// `has_html`: the timeline rendered its formatted body, so pills were pills.
/// An Agent `sender`'s reply view drops the trailing permalink, so Copy does too.
pub(crate) fn copied_message_text(body: &str, has_html: bool, sender: &matrix_sdk::ruma::UserId) -> String {
    #[cfg(feature = "agent_chat")]
    let body: std::borrow::Cow<'_, str> = if crate::agent_chat::agents::is_agent_localpart(sender.localpart()) {
        crate::agent_chat::presentation::split_permalink(body).0
    } else { body.into() };
    #[cfg(not(feature = "agent_chat"))]
    let _ = sender;
    if has_html { crate::shared::mentions_as_names(&body) } else { body.to_string() }
}

#[cfg(test)]
mod tests {
    use super::copied_message_text;
    use matrix_sdk::ruma::{user_id, UserId};
    fn human() -> &'static UserId { user_id!("@chris:localhost") }
    fn agent() -> &'static UserId { user_id!("@_hagency_agt_01m7:localhost") }
    const PILL: &str = "[\\@bird\\-claude](https://matrix.to/#/@_hagency_agt_01m7:localhost:8452)";

    #[test]
    fn copy_text_of_a_message_with_a_mention_pill_is_the_visible_name() {
        let body = format!("{PILL} what are the issues in the repo?");
        assert_eq!(copied_message_text(&body, true, human()), "@bird-claude what are the issues in the repo?");
    }
    #[test]
    fn plain_text_messages_and_previews_are_copied_as_shown() {
        let literal = "[Alice](https://matrix.to/#/@alice:example.org) left: reason";
        assert_eq!(copied_message_text(literal, false, human()), literal);
    }
    #[test]
    fn code_and_ordinary_links_stay_but_pills_inside_text_become_names() {
        let body = format!("```\n{PILL}\n```\nsee [web](https://example.org) and {PILL}");
        let copied = copied_message_text(&body, true, human());
        assert!(copied.starts_with(&format!("```\n{PILL}\n```")), "{copied}");
        assert!(copied.ends_with("see [web](https://example.org) and @bird-claude"), "{copied}");
    }
    #[cfg(feature = "agent_chat")]
    #[test]
    fn agent_answer_copy_drops_the_hidden_permalink_line() {
        let body = format!("Done, {PILL}.\n\n🔗 https://matrix.to/#/!r:x/$e");
        assert_eq!(copied_message_text(&body, true, agent()), "Done, @bird-claude.");
        assert!(copied_message_text(&body, true, human()).contains("🔗"));
    }
}
