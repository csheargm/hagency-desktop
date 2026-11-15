//! The Skills list in the room information pane: every team skill by name and
//! description, Edit and Delete, and one form to add or edit a skill (paste
//! Markdown). Members without the room's power level see it read-only.
use super::io::Loaded;
use super::model::{self, Change};
use super::ops;
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let SkillText = Label {
        width: Fill height: Fit flow: Flow.Right{wrap: true}
        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
    }
    let SkillButton = RobrixNeutralIconButton {
        width: Fit height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
    }

    mod.widgets.RoomSkillsPanel = #(RoomSkillsPanel::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fill flow: Down
        skills_intro := SkillText {
            margin: Inset{left: 16 right: 16 top: 4 bottom: 8}
            text: #(crate::i18n::tr("Team skills are step-by-step playbooks your Agents can follow in this room. Agents can suggest skills; you approve them."))
            i18n_text: "Team skills are step-by-step playbooks your Agents can follow in this room. Agents can suggest skills; you approve them."
        }
        skills_notice := SkillText {visible: false margin: Inset{left: 16 right: 16 bottom: 8}}
        skills_list := PortalList {
            width: Fill height: Fill flow: Down
            Skill := View {
                width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
                name := Label {
                    width: Fill height: Fit
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                description := SkillText {}
                actions := View {width: Fill height: Fit flow: Right spacing: 6
                    edit := SkillButton {text: #(crate::i18n::tr("Edit")) i18n_text: "Edit"}
                    remove := RobrixNegativeIconButton {
                        width: Fit height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                        text: #(crate::i18n::tr("Delete")) i18n_text: "Delete"
                    }
                }
            }
            Filler := View {width: Fill height: 24}
        }
        form := View {width: Fill height: Fit flow: Down spacing: 8 padding: 12
            skill_name := TextInput {width: Fill height: Fit
                empty_text: #(crate::i18n::tr("Skill name, e.g. release-notes")) i18n_empty_text: "Skill name, e.g. release-notes"}
            skill_description := TextInput {width: Fill height: Fit
                empty_text: #(crate::i18n::tr("One-line description")) i18n_empty_text: "One-line description"}
            skill_body := TextInput {width: Fill height: 120 is_multiline: true submit_on_enter: false
                empty_text: #(crate::i18n::tr("Steps in Markdown (paste here)")) i18n_empty_text: "Steps in Markdown (paste here)"}
            View {width: Fill height: Fit flow: Right spacing: 8
                skill_save := RobrixPositiveIconButton {
                    width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                    text: #(crate::i18n::tr("Add skill")) i18n_text: "Add skill"
                }
                skill_cancel := SkillButton {visible: false height: 32 text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomSkillsPanel {
    #[deref]
    pub view: View,
    #[rust]
    pub room: Option<OwnedRoomId>,
    /// The skill being edited in the form, by name.
    #[rust]
    editing: Option<String>,
    /// A save from the form is running: clear the form when it succeeds.
    #[rust]
    awaiting: bool,
    #[rust]
    rows: Vec<hagency::room_skills::Skill>,
}

const FIELDS: [&[LiveId]; 3] = [ids!(skill_name), ids!(skill_description), ids!(skill_body)];

impl RoomSkillsPanel {
    fn snapshot(&self) -> Option<Loaded> {
        let room = self.room.as_ref()?;
        ops::lock().current(room, crate::home::room_memory::store::me().as_deref()).cloned()
    }
    fn submit(&mut self, cx: &mut Cx, change: Change, name: String) {
        let Some(room) = self.room.clone() else { return };
        self.awaiting = matches!(change, Change::Save { .. });
        let subject = name.trim().to_lowercase();
        let key = subject.clone();
        ops::submit(&room, &subject, Box::new(move |loaded| {
            let current = loaded.skills.iter().find(|s| s.name == key);
            model::plan_write(&change, current, &loaded.me, &loaded.levels, loaded.storage)
        }));
        self.view.redraw(cx);
    }
    fn clear_form(&mut self, cx: &mut Cx) {
        self.editing = None;
        for id in FIELDS {
            self.view.text_input(cx, id).set_text(cx, "");
        }
    }
    fn start_editing(&mut self, cx: &mut Cx, skill: &hagency::room_skills::Skill) {
        self.editing = Some(skill.name.clone());
        for (id, text) in FIELDS.into_iter().zip([&skill.name, &skill.description, &skill.body]) {
            self.view.text_input(cx, id).set_text(cx, text);
        }
        self.view.text_input(cx, ids!(skill_body)).set_key_focus(cx);
    }
    fn save_form(&mut self, cx: &mut Cx) {
        let [name, description, body] = FIELDS.map(|id| self.view.text_input(cx, id).text());
        let target = self.editing.clone().unwrap_or(name.clone());
        self.submit(cx, Change::Save { name: target.clone(), description, body }, target);
    }
    fn handle_row_clicks(&mut self, cx: &mut Cx, actions: &Actions) {
        let list = self.view.portal_list(cx, ids!(skills_list));
        for (index, row) in list.items_with_actions(actions) {
            let Some(skill) = self.rows.get(index).cloned() else { continue };
            if row.button(cx, ids!(edit)).clicked(actions) {
                return self.start_editing(cx, &skill);
            }
            if row.button(cx, ids!(remove)).clicked(actions) {
                return self.submit(cx, Change::Delete, skill.name);
            }
        }
    }
    fn finish_save(&mut self, cx: &mut Cx) {
        let Some(room) = self.room.as_ref() else { return };
        let (busy, failed) = {
            let store = ops::lock();
            (store.busy(room), store.errors.contains_key(room))
        };
        if self.awaiting && !busy {
            self.awaiting = false;
            if !failed {
                self.clear_form(cx);
            }
        }
    }
    fn sync_controls(&mut self, cx: &mut Cx) {
        let loaded = self.snapshot();
        let (busy, error) = match self.room.as_ref() {
            Some(room) => {
                let store = ops::lock();
                (store.busy(room), store.errors.get(room).cloned())
            }
            None => (false, None),
        };
        self.rows = loaded.as_ref().map(|l| l.skills.clone()).unwrap_or_default();
        let can_write = loaded.as_ref().is_some_and(Loaded::can_write);
        let notice = notice(loaded.as_ref(), error.as_deref());
        self.view.label(cx, ids!(skills_notice)).set_visible(cx, !notice.is_empty());
        self.view.label(cx, ids!(skills_notice)).set_text(cx, &notice);
        self.view.label(cx, ids!(skills_intro)).set_visible(cx, loaded.is_some() && self.rows.is_empty());
        let editing = self.editing.is_some();
        let save = self.view.button(cx, ids!(skill_save));
        save.set_text(cx, crate::i18n::tr(if editing { "Save" } else { "Add skill" }));
        save.set_enabled(cx, can_write && !busy);
        self.view.button(cx, ids!(skill_cancel)).set_visible(cx, editing);
        self.view.view(cx, ids!(form)).set_visible(cx, can_write);
        // The name is the skill's key: editing keeps it.
        self.view.text_input(cx, ids!(skill_name)).set_is_read_only(cx, editing);
    }
}

/// What the panel says above the list.
pub fn notice(loaded: Option<&Loaded>, error: Option<&str>) -> String {
    let tr = crate::i18n::tr;
    let mut lines: Vec<String> = Vec::new();
    match (loaded, error) {
        (_, Some(error)) => lines.push(error.to_owned()),
        (None, None) => lines.push(tr("Loading team skills…").to_owned()),
        _ => {}
    }
    if let Some(l) = loaded {
        if !l.can_write() {
            lines.push(tr("Only members with enough permission can change team skills.").to_owned());
        }
        if l.unreadable > 0 {
            lines.push(crate::i18n::format("{count} skill events could not be read on this device and are not shown.", &[("count", l.unreadable.to_string())]));
        }
        if l.truncated {
            lines.push(tr("Only recent room history was checked, so older skills may be missing.").to_owned());
        }
    }
    lines.join("\n")
}

impl Widget for RoomSkillsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if let Some(room) = self.room.as_ref()
            && actions.iter().any(|a| a.downcast_ref::<ops::SkillsChanged>().is_some_and(|c| &c.room_id == room))
        {
            self.finish_save(cx);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(skill_save)).clicked(actions) {
            self.save_form(cx);
        } else if self.view.button(cx, ids!(skill_cancel)).clicked(actions) {
            self.clear_form(cx);
        } else {
            self.handle_row_clicks(cx, actions);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync_controls(cx);
        let can_write = self.snapshot().is_some_and(|l| l.can_write());
        let busy = self.room.as_ref().is_some_and(|room| ops::lock().busy(room));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = item.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(skill) = self.rows.get(index) else {
                    list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                    continue;
                };
                let row = list.item(cx, index, id!(Skill));
                row.label(cx, ids!(name)).set_text(cx, &skill.name);
                row.label(cx, ids!(description)).set_text(cx, &skill.description);
                row.view(cx, ids!(actions)).set_visible(cx, can_write);
                for id in [ids!(edit), ids!(remove)] {
                    row.button(cx, id).set_enabled(cx, !busy);
                }
                row.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

impl RoomSkillsPanelRef {
    /// Shows this room's team skills, reloading them.
    pub fn show(&self, cx: &mut Cx, room: &matrix_sdk::ruma::RoomId) {
        let Some(mut panel) = self.borrow_mut() else { return };
        if panel.room.as_deref() != Some(room) {
            panel.room = Some(room.to_owned());
            panel.awaiting = false;
            panel.clear_form(cx);
        }
        ops::refresh(room);
        panel.view.redraw(cx);
    }
}
