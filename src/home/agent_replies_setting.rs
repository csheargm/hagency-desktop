//! "Agent replies: In the room / In threads" in a room's information pane
//! (ADR 0023). The choice is the room's `im.hagency.agent_replies` state
//! event, so every member's app and every Agent read the same answer; a room
//! that never chose answers in the main timeline.
use hagency_core::agent_replies::{Mode, STATE_EVENT};
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId, api::client::state::get_state_event_for_key::v3 as get_state, events::StateEventType};

use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentRepliesSetting = #(AgentRepliesSetting::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fit flow: Down spacing: 4
        padding: Inset{left: 16 right: 16 top: 4 bottom: 8}
        agent_replies_row := View {
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            agent_replies_title := Label {
                width: Fill height: Fit
                text: #(crate::i18n::tr("Agent replies")) i18n_text: "Agent replies"
                draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
            }
            agent_replies_mode := mod.widgets.RobrixSettingsDropDown {
                width: 150 height: 32 margin: 0
                labels: [#(crate::i18n::tr("In the room")), #(crate::i18n::tr("In threads"))] i18n_labels: ["In the room", "In threads"]
                selected_item: 0
            }
        }
        agent_replies_hint := Label {
            width: Fill height: Fit flow: Flow.Right{wrap: true}
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }
}

/// What the hint under the choice says.
pub fn hint(mode: Mode) -> &'static str {
    match mode {
        Mode::Room => "Agent answers appear in the room for everyone, quoting the question.",
        Mode::Thread => "Agent answers open a thread under each question.",
    }
}

/// The drop-down row for `mode`, and back.
pub fn index_of(mode: Mode) -> usize {
    match mode {
        Mode::Room => 0,
        Mode::Thread => 1,
    }
}
pub fn mode_at(index: usize) -> Mode {
    if index == 1 { Mode::Thread } else { Mode::Room }
}

/// A room's choice has been read, or a write settled (`Some(saved)`:
/// whether the server accepted it).
#[derive(Clone, Debug)]
pub struct AgentRepliesLoaded {
    pub room_id: OwnedRoomId,
    pub mode: Mode,
    /// The request this answers; only the newest one is shown (GPT review).
    pub ticket: u64,
    pub account: String,
}

static TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn next_ticket() -> u64 {
    TICKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}
fn account() -> String {
    let user = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    format!("{user}#{}", crate::account_session::epoch())
}

#[derive(Script, ScriptHook, Widget)]
pub struct AgentRepliesSetting {
    #[deref] view: View,
    #[rust] room: Option<OwnedRoomId>,
    #[rust] mode: Mode,
    /// The room's choice as the server last confirmed it; a refusal returns here.
    #[rust] confirmed: Mode,
    #[rust] ticket: u64,
}

