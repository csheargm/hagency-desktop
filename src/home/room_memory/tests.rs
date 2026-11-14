//! Room memory in the human client: what Accept, edit, pin and delete write in
//! plaintext and encrypted rooms, who may write, card states and rendering,
//! and the list's order. Wire formats come from the backend's own functions.
use super::card::{MemoryProposalCard, status_text};
use super::io::Loaded;
use super::model::*;
use super::panel::{RoomMemoryPanel, meta_line};
use super::store::{self, Dismissed, Progress, TEST_ME};
use hagency::room_memory::{self, Entry, MEMORY_STATE_TYPE, PowerLevels, Storage};
use hagency::room_memory_timeline::{TimelineScan, reduce_timeline};
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use serde_json::{Value, json};

const ADMIN: &str = "@admin:s";
const GUEST: &str = "@guest:s";

fn levels() -> PowerLevels {
    PowerLevels {
        users: [(ADMIN.to_string(), 100), (GUEST.to_string(), 0)].into(),
        users_default: 0,
        events: Default::default(),
        state_default: Some(50),
    }
}
fn proposal() -> Proposal {
    let content = room_memory::proposal_content("p1", "We use metric units", "Said twice", "$e", "Helper");
    parse_proposal(&content).expect("a valid card parses")
}
fn entry(id: &str, text: &str, pinned: bool, rev: u64) -> Entry { Entry { id: id.into(), text: text.into(), author: "@a:s".into(), source_event: None, pinned, rev } }
/// A state event as the room returns it after `write` was sent.
fn state_event(write: &Write) -> Value {
    let Delivery::State { state_key } = &write.delivery else { panic!("expected a state write") };
    json!({"state_key": state_key, "content": write.content})
}
/// A decrypted timeline event as the scanner sees it.
fn timeline_event(write: &Write, sender: &str) -> Value {
    assert_eq!(write.delivery, Delivery::Timeline);
    json!({"type": MEMORY_STATE_TYPE, "sender": sender, "content": write.content})
}
fn scan(events: Vec<Value>) -> TimelineScan { TimelineScan { events, ..Default::default() } }

#[test]
fn accept_writes_state_in_a_plaintext_room_and_a_timeline_event_in_an_encrypted_one() {
    let state = accept_plan(&proposal(), None, ADMIN, &levels(), Storage::State).unwrap();
    assert_eq!(state.delivery, Delivery::State { state_key: "p1".into() });
    assert_eq!((state.content["text"].as_str(), state.content["rev"].as_u64(), state.content["author"].as_str()), (Some("We use metric units"), Some(1), Some("Helper")));
    assert!(state.content.get("deleted").is_none() && state.content.get("id").is_none());
    let timeline = accept_plan(&proposal(), None, ADMIN, &levels(), Storage::Timeline).unwrap();
    assert_eq!(timeline.delivery, Delivery::Timeline);
    assert_eq!((timeline.content["id"].as_str(), timeline.content["deleted"].as_bool()), (Some("p1"), Some(false)));
}

#[test]
fn accepting_a_proposal_already_in_memory_takes_the_next_revision() {
    let existing = entry("p1", "older", false, 4);
    let write = accept_plan(&proposal(), Some(&existing), ADMIN, &levels(), Storage::State).unwrap();
    assert_eq!(write.content["rev"], 5);
}

#[test]
fn members_below_the_required_level_cannot_accept_edit_pin_delete_or_add() {
    let current = entry("a", "x", false, 1);
    assert_eq!(accept_plan(&proposal(), None, GUEST, &levels(), Storage::State), Err(WriteError::PowerLevel));
    for storage in [Storage::State, Storage::Timeline] {
        for change in [Change::Edit("y".into()), Change::Pin(true), Change::Delete, Change::Add("z".into())] {
            assert_eq!(plan_write(&change, Some(&current), "a", GUEST, &levels(), storage), Err(WriteError::PowerLevel), "{change:?}");
        }
    }
}

#[test]
fn edit_bumps_the_revision_and_keeps_the_author_in_both_storages() {
    let current = entry("a", "old", true, 3);
    let state = plan_write(&Change::Edit("  new   text ".into()), Some(&current), "a", ADMIN, &levels(), Storage::State).unwrap();
    assert_eq!((state.content["text"].as_str(), state.content["rev"].as_u64(), state.content["author"].as_str(), state.content["pinned"].as_bool()), (Some("new text"), Some(4), Some("@a:s"), Some(true)));
    let timeline = plan_write(&Change::Edit("new".into()), Some(&current), "a", ADMIN, &levels(), Storage::Timeline).unwrap();
    assert_eq!(timeline.content["rev"], 4);
}

