//! The Status tab's top section (ADR 0030): the room's agent work in one
//! place. Everyone sees the status line ("Autonomous · 2 agents working",
//! "Paused by Anna at 13:40") and who can change it; the room's owner and
//! maintainers get the level choice and Resume; every member gets Pause; and
//! "Let agents work on a repo" sets a room up in one action. Everything is
//! read from the homeserver; it refuses anyone below an event's level.
use crate::hagency::room_autonomy::{lines, server, state::RoomState};
use ::hagency::issue_watch::safeguards::room_autonomy::Level;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let AutonomyText = Label {
        width: Fill height: Fit flow: Flow.Right{wrap: true}
        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
    }

    mod.widgets.RoomAutonomy = #(RoomAutonomy::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 16 right: 16 bottom: 10}
        Label {
            width: Fill height: Fit
            text: #(crate::i18n::tr("Agent work")) i18n_text: "Agent work"
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        autonomy_status := Label {
            width: Fill height: Fit flow: Flow.Right{wrap: true}
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        autonomy_level := RobrixSettingsDropDown {visible: false width: Fill height: 36 labels: ["-"]
            popup_menu_position: PopupMenuPosition.BelowInput
        }
        autonomy_level_text := AutonomyText {}
        AutonomyText {text: #(crate::i18n::tr(crate::hagency::room_autonomy::lines::WHO_CAN_CHANGE)) i18n_text: "Owner and maintainers can change this. Anyone in the room can pause."}
        View {width: Fill height: Fit flow: Right spacing: 8
            autonomy_pause := RobrixNegativeIconButton {visible: false width: Fit height: 40 padding: Inset{left: 16 right: 16 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("⏸ Pause agent work")) i18n_text: "⏸ Pause agent work"}
            autonomy_resume := RobrixPositiveIconButton {visible: false width: Fit height: 40 padding: Inset{left: 16 right: 16 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("▶ Resume agent work")) i18n_text: "▶ Resume agent work"}
        }
        setup_box := View {visible: false width: Fill height: Fit flow: Down spacing: 6 padding: Inset{top: 6}
            Label {width: Fill height: Fit text: #(crate::i18n::tr("Let agents work on a repo")) i18n_text: "Let agents work on a repo"
                draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
            View {width: Fill height: Fit flow: Right spacing: 8
                setup_repo := TextInput {width: Fill height: Fit
                    empty_text: #(crate::i18n::tr("owner/name of a GitHub repository")) i18n_empty_text: "owner/name of a GitHub repository"}
                setup_start := RobrixPositiveIconButton {width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                    text: #(crate::i18n::tr("Let agents work")) i18n_text: "Let agents work"}
            }
            AutonomyText {text: #(crate::i18n::tr(super::autonomy::SETUP_HINT)) i18n_text: "Links the repository to this room, lets your agents in this room work on it, and starts the room at Assist: agents claim, build and review, and a person merges."}
        }
        autonomy_note := AutonomyText {}
    }
}

/// Under "Let agents work on a repo".
pub const SETUP_HINT: &str = "Links the repository to this room, lets your agents in this room work on it, and starts the room at Assist: agents claim, build and review, and a person merges.";

/// What the section shows for a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Loading,
    Failed(String),
    Ready(RoomState),
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomAutonomy {
    #[deref]
    view: View,
    #[rust]
    room: Option<OwnedRoomId>,
    /// The level the drop-down shows, so a redraw never reads as a choice.
    #[rust]
    shown_level: Option<usize>,
}

impl Widget for RoomAutonomy {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(room) = self.room.clone() else { return };
        if actions.iter().any(|a| a.downcast_ref::<AutonomyChanged>().is_some_and(|c| c.room_id == room)) {
            self.view.redraw(cx);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(autonomy_level)).changed(actions)
            && Some(index) != self.shown_level
            && let Some(level) = Level::ALL.get(index).copied()
        {
            act(&room, Act::Level(level));
        }
        if self.view.button(cx, ids!(autonomy_pause)).clicked(actions) {
            act(&room, Act::Pause);
        }
        if self.view.button(cx, ids!(autonomy_resume)).clicked(actions) {
            act(&room, Act::Resume);
        }
        if self.view.button(cx, ids!(setup_start)).clicked(actions) {
            let repo = self.view.text_input(cx, ids!(setup_repo)).text();
            if !repo.trim().is_empty() {
                self.view.text_input(cx, ids!(setup_repo)).set_text(cx, "");
                act(&room, Act::Setup(repo.trim().to_owned()));
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let shown = self.room.as_deref().map_or(Shown::Loading, current);
        self.fill(cx, &shown);
        self.view.draw_walk(cx, scope, walk)
    }
}

impl RoomAutonomy {
    fn fill(&mut self, cx: &mut Cx, shown: &Shown) {
        let room = self.room.as_ref().map(|r| r.to_string()).unwrap_or_default();
        let (status, state) = match shown {
            Shown::Loading => (crate::i18n::tr("Reading the room's agent work…").to_owned(), None),
            Shown::Failed(error) => (crate::i18n::format("Agent work is unavailable: {error}", &[("error", error.clone())]), None),
            Shown::Ready(state) => (lines::status_line(state, crate::hagency::run_controls::running_in(&room), display_name, local_time), Some(state)),
        };
        self.view.label(cx, ids!(autonomy_status)).set_text(cx, &status);
        let can_change = state.is_some_and(|s| s.can_change);
        let level = state.and_then(|s| s.level.as_ref().ok().copied()).unwrap_or_default();
        let dropdown = self.view.drop_down(cx, ids!(autonomy_level));
        dropdown.set_visible(cx, can_change);
        if can_change && self.shown_level != Some(lines::level_index(level)) {
            dropdown.set_labels(cx, lines::LEVEL_LABELS.iter().map(|l| crate::i18n::tr(l).to_owned()).collect());
            dropdown.set_selected_item(cx, lines::level_index(level));
            self.shown_level = Some(lines::level_index(level));
        }
        let read_only = state.filter(|_| !can_change).map(|_| crate::i18n::tr(lines::LEVEL_LABELS[lines::level_index(level)]).to_owned());
        self.view.label(cx, ids!(autonomy_level_text)).set_text(cx, read_only.as_deref().unwrap_or_default());
        let paused = state.is_some_and(|s| s.pause.is_some());
        self.view.button(cx, ids!(autonomy_pause)).set_visible(cx, state.is_some_and(|s| s.can_pause) && !paused);
        self.view.button(cx, ids!(autonomy_resume)).set_visible(cx, can_change && paused);
        self.view.view(cx, ids!(setup_box)).set_visible(cx, state.is_some_and(|s| s.can_link));
        let note = self.room.as_deref().and_then(note);
        self.view.label(cx, ids!(autonomy_note)).set_text(cx, note.as_deref().unwrap_or_default());
    }
}

/// A member's name for the status line: the local part of their Matrix ID.
fn display_name(id: &str) -> String {
    id.trim_start_matches('@').split(':').next().unwrap_or(id).to_owned()
}
/// A server time (ms) as the local "13:40".
fn local_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
}

impl RoomAutonomyRef {
    /// Shows `room`'s agent work, reading it again.
    pub fn show(&self, cx: &mut Cx, room: &RoomId) {
        let Some(mut section) = self.borrow_mut() else { return };
        section.room = Some(room.to_owned());
        section.shown_level = None;
        refresh(room);
        section.view.redraw(cx);
    }
}

/// Posted when a room's agent work was read or changed.
#[derive(Clone, Debug)]
pub struct AutonomyChanged {
    pub room_id: OwnedRoomId,
}

type Key = (String, OwnedRoomId);
static SHOWN: LazyLock<Mutex<HashMap<Key, Shown>>> = LazyLock::new(Default::default);
static NOTES: LazyLock<Mutex<HashMap<Key, String>>> = LazyLock::new(Default::default);

use super::account;
fn set(account: String, room: &RoomId, shown: Shown) {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), shown);
    Cx::post_action(AutonomyChanged { room_id: room.to_owned() });
}
/// What the section (and the room header's badge) shows for `room`.
pub fn current(room: &RoomId) -> Shown {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned().unwrap_or(Shown::Loading)
}
/// The room header's badge for `room`, once its agent work was read.
pub fn badge_of(room: &RoomId) -> Option<&'static str> {
    match current(room) {
        Shown::Ready(state) => lines::badge(&state),
        Shown::Loading | Shown::Failed(_) => None,
    }
}
fn note(room: &RoomId) -> Option<String> {
    NOTES.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned()
}
fn set_note(account: String, room: &RoomId, text: String) {
    NOTES.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), text);
    Cx::post_action(AutonomyChanged { room_id: room.to_owned() });
}

