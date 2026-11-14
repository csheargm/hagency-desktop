//! The one-line preview of a thread's latest reply shown under the thread
//! root. Agents reply in Markdown, and rendering that as HTML in a two-line
//! box can leave only a list bullet ("ann1-claude: •"), so text replies are
//! reduced to their first readable line of plain text. The rooms list's
//! latest-message preview uses the same reduction.

use std::borrow::Cow;
use matrix_sdk::ruma::{events::{room::message::MessageType, AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent}, serde::Raw, UserId};
use matrix_sdk_ui::timeline::{MsgLikeKind, TimelineItemContent};
use crate::{event_preview::{text_preview_of_message, text_preview_of_timeline_item}, utils};

/// Longest preview kept, in characters, before an ellipsis.
const MAX_PREVIEW_CHARS: usize = 120;

/// The HTML preview (`<b>sender</b>: text`) of a latest reply with `msgtype`.
pub fn preview_of_message(msgtype: &MessageType, sender_id: &UserId, sender_name: &str) -> String {
    match escaped_plain_line(msgtype) {
        Some(line) => format!("<b>{}</b>: {}", htmlize::escape_text(sender_name), line),
        None => {
            let html = text_preview_of_message(msgtype, sender_id, sender_name).format_with(sender_name, true);
            match utils::replace_linebreaks_separators(&html, true) {
                Cow::Borrowed(_) => html,
                Cow::Owned(replaced) => replaced,
            }
        }
    }
}

/// The first readable line of a text or notice message, HTML-escaped.
fn escaped_plain_line(msgtype: &MessageType) -> Option<String> {
    match msgtype {
        MessageType::Text(text) => Some(text.body.as_str()),
        MessageType::Notice(notice) => Some(notice.body.as_str()),
        _ => None,
    }.map(first_plain_line).filter(|line| !line.is_empty()).map(|line| htmlize::escape_text(&line).into_owned())
}

/// The one-line plain text of a quoted (replied-to) text or notice message,
/// without an agent's usage footer; `None` for other message types.
pub fn reply_quote_line(msgtype: &MessageType, sender_id: &UserId) -> Option<String> {
    let (body, formatted) = match msgtype {
        MessageType::Text(text) => (text.body.as_str(), text.formatted.is_some()),
        MessageType::Notice(notice) => (notice.body.as_str(), notice.formatted.is_some()),
        _ => return None,
    };
    let usage = crate::shared::agent_usage::parse_usage(sender_id.localpart(), body, formatted);
    let line = first_plain_line(usage.as_ref().map_or(body, |u| u.answer));
    (!line.is_empty()).then_some(line)
}

/// The rooms-list preview of a room's latest event: the HTML text shown with
/// the sender, and the sender-less body used by the direct-chat list.
pub fn latest_event_preview(content: &TimelineItemContent, sender_id: &UserId, sender_name: &str) -> (String, Option<String>) {
    let line = match content {
        TimelineItemContent::MsgLike(msg_like) => match &msg_like.kind {
            MsgLikeKind::Message(msg) => escaped_plain_line(msg.msgtype()),
            _ => None,
        },
        _ => None,
    };
    if let Some(line) = line {
        return (format!("<b>{}</b>: {}", htmlize::escape_text(sender_name), line), Some(line));
    }
    let preview = text_preview_of_timeline_item(content, sender_id, sender_name);
    let direct = preview.direct_message_body().map(str::to_owned);
    (preview.format_with(sender_name, true), direct)
}

/// The preview of a raw latest-reply event, if it is a room message.
pub fn preview_of_raw_event(raw: &Raw<AnySyncTimelineEvent>, sender_name: &str) -> Option<String> {
    match raw.deserialize().ok()? {
        AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(ev))) =>
            Some(preview_of_message(&ev.content.msgtype, &ev.sender, sender_name)),
        _ => None,
    }
}

/// The first non-empty line of Markdown `body` as plain text, with block
/// markers (headings, quotes, list bullets, task boxes) and inline markup
/// removed, truncated with an ellipsis.
pub fn first_plain_line(body: &str) -> String {
    let line = body.lines()
        .map(|line| strip_inline(strip_block_markers(line.trim())))
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    truncate(&line)
}

fn strip_block_markers(mut line: &str) -> &str {
    if line.starts_with("```") || line.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | '•' | ' ')) {
        return "";
    }
    loop {
        let before = line;
        line = line.trim_start_matches('#').trim_start_matches('>').trim_start();
        for bullet in ["- ", "* ", "+ ", "• "] {
            line = line.strip_prefix(bullet).unwrap_or(line);
        }
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && line[digits..].starts_with(['.', ')']) {
            line = line[digits + 1..].trim_start();
        }
        for task in ["[ ] ", "[x] ", "[X] "] {
            line = line.strip_prefix(task).unwrap_or(line);
        }
        if line == before { return line.trim(); }
    }
}

