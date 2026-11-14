//! The "Issue watching" settings block of one Agent: mode, repository and
//! Room, saved through the owner API (`{issueWatch}`) on the Agent's
//! execution device. The poller (`super`) does the watching; its last result
//! shows as `watch_status`, and a finished save says "Saved".
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
        Muted {text: #(crate::i18n::tr("New GitHub issues are posted to the chosen Room as a request to this Agent. Its normal approvals apply; nothing is merged or written to GitHub.")) i18n_text: "New GitHub issues are posted to the chosen Room as a request to this Agent. Its normal approvals apply; nothing is merged or written to GitHub."}
        watch_mode := Select {labels: ["Off"]}
        watch_hint := Muted {}
        Text {text: #(crate::i18n::tr("Repository")) i18n_text: "Repository"}
        watch_repo := RobrixTextInput {width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8} empty_text: "owner/name"}
        Text {text: #(crate::i18n::tr("Room")) i18n_text: "Room"}
        watch_room := Select {labels: ["-"]}
        save_watch := RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0} text: #(crate::i18n::tr("Save issue watching")) i18n_text: "Save issue watching"}
        watch_note := Text {}
        watch_status := Muted {}
    }
}

/// One owner-API answer for the section; stale ones are dropped.
#[derive(Debug)]
struct Reply {
    request: u64,
    agent_id: String,
    saving: bool,
    result: Result<Value, String>,
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
        if self.view.button(cx, ids!(save_watch)).clicked(actions) {
            self.save(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl IssueWatchSection {
    /// Shows `agent_id`'s watch; reads it again only when the Agent or
    /// whether this device runs it changed, so typing is never overwritten.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: bool, rooms: Vec<(String, String)>) {
        let changed = self.agent_id != agent_id || self.editable != editable;
        self.rooms = rooms;
        if changed {
            self.agent_id = agent_id.to_owned();
            self.editable = editable;
            self.watch = Value::Null;
            self.note.clear();
            if editable && !agent_id.is_empty() {
                self.send(None);
            }
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
            let result = live::saved(&service, &agent_id, change).await;
            Cx::post_action(Reply { request, agent_id, saving, result });
        });
    }
    fn apply_reply(&mut self, cx: &mut Cx, reply: &Reply) {
        if reply.request != self.request || reply.agent_id != self.agent_id {
            return;
        }
        self.pending = false;
        match &reply.result {
            Ok(value) => {
                self.watch = value["issueWatch"].clone();
                self.fill(cx);
                self.note = if reply.saving { crate::i18n::tr("Saved").to_owned() } else { String::new() };
            }
            Err(error) => {
                let template = if reply.saving { "Could not save issue watching: {error}" } else { "Could not read issue watching: {error}" };
                self.note = crate::i18n::format(template, &[("error", error.clone())]);
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
    }
    fn save(&mut self, cx: &mut Cx) {
        let index = self.view.drop_down(cx, ids!(watch_mode)).selected_item();
        let repo = self.view.text_input(cx, ids!(watch_repo)).text();
        let room = self.rooms.get(self.view.drop_down(cx, ids!(watch_room)).selected_item()).map(|(id, _)| id.as_str());
        match choices::change(index, &repo, room) {
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
        let names = if self.rooms.is_empty() {
            vec![crate::i18n::tr("No connected Rooms yet.").to_owned()]
        } else {
            self.rooms.iter().map(|(_, name)| name.clone()).collect()
        };
        let room = self.view.drop_down(cx, ids!(watch_room)).selected_item().min(names.len() - 1);
        self.view.drop_down(cx, ids!(watch_room)).set_labels(cx, names);
        self.view.drop_down(cx, ids!(watch_room)).set_selected_item(cx, room);
        let usable = self.editable && self.watch.is_object() && !self.pending;
        for id in [ids!(watch_mode), ids!(watch_repo)] {
            self.view.widget(cx, id).set_disabled(cx, !usable);
        }
        self.view.widget(cx, ids!(watch_room)).set_disabled(cx, !usable || self.rooms.is_empty());
        self.view.button(cx, ids!(save_watch)).set_enabled(cx, usable);
        self.view.label(cx, ids!(watch_note)).set_text(cx, &self.note);
        self.view.label(cx, ids!(watch_status)).set_text(cx, &self.status_line());
    }
    fn status_line(&self) -> String {
        if !self.editable {
            return crate::i18n::tr("Issue watching runs on the device that runs this Agent.").to_owned();
        }
        let watching = self.watch["mode"].as_str().is_some_and(|m| m != "off");
        match super::status_line(&self.agent_id) {
            Some(line) if watching => line,
            _ if watching => match super::github_token() {
                Ok(_) => crate::i18n::tr("Waiting for the first check (every minute).").to_owned(),
                Err(error) => error,
            },
            _ => String::new(),
        }
    }
}

impl IssueWatchSectionRef {
    /// See [`IssueWatchSection::set_context`].
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str, editable: bool, rooms: Vec<(String, String)>) {
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
        let reply = Reply { request: section.request, agent_id: section.agent_id.clone(), saving, result };
        section.apply_reply(cx, &reply);
    }
    pub fn stale_answer(section: &mut IssueWatchSection, cx: &mut Cx, result: Result<Value, String>) {
        let reply = Reply { request: section.request + 1, agent_id: section.agent_id.clone(), saving: true, result };
        section.apply_reply(cx, &reply);
    }
}