impl Widget for AgentRepliesSetting {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(loaded) = action.downcast_ref::<AgentRepliesLoaded>()
                && self.room.as_ref() == Some(&loaded.room_id)
                && loaded.ticket == self.ticket
                && loaded.account == account()
            {
                self.confirmed = loaded.mode;
                self.show_mode(cx, loaded.mode);
            }
        }
        if let Some(index) = self.view.drop_down(cx, ids!(agent_replies_mode)).changed(actions)
            && let Some(room) = self.room.clone()
        {
            let wanted = mode_at(index);
            if wanted != self.mode {
                // Shown at once; a refused write puts the confirmed choice back.
                self.ticket = next_ticket();
                self.show_mode(cx, wanted);
                if !save(room, wanted, self.confirmed, self.ticket) {
                    let confirmed = self.confirmed;
                    self.show_mode(cx, confirmed);
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AgentRepliesSetting {
    fn show_mode(&mut self, cx: &mut Cx, mode: Mode) {
        self.mode = mode;
        self.view.drop_down(cx, ids!(agent_replies_mode)).set_selected_item(cx, index_of(mode));
        self.view.label(cx, ids!(agent_replies_hint)).set_text(cx, crate::i18n::tr(hint(mode)));
        self.view.redraw(cx);
    }
}

impl AgentRepliesSettingRef {
    /// Shows `room`'s choice, reading it from the server.
    pub fn show(&self, cx: &mut Cx, room: &RoomId) {
        let Some(mut inner) = self.borrow_mut() else { return };
        if inner.room.as_deref() != Some(room) {
            inner.room = Some(room.to_owned());
            inner.confirmed = Mode::default();
            inner.show_mode(cx, Mode::default());
        }
        inner.ticket = next_ticket();
        load(room.to_owned(), inner.ticket);
    }
}

/// Reads the room's state event from the server: the sliding-sync store does
/// not hold custom state types. A room without one gets the default.
fn load(room_id: OwnedRoomId, ticket: u64) {
    let Some(client) = crate::sliding_sync::get_client() else { return };
    let account = account();
    crate::sliding_sync::spawn_async_task(async move {
        let request = get_state::Request::new(room_id.clone(), StateEventType::from(STATE_EVENT), String::new());
        let mode = match client.send(request).await {
            Ok(response) => serde_json::from_str::<serde_json::Value>(response.event_or_content.get())
                .map(|content| Mode::from_content(&content))
                .unwrap_or_default(),
            // M_NOT_FOUND is the common case: the room never chose. Any other
            // failure also shows the default, which is what Agents then use.
            Err(_) => Mode::default(),
        };
        Cx::post_action(AgentRepliesLoaded { room_id, mode, ticket, account });
    });
}

/// Writes the room's choice; a refusal (most often: only moderators may
/// change room settings) is shown and the previous choice comes back.
/// False when there is no signed-in client to write with.
fn save(room_id: OwnedRoomId, mode: Mode, previous: Mode, ticket: u64) -> bool {
    let Some(client) = crate::sliding_sync::get_client() else { return false };
    let account = account();
    crate::sliding_sync::spawn_async_task(async move {
        let sent = match client.get_room(&room_id) {
            Some(room) => room.send_state_event_raw(STATE_EVENT, "", mode.content()).await.map(|_| ()).map_err(|e| e.to_string()),
            None => Err(crate::i18n::tr("Room not found").to_owned()),
        };
        let shown = match sent {
            Ok(()) => mode,
            Err(reason) => {
                enqueue_popup_notification(
                    crate::i18n::format("Could not change where Agents reply: {reason}", &[("reason", reason)]),
                    PopupKind::Error,
                    Some(6.0),
                );
                previous
            }
        };
        Cx::post_action(AgentRepliesLoaded { room_id, mode: shown, ticket, account });
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Language, text_for};

    #[test]
    fn the_choice_maps_to_the_room_state_and_back() {
        for mode in [Mode::Room, Mode::Thread] {
            assert_eq!(mode_at(index_of(mode)), mode);
            assert_eq!(Mode::from_content(&mode.content()), mode);
        }
        assert_eq!(mode_at(7), Mode::Room, "an unknown row is the default");
        assert_eq!(index_of(Mode::default()), 0, "a room that never chose shows In the room");
    }

    /// GPT review: only the newest read or write for this account shows.
    #[test]
    fn a_stale_answer_never_overwrites_the_shown_choice() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.AgentRepliesSetting {}});
            let widget = WidgetRef::script_from_value(vm, value);
            widget.register_as_ui_root(vm);
            widget
        });
        let room: OwnedRoomId = "!dev:test".try_into().unwrap();
        widget.as_agent_replies_setting().show(&mut cx, &room);
        let ticket = widget.as_agent_replies_setting().borrow().unwrap().ticket;
        let deliver = |cx: &mut Cx, mode: Mode, ticket: u64| {
            let loaded = AgentRepliesLoaded { room_id: room.clone(), mode, ticket, account: account() };
            let actions = cx.capture_actions(|cx| cx.action(loaded));
            widget.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
        };
        deliver(&mut cx, Mode::Thread, ticket - 1);
        assert_eq!(widget.drop_down(&mut cx, ids!(agent_replies_mode)).selected_item(), 0, "an older read is dropped");
        deliver(&mut cx, Mode::Thread, ticket);
        assert_eq!(widget.drop_down(&mut cx, ids!(agent_replies_mode)).selected_item(), 1, "the room chose threads");
        assert_eq!(widget.label(&mut cx, ids!(agent_replies_hint)).text(), crate::i18n::tr(hint(Mode::Thread)));
    }

    #[test]
    fn the_setting_words_have_english_keys_and_chinese_text() {
        for key in ["Agent replies", "In the room", "In threads", hint(Mode::Room), hint(Mode::Thread),
            "Could not change where Agents reply: {reason}"] {
            assert_eq!(text_for(Language::English, key), key);
            let zh = text_for(Language::Chinese, key);
            assert_ne!(zh, key, "{key} has no Chinese text");
            assert_eq!(key.contains("{reason}"), zh.contains("{reason}"));
        }
    }
}
