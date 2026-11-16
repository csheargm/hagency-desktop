//! The Create agent dialog's "Add to rooms" checklist: the owner's joined
//! rooms (no DMs, no spaces), the open room ticked. After creation the new
//! Agent is bound into each ticked room through the Add agent path
//! (`bring_agent::bind_into_rooms` → `Service::invite_agent_to_room`).
use super::CreateAgentDialog;
use crate::hagency::bring_agent::{self, MAX_ROOM_CHOICES, room_choices, ticked_rooms};
use crate::sliding_sync::{get_client, spawn_async_task};
use crate::utils::RoomNameId;
use makepad_widgets::*;
use serde_json::Value;

/// The checklist rows in the dialog's DSL, top to bottom.
const ROOM_TICKS: [LiveId; MAX_ROOM_CHOICES] = [
    live_id!(room_0), live_id!(room_1), live_id!(room_2), live_id!(room_3),
    live_id!(room_4), live_id!(room_5), live_id!(room_6), live_id!(room_7),
];

/// The joined rooms an Agent can be added to, as `(room id, name)`.
#[derive(Debug)]
pub(super) struct RoomsLoaded {
    request: u64,
    rooms: Vec<(String, String)>,
}

/// Joined rooms that are neither spaces nor DMs. A room whose DM flag cannot
/// be read is left out rather than risk offering a private chat.
async fn joined_group_rooms() -> Vec<(String, String)> {
    let Some(client) = get_client() else { return Vec::new() };
    let mut rooms = Vec::new();
    for room in client.joined_rooms() {
        if !is_group_room(room.is_space(), room.is_direct().await.ok()) { continue; }
        rooms.push((room.room_id().to_string(), RoomNameId::from_room(&room).await.to_string()));
    }
    rooms
}

/// A room an Agent can be added to: not a space, and known not to be a DM.
fn is_group_room(space: bool, direct: Option<bool>) -> bool {
    !space && direct == Some(false)
}

impl CreateAgentDialog {
    /// Lists the rooms for this opening, the room the app shows ticked.
    pub(super) fn load_rooms(&mut self, cx: &mut Cx) {
        self.state.rooms.clear();
        self.state.current_room = crate::home::sidebar_action_menu::plus_menu::open_room().map(|room| room.room_id().to_string());
        self.render_rooms(cx, false);
        let request = self.request;
        spawn_async_task(async move {
            Cx::post_action(RoomsLoaded { request, rooms: joined_group_rooms().await });
        });
    }