/// Whether `room`'s agent work was read on this desktop (shown in its header or Status tab).
pub(super) fn was_read(room: &RoomId) -> bool {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&(account(), room.to_owned()))
}

/// Read `room`'s agent work from the homeserver; from then on also on each
/// change another member makes ([`super::autonomy_watch`]).
pub fn refresh(room: &RoomId) {
    let account = account();
    let client = crate::sliding_sync::get_client();
    if let Some(client) = &client {
        super::autonomy_watch::attach(client);
    }
    let Some(joined) = client.and_then(|c| c.get_room(room)) else {
        return set(account, room, Shown::Failed(crate::i18n::tr("Room not found").to_owned()));
    };
    crate::sliding_sync::spawn_async_task(async move { read_into(account, &joined).await });
}

async fn read_into(account: String, joined: &matrix_sdk::Room) {
    let shown = server::read(joined).await.map_or_else(Shown::Failed, Shown::Ready);
    set(account, joined.room_id(), shown);
}

enum Act {
    Level(Level),
    Pause,
    Resume,
    Setup(String),
}

/// Do `act` in `room` as the account that clicked, then read the room again.
fn act(room: &RoomId, act: Act) {
    let account = account();
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else {
        return set_note(account, room, crate::i18n::tr("Room not found").to_owned());
    };
    crate::sliding_sync::spawn_async_task(async move {
        let outcome = match act {
            Act::Level(level) => server::set_level(&joined, level).await.map(Option::unwrap_or_default),
            // This desktop stops its own loop runs now; other owners' desktops on their next check.
            Act::Pause => pause_then_stop(server::pause(&joined), crate::hagency::room_autonomy::stop::stop_loop_runs(&joined)).await,
            Act::Resume => server::resume(&joined).await.map(Option::unwrap_or_default),
            Act::Setup(repo) => super::autonomy_setup::run(&joined, &repo).await.map(|done| super::autonomy_setup::done_line(&done)),
        };
        set_note(account.clone(), joined.room_id(), outcome.unwrap_or_else(|error| error));
        read_into(account, &joined).await;
        super::refresh(joined.room_id());
    });
}