/// Renders one Markdown line as plain text with pulldown-cmark: emphasis,
/// code and strike markers go, backslash escapes are resolved (`\-` → `-`),
/// links and images keep only their text, and code spans stay verbatim.
fn strip_inline(line: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};
    let mut out = String::new();
    for event in Parser::new_ext(line, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS) {
        match event {
            Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) | Event::Html(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            _ => {}
        }
    }
    out.trim().to_owned()
}

fn truncate(line: &str) -> String {
    if line.chars().count() <= MAX_PREVIEW_CHARS { return line.to_owned(); }
    let kept: String = line.chars().take(MAX_PREVIEW_CHARS - 1).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn a_bullet_list_reply_previews_its_first_item_not_a_lone_bullet() {
        let body = "- **Checked** the `build` config\n- Fixed [two tests](https://x.test)\n";
        let msg = MessageType::text_markdown(body);
        let preview = preview_of_message(&msg, user_id!("@ann1-claude:s"), "ann1-claude");
        assert_eq!(preview, "<b>ann1-claude</b>: Checked the build config");
    }

    #[test]
    fn markdown_blocks_are_stripped_to_readable_text() {
        assert_eq!(first_plain_line("\n\n## Summary\n"), "Summary");
        assert_eq!(first_plain_line("```rust\nlet x = 1;\n```"), "let x = 1;");
        assert_eq!(first_plain_line("---\n> 1. [x] ~~done~~ ![logo](a.png) ok"), "done logo ok");
        assert_eq!(first_plain_line("-\n•\n* \nreal text"), "real text");
        assert_eq!(first_plain_line("snake_case_name stays"), "snake_case_name stays");
    }

    #[test]
    fn long_lines_end_in_an_ellipsis() {
        let line = first_plain_line(&"word ".repeat(60));
        assert!(line.ends_with('…'));
        assert!(line.chars().count() <= MAX_PREVIEW_CHARS);
    }

    #[test]
    fn rooms_list_and_reply_quote_lines_skip_a_lone_bullet() {
        let msg = MessageType::text_markdown("- <b>Done</b> & checked\n- second\n");
        assert_eq!(escaped_plain_line(&msg).as_deref(), Some("&lt;b&gt;Done&lt;/b&gt; &amp; checked"));
        assert_eq!(escaped_plain_line(&MessageType::text_plain("-\n•\n")), None);
    }

    #[test]
    fn a_long_multiline_request_quotes_as_one_truncated_line() {
        let request = format!("{}\n\nSecond paragraph\n- item", "Please review ".repeat(20));
        let line = reply_quote_line(&MessageType::text_plain(request), user_id!("@alice:s")).unwrap();
        assert!(!line.contains('\n') && line.ends_with('…'));
        assert!(line.chars().count() <= MAX_PREVIEW_CHARS);
        let image = MessageType::Image(matrix_sdk::ruma::events::room::message::ImageMessageEventContent::plain(
            "a.png".into(), matrix_sdk::ruma::mxc_uri!("mxc://s/a").to_owned()));
        assert_eq!(reply_quote_line(&image, user_id!("@alice:s")), None);
    }

    #[test]
    fn plain_text_is_escaped_and_non_text_messages_keep_the_old_preview() {
        let msg = MessageType::text_plain("a <b> & c");
        assert_eq!(preview_of_message(&msg, user_id!("@a:s"), "A"), "<b>A</b>: a &lt;b&gt; &amp; c");
    }

    #[test]
    fn an_escaped_mention_link_quotes_as_its_display_name() {
        let body = "[@ann1\\-claude](https://matrix.to/#/@_hagency_agt_x:s) Summarize the discussion";
        let line = reply_quote_line(&MessageType::text_markdown(body), user_id!("@alice:s")).unwrap();
        assert_eq!(line, "@ann1-claude Summarize the discussion");
        assert_eq!(first_plain_line("snake\\_case \\*not em\\*"), "snake_case *not em*");
    }

    #[test]
    fn inline_code_keeps_its_backslashes() {
        assert_eq!(first_plain_line("Run `C:\\dir\\-x` now"), "Run C:\\dir\\-x now");
    }

    #[test]
    fn a_bullet_list_quotes_its_first_item() {
        let line = reply_quote_line(&MessageType::text_plain("- first \\- item\n- second"), user_id!("@a:s"));
        assert_eq!(line.as_deref(), Some("first - item"));
    }
}
