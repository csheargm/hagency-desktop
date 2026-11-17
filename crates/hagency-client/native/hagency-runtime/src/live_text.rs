//! A bounded latest-text view for chat presentation. It has no tool, approval
//! or accounting authority, and a slow UI never blocks native inference.
use std::sync::{Arc, Mutex};
pub const MAX_PREVIEW_BYTES: usize = 4096;
/// The step a run shows while a tool call waits for its owner's approval.
/// Every viewer of the run card sees it (the requester too), and a desktop
/// shows it in its own words; it says only that the run waits, never what.
pub const APPROVAL_WAIT_STEP: &str = "waiting for the owner's approval";
/// A complete native message is separate from the lossy presentation preview.
/// The host binds this sink to one authorized conversation and must durably
/// accept the message before returning. The identifier is native to this run.
pub trait MessageSink: Send + Sync {
    fn record(&self, id: &str, text: &str) -> Result<(), MessageError>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageError;
#[derive(Clone, Default)]
pub struct LiveText {
    state: Arc<Mutex<State>>,
    messages: Option<Arc<dyn MessageSink>>,
}
#[derive(Default)]
struct State {
    item: String,
    text: String,
    revision: u64,
    /// The current step in plain words and how many were shown (presentation only).
    step: String,
    steps: u32,
    /// Approval waits now open, and the step shown before the first one.
    waits: u32,
    before_wait: String,
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
    pub fn with_messages(messages: Arc<dyn MessageSink>) -> Self {
        Self { state: Default::default(), messages: Some(messages) }
    }
    pub fn has_messages(&self) -> bool { self.messages.is_some() }
    /// Returns whether a host accepted the message. An absent sink preserves
    /// the existing preview-only behavior for hosts that have not opted in.
    pub fn message(&self, id: &str, text: &str) -> Result<bool, MessageError> {
        let Some(messages) = &self.messages else { return Ok(false) };
        if id.is_empty() || id.len() > 2048 || id.chars().any(char::is_control)
            || text.len() > 1024 * 1024
        {
            return Err(MessageError);
        }
        if text.is_empty() { return Ok(false); }
        messages.record(id, text)?;
        Ok(true)
    }
    pub fn append(&self, item: &str, delta: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.text != text || state.item != item {
            state.item = item.into();
            state.text = text;
            state.revision = state.revision.saturating_add(1);
        }
    }
    /// Record the step the runtime is on now; an empty or repeated step is skipped.
    pub fn step(&self, step: &str) {
        let step = bounded(step.lines().next().unwrap_or("").trim());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if step.is_empty() || state.step == step {
            return;
        }
        state.step = step;
        state.steps = state.steps.saturating_add(1);
        state.revision = state.revision.saturating_add(1);
    }
    /// Show [`APPROVAL_WAIT_STEP`] until the returned guard drops (the owner
    /// answered, the request expired or was withdrawn); then the step shown
    /// before comes back unless a newer step replaced it. Not counted as a step.
    pub fn wait_for_approval(&self) -> ApprovalWait {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.waits == 0 {
            state.before_wait = std::mem::replace(&mut state.step, APPROVAL_WAIT_STEP.into());
            state.revision = state.revision.saturating_add(1);
        }
        state.waits = state.waits.saturating_add(1);
        ApprovalWait(self.state.clone())
    }
    /// The current step and the number of steps so far.
    pub fn current_step(&self) -> (String, u32) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.step.clone(), state.steps)
    }
    pub fn since(&self, previous: u64) -> Option<(u64, String)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.revision > previous).then(|| (state.revision, state.text.clone()))
    }
}

/// An open approval wait (see [`LiveText::wait_for_approval`]).
#[must_use]
pub struct ApprovalWait(Arc<Mutex<State>>);
impl Drop for ApprovalWait {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.waits = state.waits.saturating_sub(1);
        if state.waits == 0 && state.step == APPROVAL_WAIT_STEP {
            state.step = std::mem::take(&mut state.before_wait);
            state.revision = state.revision.saturating_add(1);
        }
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
    fn steps_are_counted_once_and_wake_the_reader() {
        let output = LiveText::default();
        output.step("running gh pr create");
        output.step("running gh pr create");
        output.step("");
        let (revision, _) = output.since(0).unwrap();
        assert_eq!(output.current_step(), ("running gh pr create".into(), 1));
        output.step("reading src/main.rs\nsecond line");
        assert!(output.since(revision).is_some());
        assert_eq!(output.current_step(), ("reading src/main.rs".into(), 2));
    }

    /// The requester's view of a run blocked on the owner: the card says it
    /// waits, without counting a step, and goes back when the owner answers.
    #[test]
    fn an_approval_wait_shows_while_open_and_restores_the_step() {
        let output = LiveText::default();
        output.step("running gh issue close 1");
        let (revision, _) = output.since(0).unwrap();
        let first = output.wait_for_approval();
        let second = output.wait_for_approval();
        assert!(output.since(revision).is_some(), "the card is refreshed");
        assert_eq!(output.current_step(), (APPROVAL_WAIT_STEP.into(), 1));
        drop(first);
        assert_eq!(output.current_step().0, APPROVAL_WAIT_STEP, "another request still waits");
        drop(second);
        assert_eq!(output.current_step(), ("running gh issue close 1".into(), 1));
        let wait = output.wait_for_approval();
        output.step("reading README.md");
        drop(wait);
        assert_eq!(output.current_step(), ("reading README.md".into(), 2), "a newer step stays");
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