    pub(super) fn handle_room_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(loaded) = action.downcast_ref::<RoomsLoaded>() && loaded.request == self.request {
                self.apply_rooms(cx, loaded.rooms.clone());
            }
        }
        for (index, id) in ROOM_TICKS.iter().enumerate() {
            if let Some(on) = self.view.check_box(cx, &[*id]).changed(actions)
                && let Some(choice) = self.state.rooms.get_mut(index) {
                choice.ticked = on;
            }
        }
    }

    /// Fills the checklist from the joined rooms, the open room first and ticked.
    fn apply_rooms(&mut self, cx: &mut Cx, rooms: Vec<(String, String)>) {
        let more = rooms.len() > MAX_ROOM_CHOICES;
        self.state.rooms = room_choices(rooms, self.state.current_room.as_deref(), MAX_ROOM_CHOICES);
        self.render_rooms(cx, more);
    }

    fn render_rooms(&mut self, cx: &mut Cx, more: bool) {
        for (index, id) in ROOM_TICKS.iter().enumerate() {
            let choice = self.state.rooms.get(index);
            let row = self.view.widget(cx, &[*id]);
            row.set_visible(cx, choice.is_some());
            if let Some(choice) = choice {
                row.set_text(cx, &choice.name);
                self.view.check_box(cx, &[*id]).set_active(cx, choice.ticked, Animate::No);
            }
        }
        let hint = if self.state.rooms.is_empty() { "No rooms to add it to yet." }
            else if more { "Add it to other rooms later with Add agent in each room." } else { "" };
        self.view.label(cx, ids!(rooms_hint)).set_text(cx, crate::i18n::tr(hint));
        self.view.redraw(cx);
    }

    /// After creation: bind the new Agent into every ticked room.
    pub(super) fn bind_ticked(&self, created: &Value) {
        let Some(puppet) = created["creation"]["agent"]["puppetMxid"].as_str() else { return };
        bring_agent::bind_into_rooms(self.state.name.trim().to_owned(), puppet.to_owned(), ticked_rooms(&self.state.rooms));
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(cx: &mut Cx) -> CreateAgentDialog {
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.CreateAgentDialog {}});
            let dialog = CreateAgentDialog::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty(), "the dialog's DSL evaluates without errors");
            dialog
        })
    }
    fn rooms(count: usize) -> Vec<(String, String)> {
        (0..count).map(|i| (format!("!r{i}:s"), format!("Room {i}"))).collect()
    }

    #[test]
    fn add_to_rooms_lists_joined_rooms_with_the_open_room_ticked() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = dialog(&mut cx);
        dialog.state.current_room = Some("!r2:s".into());
        dialog.apply_rooms(&mut cx, rooms(3));
        let first = dialog.view.check_box(&mut cx, ids!(room_0));
        assert!(first.active(&cx), "the open room is ticked");
        assert_eq!(dialog.view.widget(&mut cx, ids!(room_0)).text(), "Room 2");
        assert!(!dialog.view.check_box(&mut cx, ids!(room_1)).active(&cx));
        assert!(dialog.view.widget(&mut cx, ids!(room_2)).visible() && !dialog.view.widget(&mut cx, ids!(room_3)).visible());
        assert_eq!(dialog.view.label(&mut cx, ids!(rooms_hint)).text(), "");
        // Ticking another row adds it to the rooms the new Agent is bound into.
        dialog.state.rooms[1].ticked = true;
        assert_eq!(ticked_rooms(&dialog.state.rooms), vec![("!r2:s".to_owned(), "Room 2".to_owned()), ("!r0:s".to_owned(), "Room 0".to_owned())]);
    }

    /// Every entry point sends the same `CreateAgentAction::Open`; with or
    /// without an open room, the joined rooms reach the checklist.
    #[test]
    fn rooms_load_from_every_entry_point_and_only_the_open_room_is_ticked() {
        use crate::app::{AppStateAction, SelectedRoom};
        use crate::home::sidebar_action_menu::plus_menu;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = dialog(&mut cx);
        let team = RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Room 1".into()), matrix_sdk::ruma::OwnedRoomId::try_from("!r1:s").unwrap());
        for open in [None, Some(team)] {
            let focus = match &open {
                Some(room) => AppStateAction::RoomFocused(SelectedRoom::JoinedRoom { room_name_id: room.clone() }),
                None => AppStateAction::FocusNone,
            };
            plus_menu::observe(&cx.capture_actions(|cx| cx.action(focus)));
            dialog.request += 1;
            dialog.load_rooms(&mut cx);
            assert_eq!(dialog.state.current_room.as_deref(), open.as_ref().map(|r| r.room_id().as_str()));
            let loaded = cx.capture_actions(|cx| cx.action(RoomsLoaded { request: dialog.request, rooms: rooms(3) }));
            dialog.handle_room_actions(&mut cx, &loaded);
            assert_eq!(dialog.state.rooms.len(), 3, "open room: {open:?}");
            assert!(dialog.view.widget(&mut cx, ids!(room_2)).visible());
            let ticked: Vec<String> = ticked_rooms(&dialog.state.rooms).into_iter().map(|(id, _)| id).collect();
            assert_eq!(ticked, open.iter().map(|r| r.room_id().to_string()).collect::<Vec<_>>());
            // A reply for an earlier opening is ignored.
            let stale = cx.capture_actions(|cx| cx.action(RoomsLoaded { request: dialog.request - 1, rooms: rooms(1) }));
            dialog.handle_room_actions(&mut cx, &stale);
            assert_eq!(dialog.state.rooms.len(), 3);
        }
        plus_menu::observe(&cx.capture_actions(|cx| cx.action(AppStateAction::FocusNone)));
    }

    #[test]
    fn only_rooms_known_not_to_be_dms_or_spaces_are_offered() {
        assert!(is_group_room(false, Some(false)));
        assert!(!is_group_room(true, Some(false)), "a space");
        assert!(!is_group_room(false, Some(true)), "a DM");
        assert!(!is_group_room(false, None), "DM flag unreadable");
    }

    /// The scar: on a 1042-point window the checklist began ~615 points into a
    /// scroll area capped at 60% of the window, so it was clipped below the
    /// fold with no scroll hint. The heading and the first rows must be
    /// inside the visible fields area on a laptop-height window.
    #[test]
    fn add_to_rooms_rows_are_inside_the_visible_fields_area() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = dialog(&mut cx);
        dialog.apply_rooms(&mut cx, rooms(4));
        let size = dvec2(1280., 760.);
        for _ in 0..2 {
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, size);
            let mut list = DrawList2d::new(&mut cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(size, Layout::flow_overlay());
            let walk = dialog.walk(&mut cx2d);
            let _ = dialog.draw_walk(&mut cx2d, &mut Scope::empty(), walk);
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        let rect = |dialog: &mut CreateAgentDialog, cx: &mut Cx, id: LiveId| dialog.view.widget(cx, &[id]).area().rect(cx);
        let fields = rect(&mut dialog, &mut cx, id!(fields));
        for id in [id!(rooms_heading), id!(room_0), id!(room_3)] {
            let row = rect(&mut dialog, &mut cx, id);
            assert!(row.size.y > 0.0 && row.pos.y + row.size.y <= fields.pos.y + fields.size.y, "{id} {row:?} is inside {fields:?}");
        }
    }

    #[test]
    fn add_to_rooms_says_when_there_are_none_or_more_than_fit() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = dialog(&mut cx);
        dialog.apply_rooms(&mut cx, Vec::new());
        assert!(!dialog.view.widget(&mut cx, ids!(room_0)).visible());
        assert_eq!(dialog.view.label(&mut cx, ids!(rooms_hint)).text(), "No rooms to add it to yet.");
        dialog.apply_rooms(&mut cx, rooms(MAX_ROOM_CHOICES + 2));
        assert!(dialog.view.widget(&mut cx, ids!(room_7)).visible());
        assert!(ticked_rooms(&dialog.state.rooms).is_empty(), "no open room, nothing pre-ticked");
        assert_eq!(dialog.view.label(&mut cx, ids!(rooms_hint)).text(), "Add it to other rooms later with Add agent in each room.");
    }
}
