//! The Watches tab's "Review by a different model" switch (ADR 0025): room
//! state that any admin of the room may change, ON unless one turned it off.
//! The issue tracker reads it when it picks a pull request's reviewer.
use crate::hagency::issue_watch::room_review;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.RoomReviewSetting = #(RoomReviewSetting::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fit flow: Down spacing: 2 padding: Inset{left: 12 right: 16 bottom: 8}
        different_model := CheckBox {
            text: #(crate::i18n::tr("Review by a different model")) i18n_text: "Review by a different model"
        }
        different_model_hint := Label {
            width: Fill height: Fit flow: Flow.Right{wrap: true} margin: Inset{left: 4}
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomReviewSetting {
    #[deref]
    view: View,
    #[rust]
    room: Option<OwnedRoomId>,
}

impl Widget for RoomReviewSetting {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(room) = self.room.clone() else { return };
        if actions.iter().any(|a| a.downcast_ref::<ReviewSettingChanged>().is_some_and(|c| c.room_id == room)) {
            self.view.redraw(cx);
        }
        if let Some(on) = self.view.check_box(cx, ids!(different_model)).changed(actions) {
            save(&room, on);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let shown = self.room.as_deref().map_or(Shown::Loading, current);
        if let Shown::Ready { on, .. } = shown {
            self.view.check_box(cx, ids!(different_model)).set_active(cx, on, Animate::No);
        }
        let editable = matches!(shown, Shown::Ready { editable: true, .. });
        self.view.widget(cx, ids!(different_model)).set_disabled(cx, !editable);
        self.view.label(cx, ids!(different_model_hint)).set_text(cx, &hint(&shown));
        self.view.draw_walk(cx, scope, walk)
    }
}

impl RoomReviewSettingRef {
    /// Shows `room`'s setting, reading it again.
    pub fn show(&self, cx: &mut Cx, room: &RoomId) {
        let Some(mut setting) = self.borrow_mut() else { return };
        setting.room = Some(room.to_owned());
        refresh(room);
        setting.view.redraw(cx);
    }
}

/// What the switch shows for one room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Loading,
    /// `on`, and whether this account may change it.
    Ready { on: bool, editable: bool },
    Failed(String),
}

/// Posted when a room's switch was read or saved.
#[derive(Clone, Debug)]
pub struct ReviewSettingChanged {
    pub room_id: OwnedRoomId,
}

static SHOWN: LazyLock<Mutex<HashMap<(String, OwnedRoomId), Shown>>> = LazyLock::new(Default::default);

use super::account;
fn set(account: String, room: &RoomId, shown: Shown) {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), shown);
    Cx::post_action(ReviewSettingChanged { room_id: room.to_owned() });
}
pub fn current(room: &RoomId) -> Shown {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned().unwrap_or(Shown::Loading)
}

/// The hint under the switch.
pub fn hint(shown: &Shown) -> String {
    use crate::i18n::{format, tr};
    match shown {
        Shown::Loading => tr("Loading the review setting…").to_owned(),
        Shown::Failed(error) => format("The review setting is unavailable: {error}", &[("error", error.clone())]),
        Shown::Ready { editable: false, .. } => tr("Only a room admin can change this.").to_owned(),
        Shown::Ready { .. } => tr("A pull request one Agent opened is reviewed by an Agent on another model (Claude or Codex) when the room has one.").to_owned(),
    }
}

/// Read the switch for `room` (and whether this account may change it).
pub fn refresh(room: &RoomId) {
    let account = account();
    // This account's room, taken now: an account switch meanwhile never reads or writes as another one (GPT review).
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else {
        return set(account, room, Shown::Failed(crate::i18n::tr("Room not found").to_owned()));
    };
    crate::sliding_sync::spawn_async_task(async move { read_into(account, joined).await });
}

async fn read_into(account: String, joined: matrix_sdk::Room) {
    let shown = match room_review::prefers_different_model(&joined).await {
        Ok(on) => Shown::Ready { on, editable: room_review::can_change(&joined).await },
        Err(error) => Shown::Failed(error),
    };
    set(account, joined.room_id(), shown);
}

/// Save `on` for `room`, then read it back.
pub fn save(room: &RoomId, on: bool) {
    let account = account();
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else {
        return set(account, room, Shown::Failed(crate::i18n::tr("Room not found").to_owned()));
    };
    set(account.clone(), room, Shown::Loading);
    crate::sliding_sync::spawn_async_task(async move {
        match room_review::set_different_model(&joined, on).await {
            Ok(()) => read_into(account, joined).await,
            Err(error) => set(account, joined.room_id(), Shown::Failed(error)),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_says_who_may_change_it_and_why_it_failed() {
        assert_eq!(hint(&Shown::Ready { on: true, editable: false }), "Only a room admin can change this.");
        assert!(hint(&Shown::Ready { on: false, editable: true }).contains("another model"));
        assert_eq!(hint(&Shown::Failed("403".into())), "The review setting is unavailable: 403");
    }
}
