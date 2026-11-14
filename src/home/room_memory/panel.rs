//! The Memory list in the room information pane: every entry with its author,
//! pin, edit and delete, and one box to add or edit an entry. People without
//! the room's required power level see the list read-only.
use super::io::Loaded;
use super::model::{self, Change};
use super::{ops, store};
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let MemoryText = Label {
        width: Fill height: Fit flow: Flow.Right{wrap: true}
        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
    }
    let MemoryButton = RobrixNeutralIconButton {
        width: Fit height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
    }

    mod.widgets.RoomMemoryPanel = #(RoomMemoryPanel::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fill flow: Down
        memory_intro := MemoryText {
            margin: Inset{left: 16 right: 16 top: 4 bottom: 8}
            text: #(crate::i18n::tr("Facts and decisions your Agents remember for this room. Agents can suggest entries; you approve them."))
            i18n_text: "Facts and decisions your Agents remember for this room. Agents can suggest entries; you approve them."
        }
        memory_notice := MemoryText {visible: false margin: Inset{left: 16 right: 16 bottom: 8}}
        memory_list := PortalList {
            width: Fill height: Fill flow: Down
            Entry := View {
                width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
                text := Label {
                    width: Fill height: Fit flow: Flow.Right{wrap: true}
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                meta := MemoryText {}
                actions := View {width: Fill height: Fit flow: Right spacing: 6
                    pin := MemoryButton {text: #(crate::i18n::tr("Pin")) i18n_text: "Pin"}
                    edit := MemoryButton {text: #(crate::i18n::tr("Edit")) i18n_text: "Edit"}
                    remove := RobrixNegativeIconButton {
                        width: Fit height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                        text: #(crate::i18n::tr("Delete")) i18n_text: "Delete"
                    }
                }
            }
            Filler := View {width: Fill height: 24}
        }
        add_box := View {width: Fill height: Fit flow: Down spacing: 8 padding: 12
            memory_input := TextInput {
                width: Fill height: Fit
                empty_text: #(crate::i18n::tr("Add a fact or decision")) i18n_empty_text: "Add a fact or decision"
            }
            View {width: Fill height: Fit flow: Right spacing: 8
                memory_add := RobrixPositiveIconButton {
                    width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                    text: #(crate::i18n::tr("Add memory")) i18n_text: "Add memory"
                }
                memory_cancel := MemoryButton {visible: false height: 32 text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomMemoryPanel {
    #[deref]
    pub view: View,
    #[rust]
    pub room: Option<OwnedRoomId>,
    /// The entry being edited in the box, by id.
    #[rust]
    editing: Option<String>,
    /// A write from the box is running: clear the box when it succeeds.
    #[rust]
    awaiting: bool,
    /// The entries last drawn, so a click maps to its entry.
    #[rust]
    pub rows: Vec<hagency::room_memory::Entry>,
}

impl RoomMemoryPanel {
    fn snapshot(&self) -> Option<Loaded> {
        let room = self.room.as_ref()?;
        store::lock().loaded(room, store::me().as_deref()).cloned()
    }
    fn submit(&mut self, cx: &mut Cx, change: Change, entry: Option<hagency::room_memory::Entry>) {
        let Some(room) = self.room.clone() else { return };
        let id = entry.as_ref().map_or_else(model::new_entry_id, |e| e.id.clone());
        self.awaiting = matches!(change, Change::Add(_) | Change::Edit(_));
        let subject = id.clone();
        ops::submit(&room, &subject, Box::new(move |loaded| {
            let current = loaded.entries.iter().find(|e| e.id == id).cloned();
            model::plan_write(&change, current.as_ref(), &id, &loaded.me, &loaded.levels, loaded.storage)
        }));
        self.view.redraw(cx);
    }
    fn finish_box_write(&mut self, cx: &mut Cx) {
        let Some(room) = self.room.as_ref() else { return };
        let (busy, failed) = {
            let store = store::lock();
            (store.busy(room), store.error(room).is_some())
        };
        if self.awaiting && !busy {
            self.awaiting = false;
            if !failed {
                self.stop_editing(cx);
            }
        }
    }
    fn stop_editing(&mut self, cx: &mut Cx) {
        self.editing = None;
        self.view.text_input(cx, ids!(memory_input)).set_text(cx, "");
    }
    fn start_editing(&mut self, cx: &mut Cx, entry: &hagency::room_memory::Entry) {
        self.editing = Some(entry.id.clone());
        let input = self.view.text_input(cx, ids!(memory_input));
        input.set_text(cx, &entry.text);
        input.set_key_focus(cx);
    }
    fn submit_box(&mut self, cx: &mut Cx) {
        let text = self.view.text_input(cx, ids!(memory_input)).text();
        match self.editing.clone() {
            Some(id) => {
                let entry = self.rows.iter().find(|e| e.id == id).cloned();
                self.submit(cx, Change::Edit(text), entry);
            }
            None => self.submit(cx, Change::Add(text), None),
        }
    }
    fn handle_row_clicks(&mut self, cx: &mut Cx, actions: &Actions) {
        let list = self.view.portal_list(cx, ids!(memory_list));
        for (index, row) in list.items_with_actions(actions) {
            let Some(entry) = self.rows.get(index).cloned() else { continue };
            if row.button(cx, ids!(pin)).clicked(actions) {
                return self.submit(cx, Change::Pin(!entry.pinned), Some(entry));
            }
            if row.button(cx, ids!(edit)).clicked(actions) {
                return self.start_editing(cx, &entry);
            }
            if row.button(cx, ids!(remove)).clicked(actions) {
                return self.submit(cx, Change::Delete, Some(entry));
            }
        }
    }
    fn notice(loaded: Option<&Loaded>, error: Option<&str>) -> String {
        let tr = crate::i18n::tr;
        let mut lines: Vec<String> = Vec::new();
        match (loaded, error) {
            (_, Some(error)) => lines.push(error.to_owned()),
            (None, None) => lines.push(tr("Loading room memory…").to_owned()),
            _ => {}
        }
        if let Some(l) = loaded {
            if !l.can_write() {
                lines.push(tr("Only members with enough permission can change room memory.").to_owned());
            }
            if l.unreadable > 0 {
                lines.push(crate::i18n::format("{count} memory events could not be read on this device and are not shown.", &[("count", l.unreadable.to_string())]));
            }
            if l.truncated {
                lines.push(tr("Only recent room history was checked, so older entries may be missing.").to_owned());
            }
        }
        lines.join("\n")
    }
    fn sync_controls(&mut self, cx: &mut Cx) {
        let loaded = self.snapshot();
        let (busy, error) = match self.room.as_ref() {
            Some(room) => {
                let store = store::lock();
                (store.busy(room), store.error(room).map(String::from))
            }
            None => (false, None),
        };
        self.rows = loaded.as_ref().map(|l| l.entries.clone()).unwrap_or_default();
        let can_write = loaded.as_ref().is_some_and(Loaded::can_write);
        let notice = Self::notice(loaded.as_ref(), error.as_deref());
        self.view.label(cx, ids!(memory_notice)).set_visible(cx, !notice.is_empty());
        self.view.label(cx, ids!(memory_notice)).set_text(cx, &notice);
        self.view.label(cx, ids!(memory_intro)).set_visible(cx, loaded.is_some() && self.rows.is_empty());
        let editing = self.editing.is_some();
        let add = self.view.button(cx, ids!(memory_add));
        add.set_text(cx, crate::i18n::tr(if editing { "Save" } else { "Add memory" }));
        add.set_enabled(cx, can_write && !busy);
        self.view.button(cx, ids!(memory_cancel)).set_visible(cx, editing);
        self.view.text_input(cx, ids!(memory_input)).set_is_read_only(cx, !can_write);
    }
}

impl Widget for RoomMemoryPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if let Some(room) = self.room.as_ref()
            && actions.iter().any(|a| a.downcast_ref::<store::MemoryChanged>().is_some_and(|c| &c.room_id == room))
        {
            self.finish_box_write(cx);
            self.view.redraw(cx);
        }
        let input = self.view.text_input(cx, ids!(memory_input));
        if self.view.button(cx, ids!(memory_add)).clicked(actions) || input.returned(actions).is_some() {
            self.submit_box(cx);
        } else if self.view.button(cx, ids!(memory_cancel)).clicked(actions) {
            self.stop_editing(cx);
        } else {
            self.handle_row_clicks(cx, actions);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync_controls(cx);
        let can_write = self.snapshot().is_some_and(|l| l.can_write());
        let busy = self.room.as_ref().is_some_and(|room| store::lock().busy(room));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = item.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(entry) = self.rows.get(index) else {
                    list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                    continue;
                };
                let row = list.item(cx, index, id!(Entry));
                row.label(cx, ids!(text)).set_text(cx, &entry.text);
                row.label(cx, ids!(meta)).set_text(cx, &meta_line(entry));
                row.view(cx, ids!(actions)).set_visible(cx, can_write);
                row.button(cx, ids!(pin)).set_text(cx, crate::i18n::tr(if entry.pinned { "Unpin" } else { "Pin" }));
                for id in [ids!(pin), ids!(edit), ids!(remove)] {
                    row.button(cx, id).set_enabled(cx, !busy);
                }
                row.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

/// "Pinned · Added by alice", from what the entry carries.
pub fn meta_line(entry: &hagency::room_memory::Entry) -> String {
    let mut parts = Vec::new();
    if entry.pinned {
        parts.push(crate::i18n::tr("Pinned").to_owned());
    }
    if !entry.author.is_empty() {
        parts.push(crate::i18n::format("Added by {author}", &[("author", entry.author.clone())]));
    }
    parts.join(" · ")
}

impl RoomMemoryPanelRef {
    /// Shows this room's memory, reloading it.
    pub fn show(&self, cx: &mut Cx, room: &matrix_sdk::ruma::RoomId) {
        let Some(mut panel) = self.borrow_mut() else { return };
        if panel.room.as_deref() != Some(room) {
            panel.room = Some(room.to_owned());
            panel.editing = None;
            panel.awaiting = false;
            panel.view.text_input(cx, ids!(memory_input)).set_text(cx, "");
        }
        ops::refresh(room);
        panel.view.redraw(cx);
    }
}
