//! Presentation-only folding and incremental reveal. The Matrix body is never modified.
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use makepad_widgets::*;
use matrix_sdk::ruma::events::room::message::FormattedBody;
use matrix_sdk_ui::timeline::EventTimelineItem;
use unicode_segmentation::UnicodeSegmentation;
use crate::shared::html_or_plaintext::HtmlOrPlaintextWidgetExt;
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt;

const STALL: Duration = Duration::from_secs(300);

pub fn fold_preview(body: &str) -> Option<String> {
    if body.lines().count() <= 8 && body.graphemes(true).count() <= 1200 {
        return None;
    }
    let preview = body
        .trim_start()
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    if preview.is_empty() {
        return None;
    }
    Some(format!(
        "{}…",
        preview.graphemes(true).take(400).collect::<String>()
    ))
}

pub fn content_is_live(content: &serde_json::Value) -> bool {
    let content = content.get("m.new_content").unwrap_or(content);
    matches!(
        content.get("org.matrix.msc4357.live"),
        Some(serde_json::Value::Object(_)) | Some(serde_json::Value::Bool(true))
    )
}

pub fn event_is_live(event: &EventTimelineItem) -> bool {
    let edited = event.content().as_message().is_some_and(|m| m.is_edited());
    event
        .latest_edit_json()
        .or_else(|| (!edited).then(|| event.original_json()).flatten())
        .and_then(|raw| raw.get_field::<serde_json::Value>("content").ok())
        .flatten()
        .is_some_and(|content| content_is_live(&content))
}

#[derive(Default)]
struct ExpandedReplies(HashSet<String>);

pub fn clear_session(cx: &mut Cx) {
    cx.global::<ExpandedReplies>().0.clear();
}

pub fn event_is_fresh(event: &EventTimelineItem) -> bool {
    let timestamp = event
        .latest_edit_json()
        .and_then(|raw| raw.get_field::<u64>("origin_server_ts").ok())
        .flatten()
        .unwrap_or_else(|| u64::from(event.timestamp().0));
    super::approval::current_unix_time_millis().saturating_sub(timestamp) < 300_000
}

#[derive(Default)]
struct Reveal {
    target: String,
    visible_bytes: usize,
    live: bool,
    stalled: bool,
    changed: Option<Instant>,
}
impl Reveal {
    fn update(&mut self, body: &str, live: bool, fresh: bool, now: Instant) -> bool {
        if self.target == body && self.live == live {
            return false;
        }
        let shown = &self.target[..self.visible_bytes];
        let common_bytes: usize = shown
            .graphemes(true)
            .zip(body.graphemes(true))
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len())
            .sum();
        self.visible_bytes = if live && fresh {
            common_bytes
        } else {
            body.len()
        };
        self.target = body.to_owned();
        self.live = live;
        self.stalled = live && !fresh;
        self.changed = Some(now);
        true
    }
    fn tick(&mut self, now: Instant) {
        if self
            .changed
            .is_some_and(|time| now.duration_since(time) >= STALL)
        {
            self.stalled = self.live;
            self.visible_bytes = self.target.len();
            return;
        }
        let remaining = &self.target[self.visible_bytes..];
        let count = remaining.graphemes(true).count();
        // Catch up in about a second even after a large network batch.
        self.visible_bytes += remaining
            .graphemes(true)
            .take((count / 8).max(8))
            .map(str::len)
            .sum::<usize>();
    }
    fn active(&self) -> bool {
        self.live && !self.stalled
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let AnswerButton = NavigationBarButton {
        width: 36 height: 36 padding: 0
        tooltip_below: true
        draw_bg +: {color_hover: RINX_HOVER color_active: RINX_SELECTED border_radius: 8}
        icon := Icon {icon_walk: Walk{width: 18 height: 18} draw_icon.color: RINX_MUTED}
    }
    // The fold chevrons sit alone under a reply; keep them discreet.
    let FoldButton = AnswerButton {
        width: 24 height: 24
        draw_bg +: {border_radius: 6}
        icon := Icon {icon_walk: Walk{width: 12 height: 12} draw_icon.color: RINX_MUTED}
    }
    mod.widgets.AgentReply = #(AgentReply::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fit flow: Down spacing: 4
        agent_reply_body := HtmlOrPlaintext {selectable: true}
        stream_status := Label {
            visible: false width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        agent_reply_expand := FoldButton {visible: false icon.draw_icon.svg: ICON_CHEVRON_DOWN}
        agent_reply_collapse := FoldButton {visible: false icon.draw_icon.svg: ICON_CHEVRON_UP}
        // A fixed-height row: the actions appear on hover without moving the text below.
        answer_actions := View {
            width: Fill height: 36 flow: Right spacing: 6 align: Align{y: 0.5}
            answer_reply := AnswerButton {visible: false icon.draw_icon.svg: ICON_REPLY}
            answer_copy := AnswerButton {visible: false icon.draw_icon.svg: ICON_COPY}
            answer_forward := AnswerButton {visible: false icon.draw_icon.svg: ICON_ANSWER_FORWARD}
            answer_more := AnswerButton {visible: false icon.draw_icon.svg: ICON_MORE_VERT}
            answer_markdown := AnswerButton {visible: false icon.draw_icon.svg: ICON_MARKDOWN_FILE}
            answer_pdf := AnswerButton {visible: false icon.draw_icon.svg: ICON_PDF_FILE}
            answer_meta := Label {
                visible: false width: Fit height: Fit
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
            }
        }
    }
}

