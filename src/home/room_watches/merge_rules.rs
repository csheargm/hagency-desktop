//! The Status tab's "Merge rules" section (ADR 0029): the room's merge rules
//! for every member, read from the homeserver, and for an account the room's
//! power levels let send them an editor with Save. The homeserver refuses a
//! save from anyone below the event's level; the editor is only a convenience.
use super::merge_rules_model::{self as model, Draft, Shown};
use crate::hagency::issue_watch::room_rules;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let RulesText = Label {
        width: Fill height: Fit flow: Flow.Right{wrap: true}
        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
    }

    mod.widgets.RoomMergeRules = #(RoomMergeRules::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 16 right: 16 bottom: 8}
        rules_heading := Label {
            width: Fill height: Fit
            text: #(crate::i18n::tr("Merge rules")) i18n_text: "Merge rules"
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        rules_lines := RulesText {}
        rules_hint := RulesText {}
        rules_editor := View {visible: false width: Fill height: Fit flow: Down spacing: 6 padding: Inset{top: 4}
            rules_paths := TextInput {width: Fill height: Fit
                empty_text: #(crate::i18n::tr("Low-risk paths, comma separated")) i18n_empty_text: "Low-risk paths, comma separated"}
            View {width: Fill height: Fit flow: Right spacing: 8
                rules_lines_limit := TextInput {width: 120 height: Fit
                    empty_text: #(crate::i18n::tr("Max changed lines")) i18n_empty_text: "Max changed lines"}
                rules_approvals := TextInput {width: 120 height: Fit
                    empty_text: #(crate::i18n::tr("Approvals")) i18n_empty_text: "Approvals"}
                rules_ci := CheckBox {text: #(crate::i18n::tr("Require CI")) i18n_text: "Require CI"}
            }
            rules_save := RobrixPositiveIconButton {
                width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                text: #(crate::i18n::tr("Save merge rules")) i18n_text: "Save merge rules"
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomMergeRules {
    #[deref]
    view: View,
    #[rust]
    room: Option<OwnedRoomId>,
    /// The read the editor fields were last filled from (refilled only when it changes).
    #[rust]
    filled: Option<Shown>,
}

impl RoomMergeRules {
    fn draft(&self, cx: &mut Cx) -> Draft {
        Draft {
            paths: self.view.text_input(cx, ids!(rules_paths)).text(),
            max_lines: self.view.text_input(cx, ids!(rules_lines_limit)).text(),
            approvals: self.view.text_input(cx, ids!(rules_approvals)).text(),
            require_ci: self.view.check_box(cx, ids!(rules_ci)).active(cx),
        }
    }
    fn fill(&mut self, cx: &mut Cx, shown: &Shown) {
        let Shown::Ready { read, .. } = shown else { return };
        if self.filled.as_ref() == Some(shown) {
            return;
        }
        let draft = model::draft_of(read);
        self.view.text_input(cx, ids!(rules_paths)).set_text(cx, &draft.paths);
        self.view.text_input(cx, ids!(rules_lines_limit)).set_text(cx, &draft.max_lines);
        self.view.text_input(cx, ids!(rules_approvals)).set_text(cx, &draft.approvals);
        self.view.check_box(cx, ids!(rules_ci)).set_active(cx, draft.require_ci, Animate::No);
        self.filled = Some(shown.clone());
    }
}

impl Widget for RoomMergeRules {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(room) = self.room.clone() else { return };
        if actions.iter().any(|a| a.downcast_ref::<MergeRulesChanged>().is_some_and(|c| c.room_id == room)) {
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(rules_save)).clicked(actions)
            && let Shown::Ready { read, editable: true } = current(&room)
        {
            match model::policy_of(&self.draft(cx), &read) {
                Ok(policy) => save(&room, policy, read),
                Err(error) => set_error(&room, error),
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let shown = self.room.as_deref().map_or(Shown::Loading, current);
        self.fill(cx, &shown);
        let mut text = model::lines(&shown);
        text.extend(self.room.as_deref().and_then(error));
        self.view.label(cx, ids!(rules_lines)).set_text(cx, &text.join("\n"));
        self.view.label(cx, ids!(rules_hint)).set_text(cx, &model::hint(&shown));
        self.view.view(cx, ids!(rules_editor)).set_visible(cx, model::editor_shown(&shown));
        self.view.draw_walk(cx, scope, walk)
    }
}

impl RoomMergeRulesRef {
    /// Shows `room`'s rules, reading them again.
    pub fn show(&self, cx: &mut Cx, room: &RoomId) {
        let Some(mut rules) = self.borrow_mut() else { return };
        rules.room = Some(room.to_owned());
        rules.filled = None;
        refresh(room);
        rules.view.redraw(cx);
    }
}

/// Posted when a room's rules were read or saved.
#[derive(Clone, Debug)]
pub struct MergeRulesChanged {
    pub room_id: OwnedRoomId,
}

type Key = (String, OwnedRoomId);
static SHOWN: LazyLock<Mutex<HashMap<Key, Shown>>> = LazyLock::new(Default::default);
static ERRORS: LazyLock<Mutex<HashMap<Key, String>>> = LazyLock::new(Default::default);

use super::account;
fn set(account: String, room: &RoomId, shown: Shown) {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).insert((account, room.to_owned()), shown);
    Cx::post_action(MergeRulesChanged { room_id: room.to_owned() });
}
pub fn current(room: &RoomId) -> Shown {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned().unwrap_or(Shown::Loading)
}
fn error(room: &RoomId) -> Option<String> {
    ERRORS.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned()
}
fn set_error(room: &RoomId, error: String) {
    ERRORS.lock().unwrap_or_else(|p| p.into_inner()).insert((account(), room.to_owned()), error);
    Cx::post_action(MergeRulesChanged { room_id: room.to_owned() });
}

/// Read `room`'s rules (and whether this account may change them).
pub fn refresh(room: &RoomId) {
    let account = account();
    // This account's room, taken now: an account switch meanwhile never reads or writes as another one.
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else {
        return set(account, room, Shown::Failed(crate::i18n::tr("Room not found").to_owned()));
    };
    crate::sliding_sync::spawn_async_task(async move { read_into(account, joined).await });
}

async fn read_into(account: String, joined: matrix_sdk::Room) {
    let shown = match room_rules::read_with_editor(&joined).await {
        Ok((read, editable)) => Shown::Ready { read, editable },
        Err(error) => Shown::Failed(error),
    };
    set(account, joined.room_id(), shown);
}

/// Save `policy` as `room`'s rules, then read them back from the server.
/// The rules are read again first: if they are no longer `opened` (the read
/// the editor was filled from), nothing is saved and the editor shows them.
fn save(room: &RoomId, policy: ::hagency::issue_watch::safeguards::MergePolicy, opened: room_rules::RoomRead) {
    let account = account();
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else {
        return set_error(room, crate::i18n::tr("Room not found").to_owned());
    };
    ERRORS.lock().unwrap_or_else(|p| p.into_inner()).remove(&(account.clone(), room.to_owned()));
    set(account.clone(), room, Shown::Loading);
    crate::sliding_sync::spawn_async_task(async move {
        let checked = match room_rules::read_with_editor(&joined).await {
            Ok((now, editable)) => model::unchanged_since(&opened, &now, editable),
            Err(error) => Err(error),
        };
        if let Err(error) = checked {
            set_error(joined.room_id(), error);
        } else if let Err(error) = room_rules::save(&joined, &policy).await {
            set_error(joined.room_id(), error);
        }
        read_into(account, joined).await;
    });
}
