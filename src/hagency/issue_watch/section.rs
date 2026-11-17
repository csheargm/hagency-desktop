//! The "Issue watching" settings block of one Agent: mode, repository and
//! Room, saved through the owner API (`{issueWatch}`) on the Agent's
//! execution device. The poller (`super`) does the watching; its last result
//! shows as `watch_status`, and a finished save says "Saved".
use super::section_state::{State, TOKENS_URL, needs_read, saved_or_off, token_known};
use super::{choices, live};
use crate::hagency::backend::Service;
use makepad_widgets::*;
use serde_json::Value;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Muted = Text {draw_text +: {color: mod.widgets.RINX_MUTED}}
    let Select = RobrixSettingsDropDown {width: Fill height: 40 margin: 0 popup_menu_position: PopupMenuPosition.BelowInput
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
    mod.widgets.IssueWatchSection = #(IssueWatchSection::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 8
        Text {text: #(crate::i18n::tr("Issue watching")) i18n_text: "Issue watching" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
        Muted {text: #(crate::i18n::tr("New GitHub issues are posted to the chosen Room as a request to this Agent. Its normal approvals apply unless you allow pushes below; merges follow the repository's rules.")) i18n_text: "New GitHub issues are posted to the chosen Room as a request to this Agent. Its normal approvals apply unless you allow pushes below; merges follow the repository's rules."}
        watch_mode := Select {labels: ["Off"]}
        watch_hint := Muted {}
        Text {text: #(crate::i18n::tr("Repository")) i18n_text: "Repository"}
        watch_repo := RobrixTextInput {width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8} empty_text: "owner/name"}
        Text {text: #(crate::i18n::tr("Room")) i18n_text: "Room"}
        watch_room := Select {labels: ["-"]}
        watch_push := Select {labels: ["-"]}
        Muted {text: #(crate::i18n::tr("With this on, the Agent fetches, branches, edits, commits, pushes branches and opens pull requests in its own clone of this repository without asking. Build, test and install commands, other websites and anything outside that clone still ask, and merges follow the merge rule.")) i18n_text: "With this on, the Agent fetches, branches, edits, commits, pushes branches and opens pull requests in its own clone of this repository without asking. Build, test and install commands, other websites and anything outside that clone still ask, and merges follow the merge rule."}
        save_watch := RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0} text: #(crate::i18n::tr("Save issue watching")) i18n_text: "Save issue watching"}
        watch_note := Text {}
        watch_status := Muted {}
        open_tokens := RobrixIconButton {visible: false height: 32 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} text: #(crate::i18n::tr("Open GitHub tokens page")) i18n_text: "Open GitHub tokens page"}
    }
}

/// One owner-API answer for the section; stale ones are dropped.
#[derive(Clone, Debug)]
struct Reply {
    request: u64,
    agent_id: String,
    saving: bool,
    result: Result<Value, String>,
    /// For a read: whether the Agent or the environment has a GitHub token.
    token: Option<bool>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct IssueWatchSection {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    agent_id: String,
    /// Whether this device runs the Agent, so its ledger holds the watch.
    #[rust]
    editable: bool,
    /// Whether the Agent panel has handed over this Agent's status yet.
    #[rust]
    known: bool,
    #[rust]
    token: Option<bool>,
    #[rust]
    rooms: Vec<(String, String)>,
    /// The saved `issueWatch`, `Null` until read.
    #[rust]
    watch: Value,
    #[rust]
    request: u64,
    #[rust]
    pending: bool,
    #[rust]
    note: String,
}

/// The last answer, for a section that did not receive the action.
static LATEST: std::sync::Mutex<Option<Reply>> = std::sync::Mutex::new(None);
/// Wakes the UI so `draw_walk` can apply `LATEST`.
#[derive(Debug)]
struct IssueWatchReplyReady;

/// How long a read or save may take before the section says so and enables the controls.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl Widget for IssueWatchSection {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(reply) = action.downcast_ref::<Reply>() {
                self.apply_reply(cx, reply);
            }
            if action.downcast_ref::<super::IssueWatchChecked>().is_some() {
                self.render(cx);
            }
        }
        if self.view.drop_down(cx, ids!(watch_mode)).selected(actions).is_some() {
            self.note.clear();
            self.render(cx);
        }
        if self.view.button(cx, ids!(open_tokens)).clicked(actions) {
            crate::utils::open_url(TOKENS_URL);
        }
        if self.view.button(cx, ids!(save_watch)).clicked(actions) {
            self.save(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let latest = LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(reply) = latest.filter(|r| self.pending && r.request == self.request && r.agent_id == self.agent_id) {
            self.apply_reply(cx, &reply);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl IssueWatchSection {
    /// Shows `agent_id`'s watch; `editable` is `None` until the Agent's status
    /// is loaded. Reads the watch once per Agent, so typing is never overwritten.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        self.rooms = rooms;
        if self.agent_id != agent_id {
            self.agent_id = agent_id.to_owned();
            self.watch = Value::Null;
            self.note.clear();
            self.pending = false;
            self.token = None;
        }
        self.known = editable.is_some();
        self.editable = editable.unwrap_or(false);
        // Read once per Agent while it runs here. A device check that settles late (or
        // flips while the panel loads) no longer discards a pending read and starts over,
        // which left every control greyed out.
        if needs_read(self.editable, &self.agent_id, &self.watch, self.pending) {
            self.send(None);
        }
        self.render(cx);
    }
    fn send(&mut self, change: Option<Value>) {
        let service = match Service::for_current() {
            Ok(service) => service,
            Err(error) => {
                self.note = crate::i18n::format("Could not read issue watching: {error}", &[("error", error)]);
                return;
            }
        };
        self.request += 1;
        self.pending = true;
        let (request, agent_id, saving) = (self.request, self.agent_id.clone(), change.is_some());
        crate::sliding_sync::spawn_async_task(async move {
            let result = match tokio::time::timeout(READ_TIMEOUT, live::saved(&service, &agent_id, change)).await {
                Ok(result) => result,
                Err(_) => Err("no answer from this device's Agent host within 10 s".to_owned()),
            };
            // The token itself stays in the poller; only whether one exists is kept.
            // Bounded (the OS store may block or prompt); "unknown" shows no hint.
            let token = if saving { None } else { token_known(&agent_id).await };
            if let Err(error) = &result {
                makepad_widgets::log!("WARN issue_watch_request_failed saving={saving}: {error}");
            }
            let reply = Reply { request, agent_id, saving, result, token };
            // Also kept here: the section picks it up on its next draw even if the
            // action is not routed to it (it lives inside a nested settings page).
            LATEST.lock().unwrap_or_else(|p| p.into_inner()).replace(reply.clone());
            Cx::post_action(reply);
            Cx::post_action(IssueWatchReplyReady);
        });
    }
    fn apply_reply(&mut self, cx: &mut Cx, reply: &Reply) {
        if reply.request != self.request || reply.agent_id != self.agent_id {
            return;
        }
        self.pending = false;
        self.token = reply.token.or(self.token);
        match &reply.result {
            Ok(value) => {
                self.watch = saved_or_off(value);
                self.fill(cx);
                self.note = if reply.saving { crate::i18n::tr("Saved").to_owned() } else { String::new() };
            }
            Err(error) => {
                let template = if reply.saving { "Could not save issue watching: {error}" } else { "Could not read issue watching: {error}" };
                self.note = crate::i18n::format(template, &[("error", error.clone())]);
                // A failed read still lets the owner choose and save: the controls start at Off.
                if !reply.saving && !self.watch.is_object() {
                    self.watch = serde_json::json!({"mode": "off"});
                    self.fill(cx);
                }
            }
        }
        self.render(cx);
    }
    /// Puts the saved watch into the controls.
    fn fill(&mut self, cx: &mut Cx) {
        let index = choices::index(self.watch["mode"].as_str().unwrap_or(""));
        self.view.drop_down(cx, ids!(watch_mode)).set_selected_item(cx, index);
        self.view.text_input(cx, ids!(watch_repo)).set_text(cx, self.watch["repo"].as_str().unwrap_or(""));
        let saved_room = self.watch["roomId"].as_str().unwrap_or("");
        let room = self.rooms.iter().position(|(id, _)| id == saved_room).unwrap_or(0);
        self.view.drop_down(cx, ids!(watch_room)).set_selected_item(cx, room);
        self.view.drop_down(cx, ids!(watch_push)).set_selected_item(cx, usize::from(self.watch["pushTrusted"] == true));
    }
    fn save(&mut self, cx: &mut Cx) {
        let index = self.view.drop_down(cx, ids!(watch_mode)).selected_item();
        let repo = self.view.text_input(cx, ids!(watch_repo)).text();
        let room = self.rooms.get(self.view.drop_down(cx, ids!(watch_room)).selected_item()).map(|(id, _)| id.as_str());
        match choices::change(index, &repo, room, self.view.drop_down(cx, ids!(watch_push)).selected_item()) {
            Ok(change) => {
                self.note = crate::i18n::tr("Saving…").to_owned();
                self.send(Some(change));
            }
            Err(reason) => self.note = crate::i18n::tr(reason).to_owned(),
        }
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let labels = choices::MODE_LABELS.iter().map(|l| crate::i18n::tr(l).to_owned()).collect();
        self.view.drop_down(cx, ids!(watch_mode)).set_labels(cx, labels);
        let index = self.view.drop_down(cx, ids!(watch_mode)).selected_item();
        self.view.label(cx, ids!(watch_hint)).set_text(cx, crate::i18n::tr(choices::hint(index)));
        self.view.drop_down(cx, ids!(watch_push)).set_labels(cx, choices::PUSH_LABELS.iter().map(|l| crate::i18n::tr(l).to_owned()).collect());
        let names = if self.rooms.is_empty() {
            vec![crate::i18n::tr("No connected Rooms yet.").to_owned()]
        } else {
            self.rooms.iter().map(|(_, name)| name.clone()).collect()
        };
        let room = self.view.drop_down(cx, ids!(watch_room)).selected_item().min(names.len() - 1);
        self.view.drop_down(cx, ids!(watch_room)).set_labels(cx, names);
        self.view.drop_down(cx, ids!(watch_room)).set_selected_item(cx, room);
        let state = self.state();
        let usable = state.usable();
        for id in [ids!(watch_mode), ids!(watch_repo), ids!(watch_push)] {
            self.view.widget(cx, id).set_disabled(cx, !usable);
        }
        self.view.widget(cx, ids!(watch_room)).set_disabled(cx, !usable || self.rooms.is_empty());
        self.view.button(cx, ids!(save_watch)).set_enabled(cx, usable);
        self.view.label(cx, ids!(watch_note)).set_text(cx, &self.note);
        self.view.label(cx, ids!(watch_status)).set_text(cx, &self.status_line(&state));
        self.view.button(cx, ids!(open_tokens)).set_visible(cx, state.token == Some(false));
        self.view.redraw(cx);
    }
    fn state(&self) -> State {
        State {
            known: self.known,
            editable: self.editable,
            read: self.watch.is_object(),
            pending: self.pending,
            no_rooms: self.rooms.is_empty(),
            token: self.token,
        }
    }
    /// Why anything is disabled or missing, then the poller's last result.
    fn status_line(&self, state: &State) -> String {
        let mut lines: Vec<String> = state.reasons().into_iter().map(|r| crate::i18n::tr(r).to_owned()).collect();
        if let Some(line) = self.watch_line() {
            lines.push(line);
        }
        lines.join("\n")
    }
    fn watch_line(&self) -> Option<String> {
        if !self.usable_watch() {
            return None;
        }
        let watching = self.watch["mode"].as_str().is_some_and(|m| m != "off");
        match super::status_line(&self.agent_id) {
            Some(line) if watching => Some(line),
            _ if watching => Some(crate::i18n::tr("Waiting for the first check (every minute).").to_owned()),
            _ => None,
        }
    }
    fn usable_watch(&self) -> bool {
        self.known && self.editable && self.watch.is_object()
    }
}

impl IssueWatchSectionRef {
    /// See [`IssueWatchSection::set_context`].
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_context(cx, agent_id, editable, rooms);
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    /// Feeds one answer as if the owner API had sent it for the current request.
    pub fn answer(section: &mut IssueWatchSection, cx: &mut Cx, saving: bool, result: Result<Value, String>) {
        let reply = Reply { request: section.request, agent_id: section.agent_id.clone(), saving, result, token: None };
        section.apply_reply(cx, &reply);
    }
    pub fn stale_answer(section: &mut IssueWatchSection, cx: &mut Cx, result: Result<Value, String>) {
        let reply = Reply { request: section.request + 1, agent_id: section.agent_id.clone(), saving: true, result, token: None };
        section.apply_reply(cx, &reply);
    }
}