#[test]
fn delete_is_empty_state_content_or_a_tombstone_revision_and_the_entry_disappears() {
    let current = entry("a", "old", false, 2);
    let state = plan_write(&Change::Delete, Some(&current), "a", ADMIN, &levels(), Storage::State).unwrap();
    assert_eq!(state.content, json!({}));
    assert!(room_memory::reduce(&[state_event(&state)]).is_empty());
    let created = plan_write(&Change::Add("keep".into()), None, "a", ADMIN, &levels(), Storage::Timeline).unwrap();
    let tomb = plan_write(&Change::Delete, Some(&current), "a", ADMIN, &levels(), Storage::Timeline).unwrap();
    assert_eq!((tomb.content["deleted"].as_bool(), tomb.content["rev"].as_u64()), (Some(true), Some(3)));
    let view = reduce_timeline(&scan(vec![timeline_event(&created, ADMIN), timeline_event(&tomb, ADMIN)]), &levels());
    assert!(view.entries.is_empty(), "a tombstone with a higher revision removes the entry");
}

#[test]
fn add_validates_the_text() {
    assert_eq!(plan_write(&Change::Add("   ".into()), None, "n", ADMIN, &levels(), Storage::State), Err(WriteError::Empty));
    let long = "x".repeat(501);
    assert_eq!(plan_write(&Change::Add(long), None, "n", ADMIN, &levels(), Storage::State), Err(WriteError::TooLong));
    assert_eq!(plan_write(&Change::Edit("x".into()), None, "n", ADMIN, &levels(), Storage::State), Err(WriteError::Missing));
    let ok = plan_write(&Change::Add("a\nb".into()), None, "n", ADMIN, &levels(), Storage::State).unwrap();
    assert_eq!((ok.content["text"].as_str(), ok.content["author"].as_str(), ok.content["rev"].as_u64()), (Some("a b"), Some(ADMIN), Some(1)));
    assert!(new_entry_id().len() <= 64);
}

/// The whole life of an entry as a person sees it: an Agent proposes, a member
/// accepts, edits, pins and deletes, and the list is rebuilt after each write.
#[test]
fn proposal_to_list_flow_in_a_plaintext_room() {
    let mut events: Vec<Value> = Vec::new();
    let apply = |events: &mut Vec<Value>, write: &Write| {
        let event = state_event(write);
        events.retain(|e| e["state_key"] != event["state_key"]);
        events.push(event);
        room_memory::reduce(events)
    };
    let accepted = apply(&mut events, &accept_plan(&proposal(), None, ADMIN, &levels(), Storage::State).unwrap());
    assert_eq!((accepted.len(), accepted[0].text.as_str(), accepted[0].rev), (1, "We use metric units", 1));
    let edited = apply(&mut events, &plan_write(&Change::Edit("Metric units only".into()), Some(&accepted[0]), "p1", ADMIN, &levels(), Storage::State).unwrap());
    assert_eq!((edited[0].text.as_str(), edited[0].rev), ("Metric units only", 2));
    let pinned = apply(&mut events, &plan_write(&Change::Pin(true), Some(&edited[0]), "p1", ADMIN, &levels(), Storage::State).unwrap());
    assert!(pinned[0].pinned && pinned[0].rev == 3);
    assert!(apply(&mut events, &plan_write(&Change::Delete, Some(&pinned[0]), "p1", ADMIN, &levels(), Storage::State).unwrap()).is_empty());
}

#[test]
fn proposal_to_list_flow_in_an_encrypted_room_ignores_members_without_power() {
    let accept = accept_plan(&proposal(), None, ADMIN, &levels(), Storage::Timeline).unwrap();
    let forged = json!({"type": MEMORY_STATE_TYPE, "sender": GUEST, "content": {"id":"p1","text":"forged","author":"x","pinned":false,"rev":9,"deleted":false}});
    let view = reduce_timeline(&scan(vec![timeline_event(&accept, ADMIN), forged]), &levels());
    assert_eq!((view.entries.len(), view.entries[0].text.as_str(), view.ignored), (1, "We use metric units", 1));
    let edit = plan_write(&Change::Edit("Edited".into()), Some(&view.entries[0]), "p1", ADMIN, &levels(), Storage::Timeline).unwrap();
    let view = reduce_timeline(&scan(vec![timeline_event(&accept, ADMIN), timeline_event(&edit, ADMIN)]), &levels());
    assert_eq!((view.entries[0].text.as_str(), view.entries[0].rev), ("Edited", 2));
}

