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
        preview := HtmlOrPlaintext { width: Fill height: Fit selectable: true }
        // A finished answer, with the same actions as an Agent's reply message.
        answer := mod.widgets.AgentReply {width: Fill}
        // Only working, waiting-for-approval and failed runs have a status line.
        footer := View {
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            status := Label {
                width: Fit height: Fit
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
            }
            open_approval := Button {
                visible: false
                text: #(crate::i18n::tr("Open approval")) i18n_text: "Open approval"
            }
        }
        links := View {
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            details := ButtonFlat {
                visible: false
                text: #(crate::i18n::tr("What I did")) i18n_text: "What I did"
            }
            updates := ButtonFlat {visible: false}
        }
        what_i_did := Label {
            visible: false width: Fill height: Fit
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        // "N updates" opened in place (agent_run_updates.rs).
        updates_list := Label {
            visible: false width: Fill height: Fit flow: Flow.Right{wrap: true}
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
        }
        // More runs than slots: the last card pages through them.
        pager := View {
            visible: false width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            previous := Button {text: "‹"}
            page := Label {width: Fit height: Fit draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}}
            next := Button {text: "›"}
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
    /// The viewer's own request no card answers yet (shown while no run is).
    #[rust]
    pending: Option<super::agent_run_requester::PendingRequest>,
    /// Ticks once a second while the shown run is running.
    #[rust]
    clock: Timer,
    /// Whether the reader opened the run's "What I did".
    #[rust]
    details_open: bool,
    /// The runs this card pages through (the last slot's overflow).
    #[rust]
    pages: Vec<Entry>,
    /// The run the reader paged to, kept while it is still listed.
    #[rust]
    page_key: Option<String>,
    /// The progress thread's reply count, for "N updates".
    #[rust]
    updates_count: usize,
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
        for (path, forward) in [(ids!(pager.previous), false), (ids!(pager.next), true)] {
            if self.view.button(cx, path).clicked(actions) {
                self.turn_page(cx, forward);
                return;
            }
        }
        #[cfg(feature = "agent_chat")]
        if let Some(action) = {
            use crate::agent_chat::reply::AgentReplyWidgetExt;
            self.view.agent_reply(cx, ids!(answer)).clicked_action(cx, actions)
        } && let Some(entry) = self.page_key.as_deref().and_then(|k| self.pages.iter().find(|e| e.key == k))
        {
            super::agent_run_answer::act(cx, action, entry);
        }
        let ours = |c: &super::agent_run_updates::RunUpdatesChanged| self.run.as_ref().is_some_and(|(card, _)| c.room == card.room_id && c.thread_root == card.thread_root);
        if actions.iter().any(|a| a.downcast_ref().is_some_and(ours)) {
            self.show_updates(cx);
            self.view.redraw(cx);
        }
        let Some(key) = self.key.as_deref() else {
            return;
        };
        if self.view.button(cx, ids!(links.details)).clicked(actions) {
            self.details_open = !self.details_open;
            self.view.label(cx, ids!(what_i_did)).set_visible(cx, self.details_open);
            self.view.redraw(cx);
        }
        if let Some((card, _)) = self.run.as_ref() {
            // The owner's approval UI for this room, where the pending card waits.
            #[cfg(feature = "agent_chat")]
            if self.view.button(cx, ids!(footer.open_approval)).clicked(actions)
                && let Ok(project) = matrix_sdk::ruma::OwnedRoomId::try_from(card.room_id.as_str())
            {
                cx.action(crate::agent_chat::approval_inbox::ApprovalInboxAction::Open { project: Some(project) });
            }
            if self.view.button(cx, ids!(links.updates)).clicked(actions) {
                super::agent_run_updates::toggle(&card.room_id, &card.thread_root, self.updates_count);
            }
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
    // TODO(R4): with the file split, move this status line next to agent_run_footer.
    fn show_status(&mut self, cx: &mut Cx) {
        use super::agent_run_footer::{Footer, Waiting, footer, waiting};
        let shown = self.run.as_ref().map(|(card, started)| {
            let waiting = waiting(card);
            let owner = match (waiting, matrix_sdk::ruma::OwnedUserId::try_from(card.owner.as_str()), matrix_sdk::ruma::OwnedRoomId::try_from(card.room_id.as_str())) {
                (Waiting::ForOwner, Ok(owner), Ok(room)) => super::agent_run_requester::display_name(cx, &owner, &room),
                _ => String::new(),
            };
            footer(card, started.elapsed().as_secs(), waiting, &owner)
        });
        let pending = self.pending.as_ref().filter(|_| self.run.is_none());
        let shown = shown
            .or_else(|| pending.map(|p| Footer::Working(super::agent_run_requester::pending_text(&p.name, p.sent.elapsed().as_secs()))))
            .unwrap_or(Footer::Quiet);
        self.view.view(cx, ids!(footer)).set_visible(cx, shown != Footer::Quiet);
        self.view.label(cx, ids!(footer.status)).set_text(cx, shown.text());
        self.view.button(cx, ids!(footer.open_approval)).set_visible(cx, matches!(shown, Footer::Approval(_)));
        let ticking = self.run.as_ref().map_or(pending.is_some(), |(card, _)| super::agent_run_status::needs_tick(card));
        if !ticking {
            cx.stop_timer(self.clock);
            self.clock = Timer::default();
        } else if self.clock.is_empty() {
            self.clock = cx.start_timeout(CLOCK_TICK_SECS);
        }
    }
}
impl AgentRunCard {
    /// "N updates", or "Hide updates" over the list it opened in place.
    // TODO(R4): over budget; move the links row (details, updates) out with the file split.
    fn show_updates(&mut self, cx: &mut Cx) {
        let open = self.run.as_ref().and_then(|(card, _)| super::agent_run_updates::shown(&card.room_id, &card.thread_root));
        let link = self.view.button(cx, ids!(links.updates));
        let text = match &open {
            Some(_) => Some(crate::i18n::tr("Hide updates").to_owned()),
            None => super::agent_run_footer::updates_text(self.updates_count),
        };
        link.set_visible(cx, text.is_some());
        link.set_text(cx, text.as_deref().unwrap_or_default());
        let list = self.view.label(cx, ids!(updates_list));
        list.set_visible(cx, open.is_some() && self.updates_count > 0);
        list.set_text(cx, &open.as_ref().map(super::agent_run_updates::text).unwrap_or_default());
    }
}
impl AgentRunCardRef {
    /// The "N updates" link to the progress thread; hidden for none. Open
    /// updates follow the thread as it grows.
    pub fn set_updates(&self, cx: &mut Cx, count: usize) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.updates_count = count;
        if let Some((card, _)) = inner.run.as_ref() {
            super::agent_run_updates::follow(&card.room_id, &card.thread_root, count);
        }
        inner.show_updates(cx);
    }
    /// The viewer's own request no run card answers yet: its waiting line,
    /// shown while this card has no run.
    pub fn set_pending(&self, cx: &mut Cx, pending: Option<super::agent_run_requester::PendingRequest>) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.pending = pending;
        if inner.run.is_none() {
            let shown = inner.pending.is_some();
            inner.view.set_visible(cx, shown);
            inner.show_status(cx);
        }
    }
    pub fn set_entry(&self, cx: &mut Cx, entry: Option<&Entry>) {
        self.set_entries(cx, entry.map(std::slice::from_ref).unwrap_or_default());
    }
    /// Shows one of these runs, with a pager when there is more than one.
    pub fn set_entries(&self, cx: &mut Cx, entries: &[Entry]) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.pages = entries.to_vec();
        inner.turn_page_by(cx, None);
    }
}
impl AgentRunCard {
    /// Pages one run forward or back.
    fn turn_page(&mut self, cx: &mut Cx, forward: bool) {
        self.turn_page_by(cx, Some(forward));
    }
    fn turn_page_by(&mut self, cx: &mut Cx, step: Option<bool>) {
        use super::agent_run_pager as pager;
        let keys: Vec<&str> = self.pages.iter().map(|e| e.key.as_str()).collect();
        let mut page = pager::page_of(&keys, self.page_key.as_deref());
        if let Some(forward) = step { page = pager::step(page, keys.len(), forward); }
        let entry = self.pages.get(page).cloned();
        self.page_key = entry.as_ref().map(|e| e.key.clone());
        self.view.view(cx, ids!(pager)).set_visible(cx, self.pages.len() > 1);
        self.view.label(cx, ids!(pager.page)).set_text(cx, &pager::pager_text(page, self.pages.len()));
        self.show(cx, entry.as_ref());
    }
    // TODO(R4): split this file on the seam between the card's live run view
    // and its answer/pager presentation.
    fn show(&mut self, cx: &mut Cx, entry: Option<&Entry>) {
        let inner = self;
        let Some(entry) = entry else {
            inner.key = None;
            inner.shown_preview = None;
            inner.held_preview = None;
            inner.run = None;
            inner.show_status(cx);
            show_answer(cx, &inner.view, None);
            // Only the waiting line of a pending request, if any.
            for part in [ids!(preview), ids!(links), ids!(what_i_did), ids!(input), ids!(buttons), ids!(hint), ids!(unconfirmed_notice), ids!(unconfirmed)] {
                inner.view.widget(cx, part).set_visible(cx, false);
            }
            inner.view.set_visible(cx, inner.pending.is_some());
            return;
        };
        let changed = inner.key.as_deref() != Some(entry.key.as_str());
        if changed {
            inner.details_open = false;
        }
        inner.key = Some(entry.key.clone());
        inner.view.set_visible(cx, true);
        inner.view.view(cx, ids!(links)).set_visible(cx, true);
        // The row above already names the Agent; the runtime sits in "What I did".
        let did = super::agent_run_footer::what_i_did(&entry.card);
        inner.view.button(cx, ids!(links.details)).set_visible(cx, did.is_some());
        inner.view.label(cx, ids!(what_i_did)).set_text(cx, did.as_deref().unwrap_or_default());
        inner.view.label(cx, ids!(what_i_did)).set_visible(cx, inner.details_open && did.is_some());
        inner.run = Some((entry.card.clone(), entry.started));
        inner.show_status(cx);
        let preview = inner.view.html_or_plaintext(cx, ids!(preview));
        let body = shown_text(&entry.card);
        let in_reply = show_answer(cx, &inner.view, Some(entry));
        preview.set_visible(cx, !body.is_empty() && !in_reply);
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

/// Shows an answered run's answer in the reply component (true), else hides it.
fn show_answer(cx: &mut Cx, view: &View, entry: Option<&Entry>) -> bool {
    #[cfg(feature = "agent_chat")]
    {
        use crate::agent_chat::reply::AgentReplyWidgetExt;
        let reply = view.agent_reply(cx, ids!(answer));
        match entry.filter(|e| !e.card.answer.is_empty()) {
            Some(entry) => {
                let me = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
                let rich = crate::agent_chat::presentation::markdown_body(&entry.card.answer);
                reply.populate(cx, format!("{me}|{}|{}", entry.card.room_id, entry.key), &entry.card.answer, rich.as_ref(), false, false);
                reply.set_meta(cx, &super::agent_run_footer::duration(&entry.card).unwrap_or_default());
                true
            }
            None => { reply.hide(cx); false }
        }
    }
    #[cfg(not(feature = "agent_chat"))]
    { let _ = (cx, view, entry); false }
}

/// The card's main text: the answer once the run carries it, else the live text.
fn shown_text(card: &::hagency::agent_run_controls::Card) -> &str {
    if card.answer.is_empty() { &card.preview } else { &card.answer }
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
    fn an_answered_run_shows_the_answer_and_a_quiet_footer() {
        let mut card: ::hagency::agent_run_controls::Card = serde_json::from_value(serde_json::json!({"version":1,
            "id":"c","dispatchId":"d","executionId":"e","roomId":"!r","eventId":"$e","threadRoot":"!r",
            "requester":"@a","owner":"@b","runtime":"codex","state":"running","revision":0,"preview":"live"})).unwrap();
        assert_eq!(shown_text(&card), "live");
        assert!(matches!(super::super::agent_run_footer::footer(&card, 2, super::super::agent_run_footer::Waiting::No, ""), super::super::agent_run_footer::Footer::Working(_)));
        card.state = ::hagency::agent_run_controls::State::Completed;
        card.preview.clear();
        card.answer = "42".into();
        assert_eq!(shown_text(&card), "42");
        assert_eq!(super::super::agent_run_footer::footer(&card, 2, super::super::agent_run_footer::Waiting::No, ""), super::super::agent_run_footer::Footer::Quiet);
    }
    /// Review fix: with more runs than slots, the last card pages through
    /// them, and an answer shows in the reply component with its actions.
    #[cfg(feature = "agent_chat")]
    #[test]
    fn the_last_card_pages_through_overflow_runs_and_answers_keep_their_actions() {
        use crate::hagency::run_controls::fixture;
        use ::hagency::agent_run_controls::State;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.AgentRunCard {}});
            let widget = WidgetRef::script_from_value(vm, value);
            widget.register_as_ui_root(vm);
            widget
        });
        let card = widget.as_agent_run_card();
        let runs = [fixture("$a", State::Completed, "first answer"), fixture("$b", State::Running, ""), fixture("$c", State::Completed, "third")];
        let all: Vec<_> = (0..5).map(|i| fixture(&format!("$r{i}"), State::Completed, "x")).collect();
        let slots = super::super::agent_run_pager::slot_entries(&all);
        assert_eq!(slots.iter().map(|s| s.len()).sum::<usize>(), 5, "every run has a slot");
        card.set_entries(&mut cx, &runs);
        assert!(widget.view(&mut cx, ids!(pager)).visible());
        assert_eq!(widget.label(&mut cx, ids!(pager.page)).text(), "1 / 3");
        assert!(widget.widget(&mut cx, ids!(answer)).visible(), "the answer shows in the reply component");
        assert!(!widget.widget(&mut cx, ids!(preview)).visible());
        assert!(widget.view(&mut cx, ids!(answer.answer_actions)).visible(), "Copy/Forward/Markdown/PDF are offered");
        card.borrow_mut().unwrap().turn_page(&mut cx, true);
        assert_eq!(card.borrow().unwrap().key.as_deref(), Some("$b"));
        assert!(widget.button(&mut cx, ids!(buttons.stop)).visible(), "an overflow active run keeps its controls");
        assert!(!widget.widget(&mut cx, ids!(answer)).visible());
        card.set_entries(&mut cx, &runs);
        assert_eq!(card.borrow().unwrap().key.as_deref(), Some("$b"), "the page holds across updates");
        card.set_entries(&mut cx, &runs[..1]);
        assert!(!widget.view(&mut cx, ids!(pager)).visible());
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
