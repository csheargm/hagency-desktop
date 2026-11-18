//! The user panel's "Rooms in common" tab: the joined rooms this user is a
//! member of, read from the local store only, each row opening its room.

use std::sync::atomic::{AtomicU64, Ordering};
use makepad_widgets::*;
use matrix_sdk::ruma::{events::room::member::MembershipState, OwnedUserId};
use crate::{app::AppStateAction, home::rooms_list::RoomsListRef, room::BasicRoomDetails, utils::RoomNameId};
use super::rooms_in_common::{KnownRoom, RoomInCommon, RoomsInCommon, rooms_in_common};

/// Tags each load so a late reply for a previously shown user is ignored.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Posted by the background load when the rooms in common are known.
#[derive(Clone, Debug)]
struct RoomsInCommonLoaded {
    request: u64,
    found: RoomsInCommon,
}

/// Emitted when a row opened its room, so the pane can close itself.
#[derive(Clone, Debug, Default)]
pub enum UserRoomsAction {
    OpenedRoom,
    #[default]
    None,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.UserRoomsInCommon = #(UserRoomsInCommon::register_widget(vm)) {
        width: Fill, height: Fit
        flow: Down, spacing: 6

        rooms_title := Label {
            width: Fill, height: Fit
            draw_text +: {
                color: mod.widgets.RINX_INK,
                text_style: USERNAME_TEXT_STYLE { font_size: (11 * mod.widgets.RINX_TEXT_SCALE) },
            }
            text: #(crate::i18n::tr("Rooms in common"))
        }

