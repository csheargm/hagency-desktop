//! "+" → New room: one screen, Buzz style. Name, optional topic, Public (off)
//! and End-to-end encryption (off: hosted Agents only answer in unencrypted
//! rooms), Cancel/Create pinned. No parent space is needed; spaces keep their
//! own wizard under "+" → New space (`space_management`).
//!
//! After creation the room opens, and the owner's default Agent joins it
//! through `bring_agent::owner_created_room`.
use super::space_management::CreateRoomConfig;
use crate::sliding_sync::{get_client, spawn_async_task};
use crate::utils::RoomNameId;
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Hint = Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style.font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}}
    let Field = View {width: Fill height: Fit flow: Down spacing: 4}
    let Input = RobrixTextInput {
        width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
    }
    let Action = RobrixNeutralIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Primary = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}

    mod.widgets.NewRoomDialog = #(NewRoomDialog::register_widget(vm)) {
        ..mod.widgets.SmallModal
        width: Fill{max: 480} spacing: 14
        ModalTitle {text: #(crate::i18n::tr("New room")) i18n_text: "New room" margin: Inset{bottom: 6}}
        Field {Text {text: #(crate::i18n::tr("Name")) i18n_text: "Name"} room_name := Input {empty_text: #(crate::i18n::tr("Give it a name")) i18n_empty_text: "Give it a name"}}
        Field {Text {text: #(crate::i18n::tr("Description · optional")) i18n_text: "Description · optional"} room_topic := Input {empty_text: #(crate::i18n::tr("What is this place for?")) i18n_empty_text: "What is this place for?"}}
        Field {
            public := CheckBox {width: Fill text: #(crate::i18n::tr("Public")) i18n_text: "Public"}
            Hint {text: #(crate::i18n::tr("Allow people to discover and join.")) i18n_text: "Allow people to discover and join."}
        }
        Field {
            encrypted := CheckBox {width: Fill text: #(crate::i18n::tr("End-to-end encryption")) i18n_text: "End-to-end encryption"}
            Hint {text: #(crate::i18n::tr("Off by default: hosted agents only answer in unencrypted rooms.")) i18n_text: "Off by default: hosted agents only answer in unencrypted rooms."}
        }
        status := Text {}
        ModalButtonsRow {
            cancel := Action {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
            create := Primary {text: #(crate::i18n::tr("Create room")) i18n_text: "Create room"}
        }
    }
}

/// Opens or closes the dialog; the app shell owns its modal.
#[derive(Clone, Debug)]
pub enum NewRoomAction {
    Open,
    Close,
}

#[derive(Debug)]
struct Created {
    request: u64,
    result: Result<RoomNameId, String>,
}

static REQUEST: AtomicU64 = AtomicU64::new(1);
fn next_request() -> u64 {
    REQUEST.fetch_add(1, Ordering::Relaxed)
}

/// What the form asks the server for: a plain room, never a space, no parent.
pub(crate) fn new_room_config(name: &str, topic: &str, public: bool, encrypted: bool) -> CreateRoomConfig {
    CreateRoomConfig {
        name: name.trim().to_owned(),
        topic: topic.trim().to_owned(),
        public,
        // A public room cannot use the encrypted preset; the form keeps them exclusive.
        encrypted: encrypted && !public,
        space: false,
        parent: None,
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct NewRoomDialog {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] request: u64,
    #[rust] busy: bool,
}

impl Widget for NewRoomDialog {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl NewRoomDialog {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(created) = action.downcast_ref::<Created>() && created.request == self.request {
                self.finish(cx, &created.result);
            }
        }
        if self.view.button(cx, ids!(cancel)).clicked(actions) { cx.action(NewRoomAction::Close); }
        if self.view.button(cx, ids!(create)).clicked(actions) { self.create(cx); }
        if self.view.text_input(cx, ids!(room_name)).returned(actions).is_some() { self.create(cx); }
        if self.view.text_input(cx, ids!(room_name)).changed(actions).is_some() { self.render_create(cx); }
        // Public rooms cannot be encrypted with this preset; ticking one clears the other.
        if self.view.check_box(cx, ids!(public)).changed(actions) == Some(true) {
            self.view.check_box(cx, ids!(encrypted)).set_active(cx, false, Animate::Yes);
        }
        if self.view.check_box(cx, ids!(encrypted)).changed(actions) == Some(true) {
            self.view.check_box(cx, ids!(public)).set_active(cx, false, Animate::Yes);
        }
    }

    fn open(&mut self, cx: &mut Cx) {
        self.request = next_request();
        self.busy = false;
        for id in [ids!(room_name), ids!(room_topic)] {
            self.view.text_input(cx, id).set_text(cx, "");
        }
        for id in [ids!(public), ids!(encrypted)] {
            self.view.check_box(cx, id).set_active(cx, false, Animate::No);
        }
        self.view.label(cx, ids!(status)).set_text(cx, "");
        self.render_create(cx);
        self.view.text_input(cx, ids!(room_name)).set_key_focus(cx);
    }

    fn config(&mut self, cx: &mut Cx) -> CreateRoomConfig {
        new_room_config(
            &self.view.text_input(cx, ids!(room_name)).text(),
            &self.view.text_input(cx, ids!(room_topic)).text(),
            self.view.check_box(cx, ids!(public)).active(cx),
            self.view.check_box(cx, ids!(encrypted)).active(cx),
        )
    }

    fn render_create(&mut self, cx: &mut Cx) {
        let named = !self.view.text_input(cx, ids!(room_name)).text().trim().is_empty();
        self.view.button(cx, ids!(create)).set_enabled(cx, named && !self.busy);
        self.view.redraw(cx);
    }

    fn create(&mut self, cx: &mut Cx) {
        if self.busy { return; }
        let config = self.config(cx);
        let request = match config.request() {
            Ok(request) => request,
            Err(error) => { self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(&error.to_string())); return; }
        };
        let Some(client) = get_client() else {
            self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr("Sign in again"));
            return;
        };
        self.busy = true;
        self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr("Creating…"));
        self.render_create(cx);
        let (ticket, name) = (self.request, config.name.clone());
        spawn_async_task(async move {
            let result = client.create_room(request).await
                .map(|room| RoomNameId::new(matrix_sdk::RoomDisplayName::Named(name.into()), room.room_id().to_owned()))
                .map_err(|error| error.to_string());
            Cx::post_action(Created { request: ticket, result });
        });
    }

    /// Opens the new room and brings the owner's newest Agent along.
    fn finish(&mut self, cx: &mut Cx, result: &Result<RoomNameId, String>) {
        self.busy = false;
        match result {
            Ok(room) => {
                crate::hagency::bring_agent::owner_created_room(room.room_id().clone());
                cx.action(NewRoomAction::Close);
                cx.action(crate::app::AppStateAction::NavigateToRoom {
                    room_to_close: None,
                    destination_room: crate::room::BasicRoomDetails::RoomId(room.clone()),
                });
            }
            Err(error) => {
                let message = format!("{}: {error}", crate::i18n::tr("Could not create the room"));
                self.view.label(cx, ids!(status)).set_text(cx, &message);
                self.render_create(cx);
            }
        }
    }
}


impl NewRoomDialogRef {
    pub fn action(&self, cx: &mut Cx, modal: ModalRef, action: &NewRoomAction) {
        let Some(mut inner) = self.borrow_mut() else { return };
        match action {
            NewRoomAction::Open => {
                inner.open(cx);
                modal.open(cx);
            }
            NewRoomAction::Close => {
                // A late creation result for this opening is ignored.
                inner.request = next_request();
                inner.busy = false;
                modal.close(cx);
            }
        }
    }
}

#[cfg(test)]
#[path = "new_room_dialog_tests.rs"]
mod tests;
