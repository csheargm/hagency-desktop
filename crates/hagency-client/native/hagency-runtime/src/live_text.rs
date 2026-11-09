//! A bounded latest-text view for chat presentation. It has no tool, approval
//! or accounting authority, and a slow UI never blocks native inference.
use std::sync::{Arc, Mutex};
pub const MAX_PREVIEW_BYTES: usize = 4096;
#[derive(Clone, Default)]
pub struct LiveText(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    item: String,
    text: String,
    revision: u64,
}
fn bounded(value: &str) -> String {
    let cut = value.len() > MAX_PREVIEW_BYTES;
    let mut start = if cut {
        value.len() - (MAX_PREVIEW_BYTES - 4)
    } else {
        0
    };
    while !value.is_char_boundary(start) {
        start += 1;
    }
    let text: String = value[start..]
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'))
        .collect();
    if cut { format!("…\n{text}") } else { text }
}

impl LiveText {
    pub fn append(&self, item: &str, delta: &str) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.item != item {
            state.item = item.into();
            state.text.clear();
        }
        // Bound before allocation, even when a single provider delta is large.
        let delta = bounded(delta);
        let text = bounded(&(state.text.clone() + &delta));
        if text != state.text {
            state.text = text;
            state.revision = state.revision.saturating_add(1);
        }
    }
    pub fn complete(&self, item: &str, text: &str) {
        let text = bounded(text);
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.text != text || state.item != item {
            state.item = item.into();
            state.text = text;
            state.revision = state.revision.saturating_add(1);
        }
    }
    pub fn since(&self, previous: u64) -> Option<(u64, String)> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (state.revision > previous).then(|| (state.revision, state.text.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_unicode_preview_is_bounded_and_completed_text_is_not_duplicated() {
        let output = LiveText::default();
        output.append("one", &"中文🙂\u{1b}".repeat(10_000));
        let (revision, text) = output.since(0).unwrap();
        assert!(text.len() <= MAX_PREVIEW_BYTES);
        assert!(text.starts_with("…\n"));
        assert!(!text.contains('\u{1b}'));
        output.complete("one", &text);
        assert!(output.since(revision).is_none());
        output.append("two", "下一条");
        let (_, text) = output.since(revision).unwrap();
        assert_eq!(text, "下一条");
    }

    #[test]
    fn unread_snapshots_do_not_accumulate_or_backpressure_a_provider() {
        let output = LiveText::default();
        for _ in 0..10_000 {
            output.append("one", "abcdef");
        }
        let (revision, text) = output.since(0).unwrap();
        assert!(text.len() <= MAX_PREVIEW_BYTES);
        output.complete("one", "final answer");
        assert_eq!(output.since(revision).unwrap().1, "final answer");
    }
}