        rooms_status := Label {
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            draw_text +: {
                color: (MESSAGE_TEXT_COLOR),
                text_style: MESSAGE_TEXT_STYLE { font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE) },
            }
            text: ""
        }

        // A PortalList needs a bounded height inside the scrolling profile view.
        rooms_list := PortalList {
            width: Fill, height: 180
            Row := View {
                width: Fill, height: Fit
                padding: Inset{top: 2, bottom: 2}
                room_button := RobrixNeutralIconButton {
                    width: Fill, height: 34, margin: 0
                    align: Align{x: 0 y: 0.5}
                    padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
                    icon_walk: Walk{width: 0 height: 0}
                    text: ""
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct UserRoomsInCommon {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] request: u64,
    /// `None` while the load is running.
    #[rust] found: Option<RoomsInCommon>,
}

impl Widget for UserRoomsInCommon {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(loaded) = action.downcast_ref::<RoomsInCommonLoaded>() {
                if loaded.request == self.request {
                    self.found = Some(loaded.found.clone());
                    self.view.redraw(cx);
                }
            }
        }
        let list = self.view.portal_list(cx, ids!(rooms_list));
        for (index, item) in list.items_with_actions(actions) {
            if !item.button(cx, ids!(room_button)).clicked(actions) { continue; }
            let Some(room) = self.found.as_ref().and_then(|found| found.rooms.get(index)).cloned() else { continue };
            open_room(cx, &room);
            cx.widget_action(self.widget_uid(), UserRoomsAction::OpenedRoom);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.label(cx, ids!(rooms_status)).set_text(cx, &status_text(self.found.as_ref()));
        let rooms: &[RoomInCommon] = self.found.as_ref().map_or(&[], |found| &found.rooms);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, rooms.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(room) = rooms.get(index) else { continue };
                let item = list.item(cx, index, id!(Row));
                item.button(cx, ids!(room_button)).set_text(cx, &room.name);
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
}

impl UserRoomsInCommon {
    /// Starts gathering the rooms in common with `user`, dropping any earlier result.
    pub fn load(&mut self, cx: &mut Cx, user: OwnedUserId) {
        self.request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.found = None;
        self.view.portal_list(cx, ids!(rooms_list)).set_first_id_and_scroll(0, 0.0);
        spawn_load(user, self.request);
        self.view.redraw(cx);
    }
}

impl UserRoomsInCommonRef {
    /// See [`UserRoomsInCommon::load()`].
    pub fn load(&self, cx: &mut Cx, user: OwnedUserId) {
        if let Some(mut inner) = self.borrow_mut() { inner.load(cx, user); }
    }

    /// Whether one of this tab's rows opened a room in `actions`.
    pub fn opened_room(&self, actions: &Actions) -> bool {
        actions.find_widget_action(self.widget_uid()).is_some_and(|action| matches!(action.cast(), UserRoomsAction::OpenedRoom))
    }
}

/// The line above the list: loading, or empty. An empty list says so; when
/// some rooms' member lists are not on this device it says the list covers
/// only the loaded ones, since the user may be in one of the others. A list
/// with rooms needs no note.
pub(super) fn status_text(found: Option<&RoomsInCommon>) -> String {
    let Some(found) = found else { return crate::i18n::tr("Loading…").to_owned() };
    let key = match (found.rooms.is_empty(), found.incomplete > 0) {
        (false, _) => return String::new(),
        (true, false) => "No rooms in common",
        (true, true) => "No rooms in common in the member lists loaded on this device",
    };
    crate::i18n::tr(key).to_owned()
}

fn open_room(cx: &mut Cx, room: &RoomInCommon) {
    let destination_room = match cx.get_global::<RoomsListRef>().get_room_name(&room.room_id) {
        Some(room_name_id) => BasicRoomDetails::Name(room_name_id),
        None => BasicRoomDetails::RoomId(RoomNameId::empty(room.room_id.clone())),
    };
    cx.action(AppStateAction::NavigateToRoom { room_to_close: None, destination_room });
}

/// Reads every joined room's cached membership of `user` from the store
/// (no per-room network request) and posts the result to the UI.
fn spawn_load(user: OwnedUserId, request: u64) {
    crate::sliding_sync::spawn_async_task(async move {
        let joined = match crate::sliding_sync::get_client() {
            Some(client) => known_rooms(&client, &user).await,
            None => Vec::new(),
        };
        Cx::post_action(RoomsInCommonLoaded { request, found: rooms_in_common(&user, &joined) });
    });
}

async fn known_rooms(client: &matrix_sdk::Client, user: &OwnedUserId) -> Vec<KnownRoom> {
    let mut out = Vec::new();
    for room in client.joined_rooms() {
        // A failed store read leaves this room unknown: it is counted as
        // incomplete below (noted when the list comes up empty) instead of failing it.
        let lookup = room.get_member_no_sync(user).await;
        let joined = matches!(&lookup, Ok(Some(member)) if *member.membership() == MembershipState::Join);
        out.push(KnownRoom {
            room_id: room.room_id().to_owned(),
            name: room.cached_display_name().map(|name| name.to_string()).or_else(|| room.name()).unwrap_or_default(),
            joined_members: if joined { [user.clone()].into() } else { Default::default() },
            members_complete: lookup.is_ok() && room.are_members_synced(),
            hidden: crate::moments::is_moments(&room),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::rooms_in_common::RoomInCommon;
    use matrix_sdk::ruma::OwnedRoomId;

    #[test]
    fn status_line_says_loading_or_empty_and_notes_unloaded_rooms_only_when_empty() {
        assert_eq!(status_text(None), "Loading…");
        assert_eq!(status_text(Some(&RoomsInCommon::default())), "No rooms in common");
        let room = RoomInCommon { room_id: OwnedRoomId::try_from("!a:s").unwrap(), name: "Ops".into() };
        assert_eq!(status_text(Some(&RoomsInCommon { rooms: vec![room.clone()], incomplete: 0 })), "");
        assert_eq!(status_text(Some(&RoomsInCommon { rooms: vec![room], incomplete: 2 })), "",
            "Nov 18: a found list carried a filler note about unloaded member lists");
        assert_eq!(status_text(Some(&RoomsInCommon { rooms: vec![], incomplete: 2 })),
            "No rooms in common in the member lists loaded on this device");
    }
}
