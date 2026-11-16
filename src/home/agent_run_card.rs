use makepad_widgets::*;
use crate::shared::html_or_plaintext::HtmlOrPlaintextWidgetExt;
use super::agent_run_preview::preview_to_show;
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
        preview := HtmlOrPlaintext { width: Fill height: Fit selectable: true }
        footer := View {
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            status := Label {
                width: Fit height: Fit
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
            }
            details := Button {
                visible: false
                text: #(crate::i18n::tr("Details")) i18n_text: "Details"
            }
        }
        input := TextInput {
            width: Fill height: Fit
            empty_text: #(crate::i18n::tr("Add instructions…")) i18n_empty_text: "Add instructions…"
        }
        buttons := View {
            width: Fill height: Fit flow: Right spacing: 8
            append := Button {text: #(crate::i18n::tr("Add to the current run")) i18n_text: "Add to the current run"}
            stop := Button {text: #(crate::i18n::tr("Stop this run")) i18n_text: "Stop this run"}
            restore := Button {text: #(crate::i18n::tr("Restore unsent instructions")) i18n_text: "Restore unsent instructions"}
        }
        hint := Label {
            width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        unconfirmed_notice := Label {
            visible: false width: Fill height: Fit
            text: #(crate::i18n::tr("The result of the instructions below is unconfirmed; the text can be copied. Check first to avoid running them twice.")) i18n_text: "The result of the instructions below is unconfirmed; the text can be copied. Check first to avoid running them twice."
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
    /// The shown run and when it started, for its ticking elapsed time.
    #[rust]
    run: Option<(::hagency::agent_run_controls::Card, std::time::Instant)>,
    /// Ticks once a second while the shown run is running.
    #[rust]
    clock: Timer,
    /// Whether the reader opened an answered run's details (Agent, runtime).
    #[rust]
    details_open: bool,
}
/// How often a running card's elapsed time ticks.
const CLOCK_TICK_SECS: f64 = 1.0;
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
        if self.clock.is_event(event).is_some() {
            self.clock = Timer::default();
            self.show_status(cx);
            self.view.redraw(cx);
            cx.action(super::agent_run_status::RunClockTick);
        }
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
        if self.view.button(cx, ids!(footer.details)).clicked(actions) {
            self.details_open = !self.details_open;
            self.view.label(cx, ids!(title)).set_visible(cx, self.details_open);
            self.view.redraw(cx);
        }
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
impl AgentRunCard {
    /// Shows the run's live status; keeps the clock ticking only while it runs.
    fn show_status(&mut self, cx: &mut Cx) {
        let ticking = self.run.as_ref().is_some_and(|(card, started)| {
            let status = super::agent_run_status::live_status(card, started.elapsed().as_secs());
            self.view.label(cx, ids!(footer.status)).set_text(cx, &footer_text(card, &status));
            super::agent_run_status::needs_tick(card)
        });
        if !ticking {
            cx.stop_timer(self.clock);
            self.clock = Timer::default();
        } else if self.clock.is_empty() {
            self.clock = cx.start_timeout(CLOCK_TICK_SECS);
        }
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
            inner.run = None;
            inner.show_status(cx);
            inner.view.set_visible(cx, false);
            return;
        };
        let changed = inner.key.as_deref() != Some(entry.key.as_str());
        if changed {
            inner.details_open = false;
        }
        inner.key = Some(entry.key.clone());
        inner.view.set_visible(cx, true);
        inner
            .view
            .label(cx, ids!(title))
            .set_text(cx, &format!("{} · {}", entry.name, entry.card.runtime));
        // An answered run is the answer itself: its Agent and runtime move
        // behind "Details", and the status line becomes a short footer.
        let answered = !entry.card.answer.is_empty();
        let show_title = !answered || inner.details_open;
        inner.view.label(cx, ids!(title)).set_visible(cx, show_title);
        inner.view.button(cx, ids!(footer.details)).set_visible(cx, answered);
        inner.run = Some((entry.card.clone(), entry.started));
        inner.show_status(cx);
        let preview = inner.view.html_or_plaintext(cx, ids!(preview));
        let body = shown_text(&entry.card);
        preview.set_visible(cx, !body.is_empty());
        let open = entry.card.state == ::hagency::agent_run_controls::State::Running;
        let selecting = preview.selection_active(cx);
        if let Some(text) = preview_to_show(
            inner.shown_preview.as_deref(), body, open, !changed, selecting,
        ) {
            show_preview(cx, &preview, text);
            inner.shown_preview = Some(text.to_owned());
            inner.held_preview = None;
        } else {
            // Kept back for the selection; `handle_event` shows it on release.
            inner.held_preview = (inner.shown_preview.as_deref() != Some(body))
                .then(|| body.to_owned());
        }
        let input = inner.view.text_input(cx, ids!(input));
        input.set_visible(cx, entry.may_append && (open || !entry.draft.is_empty() || !entry.recovered_drafts.is_empty()));
        if changed || input.text() != entry.draft {
            input.set_text(cx, &entry.draft);
        }
        let append = inner.view.button(cx, ids!(buttons.append));
        append.set_visible(cx, open && entry.may_append);
        append.set_text(cx, crate::i18n::tr("Add to the current run"));
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
        hint.set_text(cx, crate::i18n::tr(&entry.hint));
        let unconfirmed = entry.unconfirmed_text();
        inner.view.label(cx, ids!(unconfirmed_notice)).set_visible(cx, unconfirmed.is_some());
        inner.view.view(cx, ids!(unconfirmed)).set_visible(cx, unconfirmed.is_some());
        inner.view.html_or_plaintext(cx, ids!(unconfirmed.text)).show_plaintext(cx, unconfirmed.as_deref().unwrap_or_default());
        inner.view.redraw(cx);
    }
}

/// The card's main text: the answer once the run carries it, else the live text.
fn shown_text(card: &::hagency::agent_run_controls::Card) -> &str {
    if card.answer.is_empty() { &card.preview } else { &card.answer }
}
/// The status line: "✅ Done in 2 s · 0 steps" under an answer, else the live status.
fn footer_text(card: &::hagency::agent_run_controls::Card, status: &str) -> String {
    if card.answer.is_empty() { status.to_owned() } else { format!("✅ {status}") }
}

#[cfg(test)]
const SOURCE_TEXT: &str = include_str!("agent_run_card.rs");
#[cfg(test)]
mod tests {
    use super::SOURCE_TEXT;
    use super::*;

    #[cfg(feature = "agent_chat")]
    /// The run card's buttons, placeholder and notices are English keys with a
    /// Chinese translation, so an English UI no longer shows Chinese labels.
    #[test]
    fn run_card_text_is_english_with_a_chinese_translation() {
        use crate::i18n::{Language, text_for};
        for key in ["Add instructions…", "Add to the current run", "Stop this run", "Restore unsent instructions",
            "Sending…", "Sent. Waiting for the Agent to confirm.", "The Agent received this action."] {
            assert!(key.is_ascii() || key.ends_with('…'), "{key}");
            assert_eq!(text_for(Language::English, key), key);
            assert_ne!(text_for(Language::Chinese, key), key, "{key} has no Chinese text");
        }
        assert!(!SOURCE_TEXT.split("#[cfg(test)]").next().unwrap().chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "Chinese literal in the card's layout");
    }
    #[test]
    fn an_answered_run_shows_the_answer_with_a_short_footer() {
        let mut card: ::hagency::agent_run_controls::Card = serde_json::from_value(serde_json::json!({"version":1,
            "id":"c","dispatchId":"d","executionId":"e","roomId":"!r","eventId":"$e","threadRoot":"!r",
            "requester":"@a","owner":"@b","runtime":"codex","state":"running","revision":0,"preview":"live"})).unwrap();
        assert_eq!(shown_text(&card), "live");
        assert_eq!(footer_text(&card, "Thinking… 2 s"), "Thinking… 2 s");
        card.state = ::hagency::agent_run_controls::State::Completed;
        card.preview.clear();
        card.answer = "42".into();
        assert_eq!(shown_text(&card), "42");
        assert_eq!(footer_text(&card, "Done in 2 s · 0 steps"), "✅ Done in 2 s · 0 steps");
    }
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

pub(crate) use super::agent_run_status::{status_label, status_of_message};
