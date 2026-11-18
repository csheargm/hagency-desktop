//! "Replying to <agent>" above a thread composer.
//!
//! A group room wakes an Agent only on an explicit mention (ADR-188 §3). In a
//! thread that is plainly a one-on-one with one Agent — the requester's own
//! root mentioned exactly that Agent and the Agent has answered in the thread —
//! having to type `@agent` again on every follow-up is a trap: a plain reply
//! reaches nobody. So the composer mentions the Agent for that requester, and
//! shows it, with a way to remove it.
//!
//! The rule is deliberately narrow and decided only from what the thread shows:
//! * only the root's own sender gets it; anyone else still has to mention;
//! * a root that mentions several Agents (or `@room`) gets nothing;
//! * a reply that mentions anyone else, or quotes someone else's message, is
//!   sent as typed.
//!
//! The mention is an ordinary `m.mentions` entry, exactly what typing the pill
//! would add, so every server-side rule (who may ask, consent, private Agents,
//! completed-task follow-ups) applies to it unchanged.
use makepad_widgets::*;
use matrix_sdk_ui::timeline::{MsgLikeKind, TimelineDetails, TimelineItem, TimelineItemContent};
use ruma::{
    OwnedUserId, UserId,
    events::{Mentions, room::message::{MessageType, RoomMessageEventContent}},
};
use std::sync::Arc;

use crate::sliding_sync::TimelineKind;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ThreadFollowupHint = #(ThreadFollowupHint::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
        padding: Inset{left: 10 right: 6 top: 2 bottom: 2}
        margin: Inset{bottom: 4}
        followup_text := Label {width: Fill height: Fit draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
        followup_remove := RobrixNeutralIconButton {
            text: #(crate::i18n::tr("Remove")) i18n_text: "Remove"
            height: 26 padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
            icon_walk: Walk{width: 0 height: 0} spacing: 0
        }
    }
}

/// The hint's words; `{0}` is the Agent's name.
pub const REPLYING_TO_TEXT: &str = "Replying to {0}";

/// The Agent a thread's plain replies continue with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowupTarget {
    pub agent: OwnedUserId,
    /// What the hint calls it: its display name, else its Matrix ID.
    pub name: String,
}

/// The one Agent `me`'s plain replies in this thread continue with, if any.
///
/// `root_sender`/`root_mentions` describe the thread root; `replies` are the
/// senders of the messages under it.
pub fn followup_agent<'a>(
    me: &UserId,
    root_sender: &UserId,
    root_mentions: Option<&Mentions>,
    replies: impl IntoIterator<Item = &'a UserId>,
    is_agent: impl Fn(&UserId) -> bool,
) -> Option<OwnedUserId> {
    if root_sender != me {
        return None;
    }
    let mentions = root_mentions?;
    if mentions.room {
        return None;
    }
    let mut agents = mentions.user_ids.iter().filter(|u| is_agent(u));
    let agent = agents.next()?;
    if agents.next().is_some() {
        return None;
    }
    let replied = replies.into_iter().any(|sender| sender == agent);
    replied.then(|| agent.clone())
}

/// Mentions `agent` in `message` unless the reply addresses someone else.
/// `reply_to` is the sender of the message being quoted, if any.
/// Returns whether the message now mentions `agent`.
pub fn address_followup(
    message: &mut RoomMessageEventContent,
    agent: &UserId,
    reply_to: Option<&UserId>,
) -> bool {
    if reply_to.is_some_and(|sender| sender != agent) {
        return false;
    }
    // Only an ordinary text reply: a `/command@agent`, `!command`, emote or
    // notice is addressed (or not) by its own rules and is sent as typed.
    let MessageType::Text(text) = &message.msgtype else { return false };
    if text.body.trim_start().starts_with(['/', '!']) {
        return false;
    }
    if let Some(mentions) = &message.mentions {
        if mentions.room || mentions.user_ids.iter().any(|u| u != agent) {
            return false;
        }
    }
    message.mentions.get_or_insert_with(Mentions::new).user_ids.insert(agent.to_owned());
    true
}

