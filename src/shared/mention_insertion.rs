//! Where an auto-complete choice (user mention, room link or slash command)
//! goes in the composer text.
//!
//! Only the typed trigger token — from the trigger character up to the cursor —
//! is replaced. Text after the cursor belongs to the user and is never removed:
//! typing `@ann` directly in front of `show` and picking `@ann4agent` must give
//! `@ann4agent show`, not swallow `show`.

use std::ops::Range;

/// The edit that puts a chosen completion into the composer text.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CompletionEdit {
    /// Byte range of the typed trigger token that is replaced.
    pub range: Range<usize>,
    /// The completion followed by a separating space, unless whitespace follows already.
    pub text: String,
    /// Byte length of the completion itself, without the separator.
    pub label_len: usize,
    /// Byte index of the cursor after the edit: past the separating whitespace.
    pub cursor: usize,
}

/// Plans replacing the trigger token `text[trigger_start..cursor]` with `label`.
///
/// Returns `None` when the cursor no longer sits in that token (it moved before
/// the trigger, or whitespace now separates them), so nothing is replaced.
pub(super) fn plan_completion(text: &str, trigger_start: usize, cursor: usize, label: &str) -> Option<CompletionEdit> {
    let token = text.get(trigger_start..cursor).filter(|token| !token.is_empty())?;
    if token.contains(char::is_whitespace) {
        return None;
    }
    let next = text[cursor..].chars().next();
    let separator = if next.is_some_and(char::is_whitespace) { "" } else { " " };
    // Step over the separator, whether inserted here or already in the text.
    let separator_len = if separator.is_empty() { next.map_or(0, char::len_utf8) } else { separator.len() };
    Some(CompletionEdit {
        range: trigger_start..cursor,
        text: format!("{label}{separator}"),
        label_len: label.len(),
        cursor: trigger_start + label.len() + separator_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::mention_text::{HistoryMove, MentionText};

    const AGENT: &str = "@ann4agent:localhost:8450";

    /// Picks `label` with the cursor at `cursor`, then types `typed` there,
    /// one character at a time, and returns the visible text and the sent Markdown.
    fn pick_then_type(before: &str, cursor: usize, label: &str, typed: &str) -> (String, String) {
        let mut draft = MentionText::default();
        draft.reset(before);
        let trigger = before[..cursor].rfind('@').unwrap();
        let edit = plan_completion(before, trigger, cursor, label).expect("cursor is in the trigger token");
        let mut text = before.to_owned();
        text.replace_range(edit.range.clone(), &edit.text);
        draft.sync(&text, Some(edit.range.clone()), false, HistoryMove::Edit);
        draft.bind(trigger..trigger + edit.label_len, AGENT.try_into().unwrap());
        let mut at = edit.cursor;
        for c in typed.chars() {
            text.insert(at, c);
            draft.sync(&text, Some(at..at), false, HistoryMove::Edit);
            at += c.len_utf8();
        }
        let markdown = draft.markdown(text.trim());
        (text, markdown)
    }

    #[test]
    fn mention_pick_then_typing_keeps_every_following_word() {
        let (text, markdown) = pick_then_type("@ann", 4, "@ann4agent", "show the files");
        assert_eq!(text, "@ann4agent show the files");
        assert_eq!(markdown, format!("[\\@ann4agent](https://matrix.to/#/{AGENT}) show the files"));
    }

    #[test]
    fn mention_typed_directly_before_a_word_does_not_swallow_it() {
        // The draft already said "show the files"; "@ann" was typed in front of it.
        let (text, markdown) = pick_then_type("@annshow the files changed in PR #2", 4, "@ann4agent", "");
        assert_eq!(text, "@ann4agent show the files changed in PR #2");
        assert!(markdown.ends_with(") show the files changed in PR #2"), "{markdown}");
        // Last night's case: the next character must survive too.
        let (text, _) = pick_then_type("@annfile", 4, "@ann4agent", "");
        assert_eq!(text, "@ann4agent file");
    }

    #[test]
    fn mention_before_existing_space_reuses_it_and_keeps_following_text() {
        let (text, markdown) = pick_then_type("@ann show", 4, "@ann4agent", "x");
        assert_eq!(text, "@ann4agent xshow");
        assert!(markdown.ends_with(") xshow"), "{markdown}");
    }

    #[test]
    fn mention_non_ascii_query_and_label_keep_following_text() {
        let before = "请 @小黄问题";
        let cursor = before.find('问').unwrap();
        let (text, markdown) = pick_then_type(before, cursor, "@小黄助手", "");
        assert_eq!(text, "请 @小黄助手 问题");
        assert!(markdown.starts_with("请 [\\@小黄助手](https://matrix.to/#/") && markdown.ends_with(") 问题"), "{markdown}");
    }

    #[test]
    fn mention_completion_is_refused_once_the_cursor_left_the_trigger_token() {
        assert_eq!(plan_completion("@ann show", 0, 0, "@ann4agent"), None);
        assert_eq!(plan_completion("@ann show", 0, 6, "@ann4agent"), None);
        assert_eq!(plan_completion("@ann", 0, 9, "@ann4agent"), None);
    }
}