#[test]
fn list_shows_pinned_first_then_newest_revision() {
    let events = vec![
        json!({"state_key":"a","content":{"text":"plain old","rev":1}}),
        json!({"state_key":"b","content":{"text":"pinned","pinned":true,"rev":1}}),
        json!({"state_key":"c","content":{"text":"plain new","rev":5}}),
    ];
    let order: Vec<_> = room_memory::reduce(&events).into_iter().map(|e| e.id).collect();
    assert_eq!(order, ["b", "c", "a"]);
    assert_eq!(meta_line(&Entry { pinned: true, author: "Helper".into(), ..entry("b", "t", true, 1) }), "Pinned · Added by Helper");
}

#[test]
fn proposal_cards_parse_and_plain_notices_fall_back() {
    let p = proposal();
    assert_eq!((p.entry.id.as_str(), p.entry.text.as_str(), p.reason.as_str(), p.entry.author.as_str()), ("p1", "We use metric units", "Said twice", "Helper"));
    assert_eq!(parse_proposal(&json!({"msgtype":"m.notice","body":"hello"})), None);
    assert_eq!(parse_proposal(&json!({"io.hagency.memory_proposal":{"text":"x"}})), None, "an id is required");
    assert_eq!(parse_proposal(&json!({"io.hagency.memory_proposal":{"id":"i","text":"  "}})), None);
}

#[test]
fn card_state_and_buttons_follow_the_proposal_and_the_viewer() {
    assert_eq!(card_state(false, false, false, None), CardState::Pending);
    assert_eq!(card_state(true, true, true, Some("e")), CardState::Added, "memory wins over everything");
    assert_eq!(card_state(false, true, true, None), CardState::Dismissed);
    assert_eq!(card_state(false, false, true, None), CardState::Adding);
    assert_eq!(card_state(false, false, false, Some("boom")), CardState::Failed("boom".into()));
    let failed = CardState::Failed("e".into());
    assert!(can_accept(&CardState::Pending, true) && can_accept(&failed, true), "a failed accept can be retried");
    assert!(!can_accept(&CardState::Pending, false), "an unprivileged member never sees Accept");
    assert!(!can_accept(&CardState::Adding, true) && !can_accept(&CardState::Added, true) && !can_accept(&CardState::Dismissed, true));
    assert!(can_dismiss(&CardState::Pending) && !can_dismiss(&CardState::Added));
    assert_eq!(status_text(&CardState::Added, true).as_deref(), Some("Added to room memory"));
    assert_eq!(status_text(&CardState::Pending, true), None);
    assert!(status_text(&CardState::Pending, false).is_some());
}

fn loaded(room: &RoomId, me: &str, entries: Vec<Entry>) -> Loaded { Loaded { room_id: room.to_owned(), me: me.into(), storage: Storage::State, entries, levels: levels(), unreadable: 0, truncated: false, ignored: 0 } }
fn room(id: &str) -> OwnedRoomId { RoomId::parse(id).unwrap() }

#[test]
fn store_derives_card_states_from_memory_and_progress() {
    let room = room("!store:s");
    let mut s = store::Store::default();
    assert_eq!(s.card_state(&room, Some(ADMIN), "p1"), CardState::Pending);
    s.set_progress(&room, "p1", Some(Progress::Working));
    assert!(s.busy(&room) && s.card_state(&room, Some(ADMIN), "p1") == CardState::Adding);
    s.set_progress(&room, "p1", Some(Progress::Failed("no".into())));
    assert_eq!(s.card_state(&room, Some(ADMIN), "p1"), CardState::Failed("no".into()));
    s.set_loaded(loaded(&room, ADMIN, vec![entry("p1", "t", false, 1)]));
    assert_eq!(s.card_state(&room, Some(ADMIN), "p1"), CardState::Added);
    assert_eq!(s.card_state(&room, Some(GUEST), "p1"), CardState::Failed("no".into()), "another account's memory is not used");
}

#[test]
fn dismissals_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dismissed.json");
    let room = room("!d:s");
    let mut d = Dismissed::default();
    d.insert(&path, &room, "p1").unwrap();
    let reloaded = Dismissed::load(&path);
    assert!(reloaded.contains(&room, "p1") && !reloaded.contains(&room, "p2"));
    assert!(!Dismissed::load(&dir.path().join("none.json")).contains(&room, "p1"));
    assert!(Dismissed::default().insert(&dir.path().join("missing/x.json"), &room, "p").is_err(), "a failed save is reported");
}

