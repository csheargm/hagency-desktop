//! The Agent's one work setting per room (ADR 0030): "May work in <room>".
//! On, the Agent joins that room's work loop: it claims and fixes issues of
//! the room's linked repositories, pushes its branches, opens and reviews
//! pull requests there, as far as the room's level allows (set in Room info
//! → Status). Saved through the owner API (`{mayWork}`) on the Agent's
//! execution device; nothing the Agent runs can set it.
use super::live;
use crate::hagency::backend::Service;
use makepad_widgets::*;
use serde_json::{Value, json};

/// The rooms shown at most (one tick each).
const MAX_ROOMS: usize = 8;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Muted = Text {draw_text +: {color: mod.widgets.RINX_MUTED}}
    mod.widgets.MayWorkSection = #(MayWorkSection::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 6
        Text {text: #(crate::i18n::tr("Agent work")) i18n_text: "Agent work" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
        Muted {text: #(crate::i18n::tr(super::may_work_section::HINT)) i18n_text: "Tick a room to let this agent claim and fix issues of the room's linked repositories, push its branches and open and review pull requests there. How far it goes is the room's setting, in Room info → Status."}
        may_work_0 := CheckBox {visible: false} may_work_1 := CheckBox {visible: false}
        may_work_2 := CheckBox {visible: false} may_work_3 := CheckBox {visible: false}
        may_work_4 := CheckBox {visible: false} may_work_5 := CheckBox {visible: false}
        may_work_6 := CheckBox {visible: false} may_work_7 := CheckBox {visible: false}
        may_work_note := Muted {}
    }
}

/// Under the title.
pub const HINT: &str = "Tick a room to let this agent claim and fix issues of the room's linked repositories, push its branches and open and review pull requests there. How far it goes is the room's setting, in Room info → Status.";

/// One owner-API answer; stale ones are dropped.
#[derive(Clone, Debug)]
struct Reply {
    request: u64,
    agent_id: String,
    result: Result<Value, String>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MayWorkSection {
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
    /// The rooms the Agent may work in, as last read.
    #[rust]
    on: Vec<String>,
    #[rust]
    request: u64,
    #[rust]
    note: String,
}

/// The tick ids, in order.
const TICKS: [LiveId; MAX_ROOMS] = [
    live_id!(may_work_0), live_id!(may_work_1), live_id!(may_work_2), live_id!(may_work_3),
    live_id!(may_work_4), live_id!(may_work_5), live_id!(may_work_6), live_id!(may_work_7),
];

/// The tick's label for a room.
pub fn label(room_name: &str) -> String {
    crate::i18n::format("May work in {room}", &[("room", room_name.to_owned())])
}

impl Widget for MayWorkSection {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(reply) = action.downcast_ref::<Reply>() {
                self.apply(cx, reply);
            }
        }
        for index in 0..self.rooms.len().min(MAX_ROOMS) {
            if let Some(on) = self.view.check_box(cx, &[TICKS[index]]).changed(actions) {
                let room = self.rooms[index].0.clone();
                self.call(Some(json!({"mayWork": {"roomId": room, "on": on}})));
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl MayWorkSection {
    /// Shows `agent_id`'s rooms; read once per Agent while it runs here.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        let changed = self.agent_id != agent_id || self.editable != (editable == Some(true));
        self.rooms = rooms;
        self.editable = editable == Some(true);
        self.agent_id = agent_id.to_owned();
        if changed && self.editable {
            self.on.clear();
            self.call(None);
        }
        self.note = if self.editable { String::new() } else { crate::i18n::tr("Agent work is kept on the device that runs this agent.").to_owned() };
        self.render(cx);
    }
    fn call(&mut self, change: Option<Value>) {
        self.request += 1;
        let (request, agent_id) = (self.request, self.agent_id.clone());
        let service = Service::for_current();
        crate::sliding_sync::spawn_async_task(async move {
            let result = match service {
                Ok(service) => live::saved(&service, &agent_id, change).await,
                Err(error) => Err(error),
            };
            Cx::post_action(Reply { request, agent_id, result });
        });
    }
    fn apply(&mut self, cx: &mut Cx, reply: &Reply) {
        if reply.request != self.request || reply.agent_id != self.agent_id {
            return;
        }
        match &reply.result {
            Ok(saved) => {
                self.on = super::room_loop::may_work_rooms(saved);
                self.note.clear();
            }
            Err(error) => self.note = crate::i18n::format("Agent work could not be read or saved: {error}", &[("error", error.clone())]),
        }
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        for (index, tick) in TICKS.iter().enumerate() {
            let row = self.view.widget(cx, &[*tick]);
            let Some((id, name)) = self.rooms.get(index).cloned() else {
                row.set_visible(cx, false);
                continue;
            };
            row.set_visible(cx, true);
            row.set_text(cx, &label(&name));
            row.set_disabled(cx, !self.editable);
            self.view.check_box(cx, &[*tick]).set_active(cx, self.on.contains(&id), Animate::No);
        }
        let note = if self.rooms.is_empty() { crate::i18n::tr("Add this agent to a project room first.").to_owned() } else { self.note.clone() };
        self.view.label(cx, ids!(may_work_note)).set_text(cx, &note);
        self.view.redraw(cx);
    }
}

impl MayWorkSectionRef {
    /// See [`MayWorkSection::set_context`].
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, rooms: Vec<(String, String)>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_context(cx, agent_id, editable, rooms);
        }
    }
}
