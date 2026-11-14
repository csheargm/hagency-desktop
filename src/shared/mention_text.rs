//! Visible @names with identity kept outside the editable text. Ranges, rather
//! than name lookups, distinguish two people with the same display name.
use std::ops::Range;
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};
use ruma::{matrix_uri::MatrixId, MatrixToUri, OwnedUserId, UserId};

pub(super) fn user_label<'a>(id: &'a UserId, name: Option<&'a str>) -> &'a str {
    name.map(str::trim).filter(|name| !name.is_empty() && *name != id.as_str() && !name.starts_with("https://matrix.to/"))
        .unwrap_or_else(|| id.localpart())
}

pub(super) fn mention_label(id: &UserId, name: &str) -> String {
    format!("@{}", user_label(id, Some(name)).trim_start_matches('@'))
}

#[derive(Clone, Debug, PartialEq)]
struct Mention {
    range: Range<usize>,
    label: String,
    user: OwnedUserId,
}

#[derive(Clone, Default)]
struct Checkpoint {
    // Store a digest, not a copy of the whole draft on every keystroke.
    text_hash: [u8; 32],
    mentions: Vec<Mention>,
}

#[derive(Clone, Copy, Default)]
pub(super) enum HistoryMove {
    #[default]
    Edit,
    Undo,
    Redo,
}

#[derive(Clone, Default)]
pub(super) struct MentionText {
    text: String,
    current: Checkpoint,
    undo: Vec<Checkpoint>,
    redo: Vec<Checkpoint>,
}

impl MentionText {
    pub fn text(&self) -> &str { &self.text }

    pub fn users(&self) -> impl Iterator<Item = &OwnedUserId> {
        self.current.mentions.iter().map(|m| &m.user)
    }

