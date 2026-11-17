//! The "Repositories" block of one Agent (ADR 0016): the owner's
//! work list, saved through the owner API (`{work}`) on the Agent's execution
//! device and never writable by the Agent. The poller (`super::work`) acts on
//! it; its last result shows under the issue-watching block.
use super::live;
use super::work_lines;
use crate::hagency::backend::Service;
use makepad_widgets::*;
use serde_json::{Value, json};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Muted = Text {draw_text +: {color: mod.widgets.RINX_MUTED}}
    let Select = RobrixSettingsDropDown {width: Fill height: 40 margin: 0 popup_menu_position: PopupMenuPosition.BelowInput
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
    mod.widgets.WorkSection = #(WorkSection::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 8
        Text {text: #(crate::i18n::tr("Repositories")) i18n_text: "Repositories" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
        Muted {text: #(crate::i18n::tr("The agent claims new issues, opens pull requests and reviews other people's pull requests in these repositories. It merges only where your GitHub account can, under your rules below; GitHub's own permissions and branch rules still apply.")) i18n_text: "The agent claims new issues, opens pull requests and reviews other people's pull requests in these repositories. It merges only where your GitHub account can, under your rules below; GitHub's own permissions and branch rules still apply."}
        work_repos := RobrixTextInput {width: Fill height: Fit margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8} empty_text: "acme/app any-path authors=betty; acme/docs"}
        Muted {text: #(crate::i18n::tr(super::work_lines::WORK_HINT)) i18n_text: "One per line or separated by ;  owner/name, then optional: all (any issue author), any-path, max=N, authors=login,login (or *), reviewers=login, human=glob, push-freely (fetch, edit, commit, push branches and open pull requests in its clone without asking), issues=N (issues per day), prs=N (open pull requests at once)"}
        Text {text: #(crate::i18n::tr("Room for its notices")) i18n_text: "Room for its notices"}
        work_room := Select {labels: ["-"]}
        save_work := RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0} text: #(crate::i18n::tr("Save repositories")) i18n_text: "Save repositories"}
        work_note := Text {}
    }
}

/// One owner-API answer; stale ones are dropped.
#[derive(Clone, Debug)]
struct Reply {
    request: u64,
    agent_id: String,
    saving: bool,
    result: Result<Value, String>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct WorkSection {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    agent_id: String,
    #[rust]
    editable: bool,
    #[rust]
    rooms: Vec<(String, String)>,
    /// The saved reply, `Null` until read.
    #[rust]
    saved: Value,
    #[rust]
    request: u64,
    #[rust]
    note: String,
}

impl Widget for WorkSection {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(reply) = action.downcast_ref::<Reply>() {
                self.apply_reply(cx, reply);
            }
        }
        if self.view.button(cx, ids!(save_work)).clicked(actions) {
            self.save(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WorkSection {
    /// Shows `agent_id`'s work list; read once per Agent while it runs here.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        self.rooms = rooms;
        self.editable = editable.unwrap_or(false);
        if self.agent_id != agent_id {
            self.agent_id = agent_id.to_owned();
            self.saved = Value::Null;
            self.note.clear();
        }
        if self.editable && self.saved.is_null() && !self.agent_id.is_empty() {
            self.saved = json!({});
            self.send(None);
        }
        self.render(cx);
    }
    fn send(&mut self, change: Option<Value>) {
        let Ok(service) = Service::for_current() else {
            self.note = crate::i18n::tr("Signed out").to_owned();
            return;
        };
        self.request += 1;
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
        match &reply.result {
            Ok(value) => {
                self.saved = value.clone();
                let work = work_lines::saved(value);
                self.view.text_input(cx, ids!(work_repos)).set_text(cx, &work_lines::render(&work));
                if let Some(room) = work.first().and_then(|w| self.rooms.iter().position(|(id, _)| *id == w.room_id)) {
                    self.view.drop_down(cx, ids!(work_room)).set_selected_item(cx, room);
                }
                self.note = if reply.saving { crate::i18n::tr("Saved").to_owned() } else { String::new() };
            }
            Err(error) => self.note = crate::i18n::format("Could not save repositories: {error}", &[("error", error.clone())]),
        }
        self.render(cx);
    }
    fn save(&mut self, cx: &mut Cx) {
        let Some((room, _)) = self.rooms.get(self.view.drop_down(cx, ids!(work_room)).selected_item()) else {
            self.note = crate::i18n::tr("Add this Agent to a Room first; the Rooms it is in are listed here.").to_owned();
            return self.render(cx);
        };
        let text = self.view.text_input(cx, ids!(work_repos)).text();
        match work_lines::parse(&text, room, &work_lines::saved(&self.saved), chrono::Utc::now().timestamp()) {
            Ok(work) => {
                self.note = crate::i18n::tr("Saving…").to_owned();
                self.send(Some(json!({"work": work})));
            }
            Err(reason) => self.note = reason,
        }
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let names: Vec<String> = if self.rooms.is_empty() { vec!["-".into()] } else { self.rooms.iter().map(|(_, n)| n.clone()).collect() };
        self.view.drop_down(cx, ids!(work_room)).set_labels(cx, names);
        for id in [ids!(work_repos), ids!(work_room)] {
            self.view.widget(cx, id).set_disabled(cx, !self.editable);
        }
        self.view.button(cx, ids!(save_work)).set_enabled(cx, self.editable);
        let note = if self.editable { self.note.clone() } else { crate::i18n::tr("Issue watching runs on the device that runs this Agent.").to_owned() };
        self.view.label(cx, ids!(work_note)).set_text(cx, &note);
        self.view.redraw(cx);
    }
}

impl WorkSectionRef {
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_context(cx, agent_id, editable, rooms);
        }
    }
}
