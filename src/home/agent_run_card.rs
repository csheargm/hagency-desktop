use makepad_widgets::*;
use crate::shared::html_or_plaintext::HtmlOrPlaintextWidgetExt;
use crate::hagency::{
    run_controls::{self, Entry},
    pending_questions::PendingQuestionsChanged,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentRunCardBase = #(AgentRunCard::register_widget(vm))
    mod.widgets.AgentRunCard = set_type_default() do mod.widgets.AgentRunCardBase {
        visible: false
        width: Fill height: Fit flow: Down spacing: 8
        margin: Inset{top: 6 bottom: 6}
        padding: 12
        show_bg: true
        draw_bg +: {color: mod.widgets.RINX_FIELD}
        title := Label {
            width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: 12}}
        }
        status := Label {
            width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        preview := HtmlOrPlaintext { width: Fill height: Fit selectable: true }
        input := TextInput {
            width: Fill height: Fit
            empty_text: "追加指令…"
        }
        buttons := View {
            width: Fill height: Fit flow: Right spacing: 8
            append := Button {text: "追加到当前运行"}
            stop := Button {text: "停止本次运行"}
            restore := Button {text: "恢复未执行指令"}
        }
        hint := Label {
            width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        unconfirmed_notice := Label {
            visible: false width: Fill height: Fit
            text: "以下指令结果未确认，原文可复制。请先核对，避免重复执行。"
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        unconfirmed := ScrollYView {
            visible: false width: Fill height: 120
            text := HtmlOrPlaintext {width: Fill height: Fit selectable: true}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct AgentRunCard {
    #[deref]
    view: View,
    #[rust]
    key: Option<String>,
    /// The live text now on screen, so the same text is not parsed again.
    #[rust]
    shown_preview: Option<String>,
    /// Newer live text kept back while the reader selects in the shown text.
    #[rust]
    held_preview: Option<String>,
}
/// The live text to put on screen, or `None` to leave the panel as it is.
///
/// The answer grows while the Agent writes, and replacing the text drops a
/// selection. While the reader selects in a running answer the text holds
/// still; the latest text shows once the selection is released, the run ends,
/// or the panel turns to another run.
fn preview_to_show<'a>(
    shown: Option<&str>,
    latest: &'a str,
    running: bool,
    same_run: bool,
    selecting: bool,
) -> Option<&'a str> {
    if running && same_run && selecting && shown.is_some() {
        return None;
    }
    (shown != Some(latest)).then_some(latest)
}
/// An Agent writes Markdown. Show the live text formatted like the finished
/// reply, through the same parser and sanitizer; text that is not Markdown
/// stays plain.
fn show_preview(cx: &mut Cx, widget: &crate::shared::html_or_plaintext::HtmlOrPlaintextRef, text: &str) {
    #[cfg(feature = "agent_chat")]
    let rich = crate::agent_chat::presentation::markdown_body(text);
    #[cfg(not(feature = "agent_chat"))]
    let rich: Option<matrix_sdk::ruma::events::room::message::FormattedBody> = None;
    crate::home::room_screen::populate_text_message_content(
        cx, widget, text, rich.as_ref(), None, None, None, None,
    );
}
impl Widget for AgentRunCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // The selection ends with a click, a release or a move of focus. Show
        // the text kept back then, not at the Agent's next update.
        if self.held_preview.is_some()
            && matches!(event, Event::MouseDown(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::KeyFocusLost(_))
        {
            let preview = self.view.html_or_plaintext(cx, ids!(preview));
            if !preview.selection_active(cx)
                && let Some(text) = self.held_preview.take()
            {
                show_preview(cx, &preview, &text);
                self.shown_preview = Some(text);
                self.view.redraw(cx);
            }
        }
        let Event::Actions(actions) = event else {
            return;
        };
        let Some(key) = self.key.as_deref() else {
            return;
        };
        if let Some(text) = self.view.text_input(cx, ids!(input)).changed(actions) {
            run_controls::draft(key, text);
        }
        let stop = self.view.button(cx, ids!(buttons.stop)).clicked(actions);
        let append = self.view.button(cx, ids!(buttons.append)).clicked(actions);
        if self.view.button(cx, ids!(buttons.restore)).clicked(actions)
            && run_controls::restore_draft(key)
        {
            cx.action(PendingQuestionsChanged);
        }
        if stop || append {
            if let Some(request) = run_controls::request(key, stop) {
                cx.action(request);
            }
            cx.action(PendingQuestionsChanged);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
impl AgentRunCardRef {
    pub fn set_entry(&self, cx: &mut Cx, entry: Option<&Entry>) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        let Some(entry) = entry else {
            inner.key = None;
            inner.shown_preview = None;
            inner.held_preview = None;
            inner.view.set_visible(cx, false);
            return;
        };
        let changed = inner.key.as_deref() != Some(entry.key.as_str());
        inner.key = Some(entry.key.clone());
        inner.view.set_visible(cx, true);
        inner
            .view
            .label(cx, ids!(title))
            .set_text(cx, &format!("{} · {}", entry.name, entry.card.runtime));
        use ::hagency::agent_run_controls::State;
        let status = match entry.card.state {
            State::Running => "运行中。追加指令会加入当前运行。",
            State::Stopping => "正在停止，等待运行时结束并结算。",
            State::Completed => "已完成",
            State::Interrupted => "已停止",
            State::Failed => "执行失败",
            State::Unknown => "运行已离线，结果需要核对。",
        };
        inner.view.label(cx, ids!(status)).set_text(cx, status);
        let preview = inner.view.html_or_plaintext(cx, ids!(preview));
        preview.set_visible(cx, !entry.card.preview.is_empty());
        let open = entry.card.state == State::Running;
        let selecting = preview.selection_active(cx);
        if let Some(text) = preview_to_show(
            inner.shown_preview.as_deref(), &entry.card.preview, open, !changed, selecting,
        ) {
            show_preview(cx, &preview, text);
            inner.shown_preview = Some(text.to_owned());
            inner.held_preview = None;
        } else {
            // Kept back for the selection; `handle_event` shows it on release.
            inner.held_preview = (inner.shown_preview.as_deref() != Some(entry.card.preview.as_str()))
                .then(|| entry.card.preview.clone());
        }
        let input = inner.view.text_input(cx, ids!(input));
        input.set_visible(cx, entry.may_append && (open || !entry.draft.is_empty() || !entry.recovered_drafts.is_empty()));
        if changed || input.text() != entry.draft {
            input.set_text(cx, &entry.draft);
        }
        let append = inner.view.button(cx, ids!(buttons.append));
        append.set_visible(cx, open && entry.may_append);
        append.set_text(cx, "追加到当前运行");
        append.set_enabled(cx, !entry.sending);
        let stop = inner.view.button(cx, ids!(buttons.stop));
        stop.set_visible(cx, open && entry.may_stop);
        stop.set_enabled(cx, !entry.sending);
        let restore = inner.view.button(cx, ids!(buttons.restore));
        restore.set_visible(cx, !entry.recovered_drafts.is_empty());
        restore.set_enabled(cx, entry.draft.is_empty());
        inner
            .view
            .view(cx, ids!(buttons))
            .set_visible(cx, (open && (entry.may_append || entry.may_stop)) || !entry.recovered_drafts.is_empty());
        let hint = inner.view.label(cx, ids!(hint));
        hint.set_visible(cx, !entry.hint.is_empty());
        hint.set_text(cx, &entry.hint);
        let unconfirmed = entry.unconfirmed_text();
        inner.view.label(cx, ids!(unconfirmed_notice)).set_visible(cx, unconfirmed.is_some());
        inner.view.view(cx, ids!(unconfirmed)).set_visible(cx, unconfirmed.is_some());
        inner.view.html_or_plaintext(cx, ids!(unconfirmed.text)).show_plaintext(cx, unconfirmed.as_deref().unwrap_or_default());
        inner.view.redraw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_text_holds_still_while_the_reader_selects_in_a_running_answer() {
        // First text, and every later one, shows when nothing is selected.
        assert_eq!(preview_to_show(None, "a", true, true, false), Some("a"));
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, false), Some("ab"));
        // The same text is not parsed again.
        assert_eq!(preview_to_show(Some("ab"), "ab", true, true, false), None);
        // A selection in the running answer keeps what is on screen.
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, true), None);
        // It shows again once released, when the run ends, or for another run.
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, false), Some("ab"));
        assert_eq!(preview_to_show(Some("a"), "ab", false, true, true), Some("ab"));
        assert_eq!(preview_to_show(Some("a"), "other", true, false, true), Some("other"));
        // Nothing on screen yet is never held back.
        assert_eq!(preview_to_show(None, "a", true, true, true), Some("a"));
    }

    #[cfg(feature = "agent_chat")]
    #[test]
    fn live_text_is_formatted_like_the_finished_reply_and_plain_text_stays_plain() {
        use crate::shared::html_or_plaintext::HtmlOrPlaintextWidgetRefExt;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let card = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.AgentRunCard {}});
            let widget = WidgetRef::script_from_value(vm, value);
            widget.register_as_ui_root(vm);
            widget
        });
        let preview = card.html_or_plaintext(&mut cx, ids!(preview));
        show_preview(&mut cx, &preview, "---\n\n## 一句话总结\n\n**11 月 11 日不是 A 股\"大涨\"，而是逼空。** 判断关键。\n\n> 数据来源：新浪财经");
        assert!(card.view(&mut cx, ids!(preview.html_view)).visible());
        assert!(!card.view(&mut cx, ids!(preview.plaintext_view)).visible());
        let html = card.widget(&mut cx, ids!(preview.html_view.html)).text();
        for tag in ["<hr", "<h2>", "<strong>", "<blockquote>"] {
            assert!(html.contains(tag), "{tag} missing in {html}");
        }
        assert!(!html.contains("**") && !html.contains("##"));
        show_preview(&mut cx, &preview, "正在读取文件");
        assert!(card.view(&mut cx, ids!(preview.plaintext_view)).visible());
        assert!(!card.view(&mut cx, ids!(preview.html_view)).visible());
    }
}