/// [`followup_agent`] for a loaded thread timeline, as the current user.
/// Nothing when the root is not loaded yet: the composer then sends as typed.
pub fn target_in(kind: &TimelineKind, items: &imbl::Vector<Arc<TimelineItem>>) -> Option<FollowupTarget> {
    let root_id = kind.thread_root_event_id()?;
    let me = crate::sliding_sync::current_user_id()?;
    let events = || items.iter().filter_map(|item| item.as_event());
    let root = events().find(|e| e.event_id() == Some(root_id.as_ref()))?;
    let message = |e: &matrix_sdk_ui::timeline::EventTimelineItem| match e.content() {
        TimelineItemContent::MsgLike(m) => match &m.kind { MsgLikeKind::Message(msg) => Some(msg.mentions().cloned()), _ => None },
        _ => None,
    };
    let replies: Vec<_> = events()
        .filter(|e| e.event_id() != Some(root_id.as_ref()) && message(e).is_some())
        .collect();
    let agent = followup_agent(&me, root.sender(), message(root).flatten().as_ref(),
        replies.iter().map(|e| e.sender()), crate::profile::profile_route::is_agent)?;
    let name = replies.iter().rev().find(|e| e.sender() == agent)
        .and_then(|e| match e.sender_profile() { TimelineDetails::Ready(p) => p.display_name.clone(), _ => None })
        .unwrap_or_else(|| agent.to_string());
    Some(FollowupTarget { agent, name })
}

#[derive(Script, ScriptHook, Widget)]
pub struct ThreadFollowupHint {
    #[deref] view: View,
    #[rust] target: Option<FollowupTarget>,
    /// The user removed the hint for this target's Agent.
    #[rust] removed: bool,
    /// The draft quotes someone other than the Agent, so it addresses them.
    #[rust] quoting_other: bool,
}

impl Widget for ThreadFollowupHint {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(followup_remove)).clicked(actions) { self.remove(cx); }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ThreadFollowupHint {
    /// The reply is sent as typed until the thread or its Agent changes.
    fn remove(&mut self, cx: &mut Cx) {
        self.removed = true;
        self.refresh(cx);
    }

    fn active(&self) -> Option<&FollowupTarget> {
        self.target.as_ref().filter(|_| !self.removed && !self.quoting_other)
    }

    fn refresh(&mut self, cx: &mut Cx) {
        let shown = self.active().is_some();
        self.view.set_visible(cx, shown);
        self.view.redraw(cx);
    }
}

impl ThreadFollowupHintRef {
    /// Shows `target`, or hides the hint. Only a different Agent (or none)
    /// clears a removal: a name that resolves later is the same Agent.
    pub fn set_target(&self, cx: &mut Cx, target: Option<FollowupTarget>) {
        let Some(mut inner) = self.borrow_mut() else { return };
        if inner.target == target { return; }
        if let Some(t) = &target {
            let text = crate::i18n::tr(REPLYING_TO_TEXT).replace("{0}", &t.name);
            inner.view.label(cx, ids!(followup_text)).set_text(cx, &text);
        }
        if inner.target.as_ref().map(|t| &t.agent) != target.as_ref().map(|t| &t.agent) {
            inner.removed = false;
        }
        inner.target = target;
        inner.refresh(cx);
    }

    /// The sender of the message the draft quotes, if any. Quoting someone
    /// other than the Agent addresses them, so the hint steps aside.
    pub fn set_quoting(&self, cx: &mut Cx, quoted: Option<&UserId>) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let other = quoted.is_some_and(|q| inner.target.as_ref().is_some_and(|t| t.agent != q));
        if inner.quoting_other == other { return; }
        inner.quoting_other = other;
        inner.refresh(cx);
    }

    /// The Agent a plain reply should mention now: shown and not removed.
    pub fn active(&self) -> Option<OwnedUserId> {
        self.borrow()?.active().map(|t| t.agent.clone())
    }
}

#[cfg(test)]
#[path = "thread_followup_tests.rs"]
mod tests;