/// Pause, then stop this desktop's loop runs once the pause is saved, even
/// when its room line was refused (GPT review); the note names both.
async fn pause_then_stop(
    pause: impl std::future::Future<Output = Result<server::Announced, String>>,
    stop: impl std::future::Future<Output = usize>,
) -> Result<String, String> {
    let announced = pause.await?;
    let notes = [stopped_line(stop.await), announced.unwrap_or_default()];
    Ok(notes.into_iter().filter(|n| !n.is_empty()).collect::<Vec<_>>().join(" "))
}

/// After Pause: how many of this desktop's loop runs were stopped.
fn stopped_line(stopped: usize) -> String {
    match stopped {
        0 => String::new(),
        1 => crate::i18n::tr("Stopped 1 run of your agents' work here.").to_owned(),
        n => crate::i18n::format("Stopped {count} runs of your agents' work here.", &[("count", n.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    /// GPT review (P2): a saved pause stops the runs even when its room line was refused.
    #[test]
    fn a_saved_pause_stops_runs_even_when_the_room_line_fails() {
        let note = run(pause_then_stop(async { Ok(Some("The change is saved, but the room was not told: M_FORBIDDEN".to_owned())) }, async { 2 }));
        assert_eq!(note, Ok("Stopped 2 runs of your agents' work here. The change is saved, but the room was not told: M_FORBIDDEN".to_owned()));
        let stopped = std::cell::Cell::new(false);
        let refused = run(pause_then_stop(async { Err("Could not pause the room: no".to_owned()) }, async { stopped.set(true); 1 }));
        assert_eq!(refused, Err("Could not pause the room: no".to_owned()));
        assert!(!stopped.get(), "an unsaved pause stops nothing");
    }
}
