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
}
impl Widget for AgentRunCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
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
        preview.show_plaintext(cx, &entry.card.preview);
        let open = entry.card.state == State::Running;
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