// TODO(R4): reply.rs is over budget; split the hover action bar from folding/reveal.
#[derive(Script, ScriptHook, Widget)]
pub struct AgentReply {
    #[deref]
    view: View,
    #[rust]
    identity: String,
    #[rust]
    reveal: Reveal,
    #[rust]
    formatted: Option<FormattedBody>,
    #[rust]
    timer: Timer,
    #[rust]
    phase: usize,
    #[rust]
    rendered: Option<(String, bool)>,
    /// The pointer is over this reply: its actions show.
    #[rust]
    hovered: bool,
    /// "More" is open: Markdown and PDF show next to it.
    #[rust]
    more_open: bool,
    /// A small grey line shown with the actions, e.g. the run's duration.
    #[rust]
    meta: String,
}
impl AgentReply {
    fn render(&mut self, cx: &mut Cx) {
        let active = self.reveal.active();
        // Do not offer an unfinished streaming fragment as a completed answer.
        let finished = !self.reveal.live && !self.reveal.target.is_empty();
        self.view.view(cx, ids!(answer_actions)).set_visible(cx, finished);
        let shown = finished && self.hovered;
        let more = shown && self.more_open;
        for (path, label, visible) in [
            (ids!(answer_actions.answer_reply), "Reply", shown),
            (ids!(answer_actions.answer_copy), "Copy", shown),
            (ids!(answer_actions.answer_forward), "Forward answer", shown),
            (ids!(answer_actions.answer_more), "More", shown),
            (ids!(answer_actions.answer_markdown), "Save Markdown", more),
            (ids!(answer_actions.answer_pdf), "Export PDF", more),
        ] {
            let button = self.view.navigation_bar_button(cx, path);
            button.set_visible(cx, visible);
            if let Some(mut button) = button.borrow_mut() {
                button.set_tooltip_text(crate::i18n::tr(label));
            }
        }
        let meta = self.view.label(cx, ids!(answer_actions.answer_meta));
        meta.set_visible(cx, shown && !self.meta.is_empty());
        meta.set_text(cx, &self.meta);
        let preview = (!active)
            .then(|| fold_preview(&self.reveal.target))
            .flatten();
        let expanded = cx.global::<ExpandedReplies>().0.contains(&self.identity);
        let folded = preview.is_some() && !expanded;
        let body = if active {
            &self.reveal.target[..self.reveal.visible_bytes]
        } else if folded {
            preview.as_deref().unwrap()
        } else {
            &self.reveal.target
        };
        let rich = !active && !folded;
        // Avoid reparsing the same HTML on every frame.
        let key = (body.to_owned(), rich);
        if self.rendered.as_ref() != Some(&key) {
            let content = self.view.html_or_plaintext(cx, ids!(agent_reply_body));
            // Folded and incrementally revealed bodies are Markdown fragments,
            // not raw text. Reparse the visible fragment (never slice HTML).
            let fragment = (!rich && self.formatted.is_some())
                .then(|| super::presentation::markdown_body(body)).flatten();
            crate::home::room_screen::populate_text_message_content(
                cx,
                &content,
                body,
                if rich { self.formatted.as_ref() } else { fragment.as_ref() },
                None,
                None,
                None,
                None,
            );
            self.rendered = Some(key);
        }
        for (path, visible, label) in [
            (ids!(agent_reply_expand), folded, "Show more"),
            (ids!(agent_reply_collapse), preview.is_some() && !folded, "Show less"),
        ] {
            let toggle = self.view.navigation_bar_button(cx, path);
            if toggle.visible() != visible {
                if let Some(mut button) = toggle.borrow_mut() {
                    button.animator_play(cx, ids!(hover.off));
                    cx.widget_action(button.widget_uid(), TooltipAction::HoverOut);
                }
                toggle.set_visible(cx, visible);
            }
            if let Some(mut button) = toggle.borrow_mut() {
                button.set_tooltip_text(crate::i18n::tr(label));
            }
        }
        let status = self.view.label(cx, ids!(stream_status));
        status.set_visible(cx, active || self.reveal.stalled);
        status.set_text(
            cx,
            &if self.reveal.stalled {
                crate::i18n::tr("Response paused; waiting for an update").to_owned()
            } else {
                format!(
                    "{}{}",
                    crate::i18n::tr("Receiving response"),
                    ".".repeat(self.phase % 3 + 1)
                )
            },
        );
    }
}
impl AgentReply {
    /// Shows the actions while the pointer is over the reply; leaving closes "More".
    fn set_hovered(&mut self, cx: &mut Cx, hovered: bool) {
        if self.hovered == hovered { return; }
        self.hovered = hovered;
        self.more_open &= hovered;
        self.render(cx);
        self.redraw(cx);
    }
}
impl Widget for AgentReply {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::MouseMove(e) if self.view.visible() => {
                let inside = self.view.area().rect(cx).contains(e.abs);
                self.set_hovered(cx, inside);
            }
            Event::MouseLeave(_) => self.set_hovered(cx, false),
            // Touch has no hover: a tap on the reply shows its actions.
            Event::TouchUpdate(e) if self.view.visible() => {
                let rect = self.view.area().rect(cx);
                if e.touches.iter().any(|t| t.state == makepad_widgets::event::TouchState::Stop && rect.contains(t.abs)) {
                    self.set_hovered(cx, true);
                }
            }
            _ => {}
        }
        if self.timer.is_event(event).is_some() {
            self.reveal.tick(Instant::now());
            self.phase += 1;
            self.timer = if self.reveal.active() {
                cx.start_timeout(0.1)
            } else {
                Timer::empty()
            };
            self.render(cx);
            self.redraw(cx);
        }
        if let Event::Actions(actions) = event {
            if self.view.visible() && self.view.navigation_bar_button(cx, ids!(answer_actions.answer_more)).clicked(actions) {
                self.more_open = !self.more_open;
                self.render(cx);
                self.redraw(cx);
            }
            if self.view.visible() && [ids!(agent_reply_expand), ids!(agent_reply_collapse)]
                .into_iter().any(|path| {
                    let button = self.view.navigation_bar_button(cx, path);
                    button.visible() && button.clicked(actions)
                })
            {
                let expanded = &mut cx.global::<ExpandedReplies>().0;
                if !expanded.remove(&self.identity) {
                    if expanded.len() >= 2048 {
                        expanded.clear();
                    }
                    expanded.insert(self.identity.clone());
                }
                self.render(cx);
                self.redraw(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.render(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}
impl AgentReplyRef {
    /// Actions use the parent Message's current event binding, never the folded preview.
    pub fn clicked_action(&self, cx: &mut Cx, actions: &Actions) -> Option<AnswerAction> {
        let inner = self.borrow()?;
        if !inner.view.visible() || inner.identity.is_empty() || inner.reveal.live || inner.reveal.target.is_empty() { return None; }
        [
            (ids!(answer_actions.answer_reply), AnswerAction::Reply),
            (ids!(answer_actions.answer_copy), AnswerAction::Copy),
            (ids!(answer_actions.answer_forward), AnswerAction::Forward),
            (ids!(answer_actions.answer_markdown), AnswerAction::Markdown),
            (ids!(answer_actions.answer_pdf), AnswerAction::Pdf),
        ].into_iter().find_map(|(path, action)| inner.view.navigation_bar_button(cx, path).clicked(actions).then_some(action))
    }
    pub fn populate(
        &self,
        cx: &mut Cx,
        identity: String,
        body: &str,
        formatted: Option<&FormattedBody>,
        live: bool,
        fresh: bool,
    ) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.view.set_visible(cx, true);
        if inner.identity != identity {
            // PortalList can reuse a row for an identical answer in another
            // room. Text equality alone must not preserve the old selection.
            inner.view.html_or_plaintext(cx, ids!(agent_reply_body)).clear_selection(cx);
            cx.stop_timer(inner.timer);
            inner.timer = Timer::empty();
            inner.identity = identity;
            inner.more_open = false;
            inner.reveal = Reveal::default();
            inner.rendered = None;
        }
        if inner.formatted.as_ref().map(|f| (&f.body, &f.format))
            != formatted.map(|f| (&f.body, &f.format))
        {
            inner.formatted = formatted.cloned();
            inner.rendered = None;
        }
        inner.reveal.update(body, live, fresh, Instant::now());
        if inner.reveal.active() && inner.timer.0 == 0 {
            inner.timer = cx.start_timeout(0.1);
        }
        if !inner.reveal.active() {
            cx.stop_timer(inner.timer);
            inner.timer = Timer::empty();
        }
        inner.render(cx);
    }
    /// A small grey line shown with the actions on hover (empty for none).
    pub fn set_meta(&self, cx: &mut Cx, meta: &str) {
        if let Some(mut inner) = self.borrow_mut() && inner.meta != meta {
            inner.meta = meta.to_owned();
            inner.render(cx);
        }
    }
    pub fn hide(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.view.html_or_plaintext(cx, ids!(agent_reply_body)).clear_selection(cx);
            cx.stop_timer(inner.timer);
            inner.timer = Timer::empty();
            inner.view.set_visible(cx, false);
            inner.identity.clear();
            inner.meta.clear();
            inner.more_open = false;
            inner.reveal = Reveal::default();
            inner.rendered = None;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// `Reply` quotes the answer in the composer (ADR 0024).
pub enum AnswerAction { Reply, Copy, Forward, Markdown, Pdf }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn answer_actions_survive_folding_and_stop_for_hidden_or_streaming_rows() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let reply = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.AgentReply {}});
            let widget = WidgetRef::script_from_value(vm, value);
            widget.register_as_ui_root(vm);
            widget.as_agent_reply()
        });
        let body = format!("# 长答案\n{}\nFULL-END", "正文\n".repeat(100));
        reply.populate(&mut cx, "account|room|first".into(), &body, None, false, false);
        for (path, expected) in [
            (ids!(answer_actions.answer_reply), AnswerAction::Reply),
            (ids!(answer_actions.answer_copy), AnswerAction::Copy),
            (ids!(answer_actions.answer_forward), AnswerAction::Forward),
            (ids!(answer_actions.answer_markdown), AnswerAction::Markdown),
            (ids!(answer_actions.answer_pdf), AnswerAction::Pdf),
        ] {
            let uid = reply.borrow().unwrap().view.navigation_bar_button(&mut cx, path).widget_uid();
            let actions = cx.capture_actions(|cx| cx.widget_action(uid, crate::shared::navigation_bar_button::NavigationBarButtonAction::Clicked));
            assert_eq!(reply.clicked_action(&mut cx, &actions), Some(expected));
        }
        assert_eq!(reply.borrow().unwrap().reveal.target, body);
        assert!(reply.borrow().unwrap().view.view(&mut cx, ids!(answer_actions)).visible());
        let uid = reply.borrow().unwrap().view.navigation_bar_button(&mut cx, ids!(answer_actions.answer_pdf)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, crate::shared::navigation_bar_button::NavigationBarButtonAction::Clicked));
        reply.populate(&mut cx, "account|other-room|stream".into(), "partial", None, true, true);
        assert_eq!(reply.clicked_action(&mut cx, &actions), None);
        assert!(!reply.borrow().unwrap().view.view(&mut cx, ids!(answer_actions)).visible());
        reply.hide(&mut cx);
        assert_eq!(reply.clicked_action(&mut cx, &actions), None);
        reply.populate(&mut cx, "other-account|room|final".into(), "new answer", None, false, false);
        assert_eq!(reply.borrow().unwrap().reveal.target, "new answer");
        assert!(reply.borrow().unwrap().view.view(&mut cx, ids!(answer_actions)).visible());
    }
    /// Quiet replies: no actions until the pointer is over the reply; Copy and
    /// Forward then, Markdown and PDF behind "More", and the duration in grey.
    #[test]
    fn actions_show_on_hover_and_exports_sit_behind_more() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let reply = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.AgentReply {}});
            let widget = WidgetRef::script_from_value(vm, value);
            widget.register_as_ui_root(vm);
            widget.as_agent_reply()
        });
        reply.populate(&mut cx, "a|r|e".into(), "answer", None, false, false);
        reply.set_meta(&mut cx, "4 s");
        let visible = |cx: &mut Cx, path: &[LiveId]| reply.borrow().unwrap().view.widget(cx, path).visible();
        for path in [ids!(answer_actions.answer_copy), ids!(answer_actions.answer_forward), ids!(answer_actions.answer_more),
            ids!(answer_actions.answer_markdown), ids!(answer_actions.answer_meta)] {
            assert!(!visible(&mut cx, path), "{path:?} shows without hover");
        }
        reply.borrow_mut().unwrap().set_hovered(&mut cx, true);
        for path in [ids!(answer_actions.answer_reply), ids!(answer_actions.answer_copy), ids!(answer_actions.answer_forward), ids!(answer_actions.answer_more), ids!(answer_actions.answer_meta)] {
            assert!(visible(&mut cx, path), "{path:?} hidden on hover");
        }
        assert!(!visible(&mut cx, ids!(answer_actions.answer_markdown)) && !visible(&mut cx, ids!(answer_actions.answer_pdf)));
        let uid = reply.borrow().unwrap().view.navigation_bar_button(&mut cx, ids!(answer_actions.answer_more)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, crate::shared::navigation_bar_button::NavigationBarButtonAction::Clicked));
        reply.borrow_mut().unwrap().handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
        assert!(visible(&mut cx, ids!(answer_actions.answer_markdown)) && visible(&mut cx, ids!(answer_actions.answer_pdf)), "More opens the exports");
        reply.borrow_mut().unwrap().set_hovered(&mut cx, false);
        assert!(!visible(&mut cx, ids!(answer_actions.answer_copy)));
        reply.borrow_mut().unwrap().set_hovered(&mut cx, true);
        assert!(!visible(&mut cx, ids!(answer_actions.answer_markdown)), "leaving closes More");
    }
    #[test]
    fn folded_and_expanded_agent_reply_both_use_rich_text() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let reply=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value=script_eval!(vm,{mod.widgets.AgentReply {}});
            let widget=WidgetRef::script_from_value(vm,value);
            widget.register_as_ui_root(vm);
            widget.as_agent_reply()
        });
        let body=format!("## 调研\n\n**结论**\n\n{}", "正文\n".repeat(12));
        let rich=super::super::presentation::markdown_body(&body).unwrap();
        reply.populate(&mut cx,"fixture".into(),&body,Some(&rich),false,false);
        let mut inner=reply.borrow_mut().unwrap();
        assert!(inner.view.navigation_bar_button(&mut cx,ids!(agent_reply_expand)).visible());
        assert!(inner.view.view(&mut cx,ids!(agent_reply_body.html_view)).visible());
        assert!(!inner.view.view(&mut cx,ids!(agent_reply_body.plaintext_view)).visible());
        for (path, expanded) in [(ids!(agent_reply_expand), true), (ids!(agent_reply_collapse), false)] {
            let uid = inner.view.navigation_bar_button(&mut cx, path).widget_uid();
            let actions = cx.capture_actions(|cx| cx.widget_action(uid, crate::shared::navigation_bar_button::NavigationBarButtonAction::Clicked));
            inner.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
            assert_eq!(cx.global::<ExpandedReplies>().0.contains("fixture"), expanded);
            assert_eq!(inner.view.navigation_bar_button(&mut cx, ids!(agent_reply_expand)).visible(), !expanded);
            assert_eq!(inner.view.navigation_bar_button(&mut cx, ids!(agent_reply_collapse)).visible(), expanded);
            assert!(inner.view.view(&mut cx,ids!(agent_reply_body.html_view)).visible());
            assert_eq!(inner.reveal.target,body);
        }
    }
    #[test]
    fn folding_is_bounded_and_preserves_graphemes() {
        assert!(fold_preview("short 中文").is_none());
        let body = "👨‍👩‍👧‍👦中文".repeat(500);
        let preview = fold_preview(&body).unwrap();
        assert_eq!(preview.graphemes(true).count(), 401);
        assert!(body.starts_with(preview.trim_end_matches('…')));
        assert_eq!(
            fold_preview(&"line\n".repeat(10)).unwrap(),
            "line\nline\nline…"
        );
    }
    #[test]
    fn reveal_handles_rewrites_completion_stalls_and_historical_messages() {
        let now = Instant::now();
        let mut r = Reveal::default();
        r.update("Hello 👨‍👩‍👧‍👦 中文", true, true, now);
        r.tick(now);
        r.update("Hi 中文", true, true, now);
        r.tick(now);
        assert!(r.target.is_char_boundary(r.visible_bytes));
        r.update("Final answer 中文", false, true, now);
        assert_eq!(r.visible_bytes, r.target.len());
        assert!(!r.active());
        r.update("unfinished", true, true, now);
        r.tick(now + STALL);
        assert!(r.stalled);
        assert!(!r.active());
        r.update("old live text", true, false, now);
        assert_eq!(r.visible_bytes, r.target.len());
        assert!(!r.active());
    }
    #[test]
    fn final_edit_removes_the_live_marker() {
        assert!(content_is_live(
            &serde_json::json!({"org.matrix.msc4357.live": {}})
        ));
        assert!(!content_is_live(
            &serde_json::json!({"org.matrix.msc4357.live": true, "m.new_content": {"body":"done"}})
        ));
        assert!(!content_is_live(
            &serde_json::json!({"org.matrix.msc4357.live": "invalid"})
        ));
    }
}
