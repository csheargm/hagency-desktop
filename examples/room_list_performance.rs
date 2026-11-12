//! Offline room-list fixture for checking row reuse and theme reapplication.
use makepad_widgets::*;
use matrix_sdk::RoomDisplayName;
use rinx::{
    home::rooms_list::{JoinedRoomInfo, LatestEventPreview},
    room::{FetchedRoomAvatar, reply_preview::CollapsiblePreviewWidgetRefExt},
    shared::html_or_plaintext::HtmlOrPlaintextWidgetRefExt,
    theme::{self, Appearance, Selection},
    utils::RoomNameId,
};

app_main!(App);

fn mobile() -> bool {
    std::env::args().any(|arg| arg == "--mobile")
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(600, 700)
                body +: {
                    width: Fill height: Fill flow: Down
                    header := Label {text: "Offline room list"}
                    View {
                        width: Fill height: Fill flow: Right
                        list := PortalList {
                            width: 340 height: Fill
                            DesktopEntry := RoomsListEntry {}
                            MobileEntry := MobileRoomsListEntry {}
                        }
                        reply := RepliedToMessage {
                            visible: true expandable: true width: Fill
                        }
                    }
                    desktop_input := RoomInputBar {visible: #(!mobile())}
                    mobile_input := MobileRoomInputBar {visible: #(mobile())}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
struct App {
    #[live] ui: WidgetRef,
    #[rust] generation: usize,
    #[rust] mobile: bool,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.mobile = mobile();
        let body = (0..16).map(|index| format!("Reply line {index:02}")).collect::<Vec<_>>().join("\n");
        self.ui.html_or_plaintext(cx, ids!(reply.preview_content.reply_preview_body))
            .show_plaintext(cx, &body);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let expanded = if self.ui.button(cx, ids!(reply.reply_expand_button)).clicked(actions) {
            Some(true)
        } else if self.ui.button(cx, ids!(reply.reply_collapse_button)).clicked(actions) {
            Some(false)
        } else {
            None
        };
        if let Some(expanded) = expanded {
            let preview = self.ui.collapsible_preview(cx, ids!(reply));
            preview.set_expanded(expanded);
            preview.redraw(cx);
        }
    }

    fn handle_draw(&mut self, cx: &mut Cx, event: &DrawEvent) {
        let mut draw = CxDraw::new(cx, event);
        let mut cx = Cx2d::new(&mut draw);
        while let Some(widget) = self.ui.draw(&mut cx, &mut Scope::empty()).step() {
            let Some(mut list) = widget.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(&mut cx, 0, 80);
            while let Some(index) = list.next_visible_item(&mut cx) {
                let room_id = format!("!room{index}:example.org").try_into().unwrap();
                let room = JoinedRoomInfo {
                    user_power_levels: None,
                    notification_mode: None,
                    room_name_id: RoomNameId::new(
                        RoomDisplayName::Named(format!("Room {index:02} version {}", self.generation)),
                        room_id,
                    ),
                    num_unread_messages: (index % 4) as u64,
                    num_unread_mentions: 0,
                    is_marked_unread: false,
                    canonical_alias: None,
                    alt_aliases: Vec::new(),
                    tags: Default::default(),
                    latest: Some(LatestEventPreview {
                        text: format!("Latest message {index:02} version {}", self.generation),
                        direct_text: None,
                        sender: None,
                        timestamp: ruma::MilliSecondsSinceUnixEpoch::now(),
                    }),
                    room_avatar: FetchedRoomAvatar::Text("R".into()),
                    has_been_shown: true,
                    is_selected: index == 1,
                    is_direct: false,
                    is_tombstoned: index == 2,
                };
                let template = if self.mobile {
                    id!(MobileEntry)
                } else {
                    id!(DesktopEntry)
                };
                list.item(&mut cx, index, template)
                    .draw_all(&mut cx, &mut Scope::with_props(&room));
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        theme::init_standalone(vm);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        rinx::app::register_widgets(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::Custom(command) = event {
            match command.as_str() {
                "fixture:update" => {
                    self.generation += 1;
                    self.ui.redraw(cx);
                }
                "fixture:dark" => {
                    let mut selection = theme::selection(cx).unwrap_or_default();
                    selection.appearance = Appearance::Dark;
                    theme::select(cx, Selection { ..selection }).unwrap();
                }
                _ => {}
            }
        }
        self.match_event(cx, event);
        if !matches!(event, Event::Draw(_)) {
            self.ui.handle_event(cx, event, &mut Scope::empty());
        }
        theme::packages::after_event(cx, event);
    }
}