    /// Import existing Matrix Markdown (including an older saved message) into
    /// the compact composer. Code blocks and unrelated links remain unchanged.
    pub fn reset(&mut self, markdown: &str) {
        let mut links = Vec::new();
        let mut active: Option<(Range<usize>, OwnedUserId, String)> = None;
        for (event, range) in Parser::new(markdown).into_offset_iter() {
            match event {
                Event::Start(Tag::Link { link_type: LinkType::Inline, dest_url, .. }) => {
                    if let Ok(uri) = MatrixToUri::parse(&dest_url)
                        && let MatrixId::User(id) = uri.id()
                    {
                        active = Some((range, id.clone(), String::new()));
                    }
                }
                Event::Text(text) | Event::Code(text) => {
                    if let Some((_, _, label)) = &mut active { label.push_str(&text); }
                }
                Event::End(TagEnd::Link) => {
                    if let Some((mut start, user, label)) = active.take() {
                        start.end = range.end;
                        links.push((start, user, label));
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        let mut mentions = Vec::new();
        let mut end = 0;
        for (range, user, label) in links {
            text.push_str(&markdown[end..range.start]);
            let label = mention_label(&user, &label);
            let start = text.len();
            text.push_str(&label);
            mentions.push(Mention {range: start..text.len(), label, user});
            end = range.end;
        }
        text.push_str(&markdown[end..]);
        self.current = Checkpoint {text_hash: *blake3::hash(text.as_bytes()).as_bytes(), mentions};
        self.text = text;
        self.undo.clear();
        self.redo.clear();
    }

    /// Follow native edits, including grouped undo/redo. Only a real history
    /// operation can restore a deleted binding: typing a matching name cannot.
    pub fn sync(&mut self, text: &str, selection: Option<Range<usize>>, forward_delete: bool, movement: HistoryMove) {
        if self.text == text { return; }
        let hash = *blake3::hash(text.as_bytes()).as_bytes();
        let history = match movement {
            HistoryMove::Undo => Some((&mut self.undo, &mut self.redo)),
            HistoryMove::Redo => Some((&mut self.redo, &mut self.undo)),
            HistoryMove::Edit => None,
        };
        if let Some((past, future)) = history
            && let Some(index) = past.iter().rposition(|state| state.text_hash == hash)
        {
            let mut crossed = past.split_off(index);
            let restored = crossed.remove(0);
            future.push(std::mem::replace(&mut self.current, restored));
            future.extend(crossed.into_iter().rev());
            self.text = text.to_owned();
            return;
        }
        let (changed, inserted) = changed_range(&self.text, text, selection, forward_delete);
        self.undo.push(self.current.clone());
        self.redo.clear();
        self.current.mentions.retain_mut(|mention| {
            if mention.range.end <= changed.start {
                // Text after the mention changed.
            } else if mention.range.start >= changed.end {
                mention.range = (mention.range.start - changed.len() + inserted)
                    ..(mention.range.end - changed.len() + inserted);
            } else {
                return false; // Editing any part of a name removes its binding.
            }
            text.get(mention.range.clone()) == Some(mention.label.as_str())
                && text[..mention.range.start].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_')
                && text[mention.range.end..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
        self.current.text_hash = hash;
        self.text = text.to_owned();
    }

    /// Called only after autocomplete's undoable replacement succeeds.
    pub fn bind(&mut self, range: Range<usize>, user: OwnedUserId) {
        let Some(label) = self.text.get(range.clone()) else { return };
        self.current.mentions.retain(|m| m.range.end <= range.start || m.range.start >= range.end);
        self.current.mentions.push(Mention {range, label: label.to_owned(), user});
        self.current.mentions.sort_by_key(|m| m.range.start);
    }

    pub fn clear(&mut self) {
        self.current.mentions.clear();
        self.undo.clear();
        self.redo.clear();
    }

    /// Expand only selected, still-intact occurrences when serializing a
    /// message. The editor and clipboard continue to contain just @names.
    pub fn markdown(&self, visible: &str) -> String {
        let offset = if visible == self.text { 0 }
            else if visible == self.text.trim() { self.text.len() - self.text.trim_start().len() }
            else { return visible.to_owned() };
        let mut output = String::new();
        let mut end = offset;
        for mention in &self.current.mentions {
            if mention.range.start < offset || mention.range.end > offset + visible.len() { continue; }
            output.push_str(&self.text[end..mention.range.start]);
            output.push_str(&mention_link(&mention.label, &mention.user));
            end = mention.range.end;
        }
        output.push_str(&self.text[end..offset + visible.len()]);
        output
    }
}

/// The Markdown pill for one mention: `[label](matrix.to link)`, with the
/// label's ASCII punctuation escaped so it cannot open other Markdown.
pub fn mention_link(label: &str, user: &UserId) -> String {
    let mut output = String::from("[");
    for c in label.chars() {
        if c.is_ascii_punctuation() { output.push('\\'); }
        output.push(c);
    }
    output.push_str(&format!("]({})", user.matrix_to_uri()));
    output
}

fn changed_range(old: &str, new: &str, selection: Option<Range<usize>>, forward_delete: bool) -> (Range<usize>, usize) {
    let matches = |range: &Range<usize>| -> Option<usize> {
        if range.end > old.len() || !old.is_char_boundary(range.start) || !old.is_char_boundary(range.end) { return None; }
        let inserted = new.len().checked_sub(old.len() - range.len())?;
        (new.get(..range.start) == old.get(..range.start)
            && new.get(range.start + inserted..) == old.get(range.end..)).then_some(inserted)
    };
    if let Some(selection) = selection {
        if let Some(inserted) = matches(&selection) { return (selection, inserted); }
        if selection.is_empty() && old.len() > new.len() {
            let removed = old.len() - new.len();
            let range = if forward_delete { selection.start..selection.start + removed }
                else { selection.start.saturating_sub(removed)..selection.start };
            if let Some(inserted) = matches(&range) { return (range, inserted); }
        }
    }
    let start = old.chars().zip(new.chars()).take_while(|(a,b)| a==b).map(|(c,_)| c.len_utf8()).sum::<usize>();
    let suffix = old[start..].chars().rev().zip(new[start..].chars().rev())
        .take_while(|(a,b)| a==b).map(|(c,_)| c.len_utf8()).sum::<usize>();
    (start..old.len()-suffix, new.len()-start-suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn user(name: &str) -> OwnedUserId { format!("@{name}:example.org").try_into().unwrap() }
    fn selected(text: &str, range: Range<usize>, name: &str) -> MentionText {
        let mut draft = MentionText::default(); draft.reset(text); draft.bind(range,user(name)); draft
    }
    #[test]
    fn identical_names_keep_separate_recipients_through_delete_and_history() {
        let mut draft=selected("@Alex @Alex ",0..5,"first"); draft.bind(6..11,user("second"));
        draft.sync("@Alex ",Some(0..6),false,HistoryMove::Edit);
        assert_eq!(draft.users().collect::<Vec<_>>(), vec![&user("second")]);
        draft.sync("@Alex @Alex ",None,false,HistoryMove::Undo);
        assert_eq!(draft.users().count(),2);
        draft.sync("@Alex ",None,false,HistoryMove::Redo);
        assert_eq!(draft.users().collect::<Vec<_>>(), vec![&user("second")]);
    }
    #[test]
    fn unicode_edits_shift_ranges_but_retyping_a_deleted_name_does_not_notify() {
        let mut draft=selected("@小黄 问题",0..7,"agent");
        draft.sync("你好 @小黄 问题",Some(0..0),false,HistoryMove::Edit);
        assert!(draft.markdown(draft.text()).contains("@agent:example.org"));
        draft.sync("你好 @小白 问题",None,false,HistoryMove::Edit);
        draft.sync("你好 @小黄 问题",None,false,HistoryMove::Edit);
        assert_eq!(draft.users().count(),0);
        assert!(!draft.markdown(draft.text()).contains("matrix.to"));
    }
    #[test]
    fn markdown_roundtrip_escapes_names_and_keeps_code_and_non_user_links() {
        let mut draft=selected("@小黄 [A]* 请看",0..12,"agent");
        // The bound label deliberately contains Markdown syntax.
        let markdown=draft.markdown(draft.text());
        draft.reset(&markdown);
        assert_eq!(draft.text(),"@小黄 [A]* 请看");
        assert_eq!(draft.users().collect::<Vec<_>>(),vec![&user("agent")]);
        let code="`[Alex](https://matrix.to/#/@alex:example.org)` [web](https://example.org)";
        draft.reset(code);assert_eq!(draft.text(),code);assert_eq!(draft.users().count(),0);
    }
    #[test]
    fn old_messages_and_trimmed_composer_keep_ids_out_of_visible_names() {
        let mut draft=MentionText::default();
        draft.reset("  [小黄](https://matrix.to/#/@agent:example.org)  ");
        assert_eq!(draft.text(),"  @小黄  ");
        assert!(draft.markdown(draft.text().trim()).contains("@agent:example.org"));
        assert_eq!(user_label(&user("alex"),None),"alex");
    }
}