fn draw(cx: &mut Cx, widget: &mut impl Widget) {
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    cx.redraw_all();
    let pass = DrawPass::new(cx);
    pass.set_size(cx, dvec2(280.0, 700.0));
    let mut list = DrawList2d::new(cx);
    let overlay = Overlay { draw_list: DrawList::new(cx) };
    let event = DrawEvent::default();
    let mut draw = CxDraw::new(cx, &event);
    let mut cx2d = Cx2d::new(&mut draw);
    cx2d.begin_pass(&pass, None);
    list.begin_always(&mut cx2d);
    overlay.begin(&mut cx2d);
    cx2d.begin_root_turtle(dvec2(280.0, 700.0), Layout::flow_overlay());
    widget.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk { width: Size::Fixed(280.0), height: Size::Fixed(700.0), ..Walk::default() });
    overlay.end(&mut cx2d);
    cx2d.end_pass_sized_turtle();
    list.end(&mut cx2d);
    cx2d.end_pass(&pass);
}
fn build<W>(make: impl FnOnce(&mut ScriptVm) -> W) -> (Cx, W) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        vm.bx.captured_errors = Some(Vec::new());
        crate::app::register_widgets(vm);
        let widget = make(vm);
        assert!(vm.take_errors().is_empty(), "the widget DSL builds without script errors");
        widget
    });
    (cx, widget)
}
fn build_panel() -> (Cx, RoomMemoryPanel) {
    build(|vm| {
        let value = script_eval!(vm, {mod.widgets.RoomMemoryPanel {visible: true}});
        RoomMemoryPanel::script_from_value(vm, value)
    })
}
fn build_card() -> (Cx, MemoryProposalCard) {
    build(|vm| {
        let value = script_eval!(vm, {mod.widgets.MemoryProposalCard {}});
        MemoryProposalCard::script_from_value(vm, value)
    })
}

#[test]
fn panel_is_writable_for_privileged_members_and_read_only_for_others() {
    for (me, writable, id) in [(ADMIN, true, "!panel-a:s"), (GUEST, false, "!panel-g:s")] {
        TEST_ME.with(|t| *t.borrow_mut() = Some(me.into()));
        let (mut cx, mut panel) = build_panel();
        let room = room(id);
        let events = [json!({"state_key":"a","content":{"text":"plain","rev":1}}), json!({"state_key":"b","content":{"text":"pinned","pinned":true,"rev":1}})];
        store::lock().set_loaded(loaded(&room, me, room_memory::reduce(&events)));
        panel.room = Some(room);
        draw(&mut cx, &mut panel);
        assert_eq!(panel.rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["b", "a"], "pinned first");
        assert_eq!(panel.view.button(&mut cx, ids!(memory_add)).borrow().unwrap().enabled(), writable, "{me}");
        assert!(!panel.view.label(&mut cx, ids!(memory_intro)).visible(), "entries hide the empty state");
        assert_eq!(panel.view.label(&mut cx, ids!(memory_notice)).visible(), !writable, "read-only members are told why");
    }
}

#[test]
fn empty_room_shows_what_memory_is() {
    TEST_ME.with(|t| *t.borrow_mut() = Some(ADMIN.into()));
    let (mut cx, mut panel) = build_panel();
    let room = room("!panel-empty:s");
    store::lock().set_loaded(loaded(&room, ADMIN, vec![]));
    panel.room = Some(room);
    draw(&mut cx, &mut panel);
    assert!(panel.view.label(&mut cx, ids!(memory_intro)).visible());
    assert!(panel.view.label(&mut cx, ids!(memory_intro)).text().starts_with("Facts and decisions your Agents remember"));
}

#[test]
fn card_renders_its_states_for_each_viewer() {
    for (me, id, buttons_for_accept) in [(ADMIN, "!card-a:s", true), (GUEST, "!card-g:s", false)] {
        TEST_ME.with(|t| *t.borrow_mut() = Some(me.into()));
        let (mut cx, mut card) = build_card();
        let room = room(id);
        store::lock().set_loaded(loaded(&room, me, vec![]));
        card.context = Some((room.clone(), proposal()));
        card.sync(&mut cx);
        assert!(card.view.visible());
        assert_eq!(card.view.label(&mut cx, ids!(memory_text)).text(), "We use metric units");
        assert_eq!(card.view.label(&mut cx, ids!(memory_reason)).text(), "Reason: Said twice");
        assert_eq!(card.view.button(&mut cx, ids!(memory_accept)).visible(), buttons_for_accept);
        assert!(card.view.button(&mut cx, ids!(memory_dismiss)).visible());
        store::lock().set_loaded(loaded(&room, me, vec![entry("p1", "We use metric units", false, 1)]));
        card.sync(&mut cx);
        assert_eq!(card.view.label(&mut cx, ids!(memory_status)).text(), "Added to room memory");
        assert!(!card.view.button(&mut cx, ids!(memory_accept)).visible() && !card.view.button(&mut cx, ids!(memory_dismiss)).visible());
    }
    let (mut cx, mut card) = build_card();
    card.sync(&mut cx);
    assert!(!card.view.visible(), "a message without a proposal stays a plain notice");
}
