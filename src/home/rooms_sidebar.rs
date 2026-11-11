//! The RoomsSideBar is the widget that contains the RoomsList and other items.
//!
//! It differs in what content it includes based on the adaptive view:
//! * On a narrow mobile view, it acts as the root_view of StackNavigation
//!   * It includes a title label, a search bar, and the RoomsList.
//! * On a wide desktop view, it acts as a permanent tab that is on the left side of the dock.
//!   * It includes a title label, a search row and the RoomsList. Spaces are listed
//!     in the navigation rail rather than behind an "All Chats | Spaces" switch.
//!   * The search row is the filter field with a "+" on its right, as in WeChat.
//!     The "+" opens the "New or Join" menu (New workspace, New room or space,
//!     Join or explore rooms; see `sidebar_action_menu`) just under it.
//!
//! Neither layout repeats a creation or tool shortcut as a row above the list:
//! File Transfer has its own rail button on desktop and a Discover row on mobile.
//! Mobile keeps its own title bar, whose "+" starts a chat from Contacts, and
//! reaches Explore Rooms from Discover.

use makepad_widgets::*;

use crate::home::rooms_list::RoomsListWidgetExt;
use crate::home::joined_spaces::JoinedSpacesWidgetExt;
use crate::home::navigation_tab_bar::{NavigationBarAction, SelectedTab};
use crate::home::sidebar_action_menu::{new_or_join_menu_pos, SidebarMenuAction, SidebarMenuKind};
use crate::shared::context_menu::ContextMenuClosed;
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt;
use crate::shared::room_filter_input_bar::{MainFilterAction, RoomFilterInputBarWidgetExt};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // "New or Join" beside the desktop search field, as in WeChat: a square as
    // tall as the field, with the field's fill, border and corners, so the two
    // read as one row. It darkens a little on hover, more while pressed, and
    // stays pressed while its menu is open.
    mod.widgets.NewOrJoinButton = NavigationBarButton {
        width: (mod.widgets.ROOM_FILTER_HEIGHT) height: (mod.widgets.ROOM_FILTER_HEIGHT)
        padding: 0 margin: 0
        align: Align{x: 0.5 y: 0.5}
        tooltip_below: true
        tooltip_text: #(crate::i18n::tr("New or Join"))
        draw_bg +: {
            down: instance(0.0)
            ink: instance(mod.widgets.RINX_INK)
            color: instance((RBX_BG_SURFACE_SUBTLE))
            border_color: (RBX_STROKE_SOFT)
            border_size: 1.0
            border_radius: (RBX_RADIUS_XS)
            // Tints toward the text color, as the theme's own hover and
            // pressed roles do, so both appearances get a visible change.
            get_color: fn() -> vec4 {
                let hover = self.color.mix(self.ink, 0.06)
                let down = self.color.mix(self.ink, 0.12)
                return self.color.mix(hover, max(self.hover, self.focus)).mix(down, max(self.down, self.active))
            }
        }
        icon := Icon {
            icon_walk: Walk{width: 18 height: 18}
            draw_icon +: {svg: ICON_PLUS color: mod.widgets.RINX_INK}
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.15}}
                    apply: {draw_bg: {hover: 0.0 down: 0.0}}
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {draw_bg: {hover: 1.0 down: 0.0}}
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {draw_bg: {hover: 1.0 down: 1.0}}
                }
            }
        }
    }

    let ChatSwitchButton = NavigationBarButton {
        width: Fill height: 32
        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SURFACE border_radius: 5}
        label := Label {
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (11.5 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }
    let ChatViewSwitch = SolidView {
        width: Fill height: 46 flow: Right
        padding: Inset{left: 12 right: 12 bottom: 10 top: 2}
        draw_bg.color: MOBILE_BG
        RoundedView {
            width: Fill height: Fill flow: Right padding: 2
            draw_bg +: {color: mod.widgets.RINX_FIELD border_radius: 6}
            all_chats := ChatSwitchButton {label.text: #(crate::i18n::tr("All Chats")) label.i18n_text: "All Chats"}
            joined_spaces_tab := ChatSwitchButton {label.text: #(crate::i18n::tr("Spaces")) label.i18n_text: "Spaces"}
        }
    }

    mod.widgets.RoomsSideBar = #(RoomsSideBar::register_widget(vm)) {
        Desktop := SolidView {
            padding: Inset{top: 20, left: 10, right: 10}
            flow: Down, spacing: 5
            width: Fill, height: Fill

            draw_bg.color: (RBX_BG_CANVAS)

            CachedWidget {
                rooms_list_header := RoomsListHeader {}
            }
            // The filter field shrinks to leave room for "New or Join" on its right.
            // Its placeholder is the short "Search", as on mobile, so it is not
            // cut off in a narrow column or with large text.
            search_row := View {
                width: Fill height: Fit
                flow: Right spacing: 8
                align: Align{y: 0.5}
                CachedWidget {
                    room_filter_input_bar := RoomFilterInputBar {
                        input +: {empty_text: #(crate::i18n::tr("Search")) i18n_empty_text: "Search"}
                    }
                }
                new_or_join_button := mod.widgets.NewOrJoinButton {}
            }
            room_filter_bubbles := RoomFilterBubbles {}
            // No "All Chats | Spaces" switch here: on desktop, spaces live in the
            // navigation rail on the left, and selecting one filters this list.
            conversations := View {
                width: Fill height: Fill
                CachedWidget {rooms_list := RoomsList {}}
            }
        },

        Mobile := SolidView {
            width: Fill height: Fill flow: Down
            draw_bg.color: mod.widgets.RINX_SURFACE
            title_bar := MobileTitle {
                title.text: #(crate::i18n::tr("Chats")) title.i18n_text: "Chats"
                controls.right.visible: true
            }
            search_area := SolidView {
                width: Fill height: 48
                padding: Inset{left: 10 right: 10 bottom: 10}
                draw_bg.color: MOBILE_BG
                room_filter_input_bar := RoomFilterInputBar {
                    height: 36
                    draw_bg +: {color: mod.widgets.RINX_SURFACE border_size: 0 border_radius: 5}
                    input +: {
                        empty_text: #(crate::i18n::tr("Search")) i18n_empty_text: "Search"
                        draw_text.text_style.font_size: (11.5 * mod.widgets.RINX_TEXT_SCALE)
                        draw_bg +: {color: mod.widgets.RINX_SURFACE color_hover: mod.widgets.RINX_SURFACE color_focus: mod.widgets.RINX_SURFACE color_empty: mod.widgets.RINX_SURFACE}
                    }
                }
            }
            room_filter_bubbles := RoomFilterBubbles {margin: Inset{left: 10, right: 10}}
            ChatViewSwitch {}
            conversations := View {
                width: Fill height: Fill
                CachedWidget {rooms_list := RoomsList {}}
            }
            spaces_tree := View {
                width: Fill height: Fill visible: false
                CachedWidget {joined_spaces := JoinedSpaces {}}
            }
        }
    }
}

/// Shared across adaptive sidebar replacements, but reset when signing out.
#[derive(Default)]
struct ChatsViewState { spaces: bool }

/// A simple wrapper around `AdaptiveView` that contains several global singleton widgets.
///
/// * In the mobile view, it serves as the root view of the StackNavigation,
///   showing the title label, the search bar, and the RoomsList.
/// * In the desktop view, it is a permanent tab in the dock,
///   showing the title label, the search row (filter field and "New or Join")
///   and the RoomsList.
#[derive(Script, Widget)]
pub struct RoomsSideBar {
    #[deref] view: AdaptiveView,

    #[rust] mobile_search_collapsed: bool,
    #[rust] pending_search_focus: bool,
    /// The "New or Join" menu opened from this sidebar's "+" is showing.
    #[rust] new_or_join_open: bool,
}

impl ScriptHook for RoomsSideBar {
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        vm.with_cx_mut(|cx| {
            // Here we set the global singleton for the RoomsList widget,
            // which is used to access the list of rooms from anywhere in the app.
            cx.set_global(self.view.rooms_list(cx, ids!(rooms_list)));

            // Follow the enclosing HomeScreen, including its view-mode override.
            // The desktop dock pane is narrow even in a wide window; selecting
            // by this pane's width would incorrectly put mobile UI inside it.
            self.view.set_variant_selector(|cx, _| {
                if crate::home::home_screen::effective_is_desktop(cx) {
                    live_id!(Desktop)
                } else {
                    live_id!(Mobile)
                }
            });
        });
    }
}

impl RoomsSideBar {
    fn active_mobile_list(&self, cx: &mut Cx) -> PortalListRef {
        if cx.global::<ChatsViewState>().spaces {
            self.view.portal_list(cx, ids!(joined_spaces.list))
        } else {
            self.view.portal_list(cx, ids!(rooms_list.list))
        }
    }

    fn update_mobile_search(&mut self, cx: &mut Cx) {
        if crate::home::home_screen::effective_is_desktop(cx) {
            self.pending_search_focus = false;
            return;
        }
        let list = self.active_mobile_list(cx);
        let collapsed = list.first_id() != 0 || list.scroll_position() < -0.5;
        self.view.view(cx, ids!(search_area)).set_visible(cx, !collapsed);
        self.view.button(cx, ids!(title_bar.controls.search)).set_visible(cx, collapsed);
        if collapsed != self.mobile_search_collapsed {
            self.mobile_search_collapsed = collapsed;
            if collapsed {
                let input = self.view.text_input(cx, ids!(room_filter_input_bar.input));
                if cx.has_key_focus(input.area()) {
                    cx.set_key_focus(Area::Empty);
                    cx.hide_text_ime();
                }
            }
            // Redraw the enclosing layout only when the header changes. The
            // list keeps its per-scroll actions disabled on the hot path.
            self.view.redraw(cx);
        }
    }

    fn open_mobile_search(&mut self, cx: &mut Cx) {
        if crate::home::home_screen::effective_is_desktop(cx) { return; }
        self.pending_search_focus = true;
        // This API cancels a running fling before returning to the beginning.
        self.active_mobile_list(cx).smooth_scroll_to(cx, 0, 1000.0, Some(1), 0.0);
        self.view.redraw(cx);
    }
}

impl Widget for RoomsSideBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // If the main room filter input bar changed keywords, re-emit that action
        // as a MainFilterAction so that other widgets can handle it.
        if let Event::Actions(actions) = event {
            if self.view.navigation_bar_button(cx, ids!(all_chats)).clicked(actions) {
                cx.global::<ChatsViewState>().spaces = false;
                cx.action(NavigationBarAction::GoToHome);
                self.view.redraw(cx);
            }
            if self.view.navigation_bar_button(cx, ids!(joined_spaces_tab)).clicked(actions) {
                cx.global::<ChatsViewState>().spaces = true;
                cx.action(NavigationBarAction::GoToHome);
                self.view.redraw(cx);
            }
            if self.view.button(cx, ids!(title_bar.controls.right)).clicked(actions) {
                cx.action(crate::home::navigation_tab_bar::NavigationBarAction::GoToTab(
                    crate::home::navigation_tab_bar::SelectedTab::Contacts,
                ));
            }
            if self.view.button(cx, ids!(title_bar.controls.search)).clicked(actions) {
                self.open_mobile_search(cx);
            }
            // "New or Join" opens its menu just under the "+". Only the menu's rows
            // run commands, so the click itself changes no page or selection.
            let new_or_join = self.view.navigation_bar_button(cx, ids!(new_or_join_button));
            if new_or_join.clicked(actions) {
                let pos = new_or_join_menu_pos(cx, new_or_join.area().rect(cx));
                new_or_join.set_selected(cx, true);
                self.new_or_join_open = true;
                cx.action(SidebarMenuAction::Open { kind: SidebarMenuKind::Add, pos });
            } else if self.new_or_join_open
                && actions.iter().any(|action| action.downcast_ref::<ContextMenuClosed>().is_some())
            {
                self.new_or_join_open = false;
                new_or_join.set_selected(cx, false);
            }
            if let Some(keywords) = self.view.room_filter_input_bar(cx, ids!(room_filter_input_bar)).changed(actions) {
                cx.action(MainFilterAction::Changed(keywords));
            }

            for action in actions {
                if let Some(crate::logout::logout_confirm_modal::LogoutAction::ClearAppState {..}) = action.downcast_ref() {
                    cx.global::<ChatsViewState>().spaces = false;
                    self.pending_search_focus = false;
                }
                // A legacy desktop Space shortcut still opens its filtered list.
                if let Some(NavigationBarAction::TabSelected(SelectedTab::Space {..})) = action.downcast_ref() {
                    cx.global::<ChatsViewState>().spaces = false;
                }
            }
        }
        self.view.handle_event(cx, event, scope);
        self.update_mobile_search(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let collapsed=crate::home::home_screen::effective_is_desktop(cx)
            && cx.peek_walk_turtle(walk).size.x <= 0.5;
        self.view.view(cx,ids!(Desktop)).set_visible(cx,!collapsed);
        if collapsed {return DrawStep::done();}
        // The desktop layout has no spaces tree (the rail lists spaces instead).
        let spaces = cx.global::<ChatsViewState>().spaces
            && !crate::home::home_screen::effective_is_desktop(cx);
        self.view.widget(cx, ids!(room_filter_bubbles)).set_visible(cx, !spaces);
        self.view.view(cx, ids!(conversations)).set_visible(cx, !spaces);
        self.view.view(cx, ids!(spaces_tree)).set_visible(cx, spaces);
        self.view.navigation_bar_button(cx, ids!(all_chats)).set_selected(cx, !spaces);
        self.view.navigation_bar_button(cx, ids!(joined_spaces_tab)).set_selected(cx, spaces);
        self.view.joined_spaces(cx, ids!(joined_spaces)).set_active(cx, spaces);
        // The tooltip follows the interface language, including after a switch.
        if let Some(mut button) = self.view.navigation_bar_button(cx, ids!(new_or_join_button)).borrow_mut() {
            button.set_tooltip_text(crate::i18n::tr("New or Join"));
        }
        self.update_mobile_search(cx);
        let search_drawn = !self.mobile_search_collapsed;
        let step = self.view.draw_walk(cx, scope, walk);
        if step.is_done() {
            // Drawing may normalize/clamp the list after filtering or loading.
            self.update_mobile_search(cx);
            if self.pending_search_focus && search_drawn && !self.mobile_search_collapsed {
                self.pending_search_focus = false;
                self.view.text_input(cx, ids!(room_filter_input_bar.input)).take_key_focus(cx);
            }
        }
        step
    }
}

/// The rooms list column, built and drawn offscreen in both layouts.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::sidebar_action_menu::SidebarActionMenu;
    use crate::i18n::{text_for, Language};
    use crate::shared::navigation_bar_button::NavigationBarButtonAction;
    use crate::theme::{Appearance, Selection};
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    /// A desktop window. The chat list column starts right of the 76-point rail.
    const WINDOW: DVec2 = dvec2(1280.0, 760.0);
    const COLUMN_X: f64 = 76.0;
    /// The column's width when the dock opens (its splitter starts at 300).
    const COLUMN_WIDTH: f64 = 300.0;
    /// Light, dark and large text, as the appearance settings offer them.
    const LOOKS: [(Appearance, f64); 4] = [
        (Appearance::Light, 1.0), (Appearance::Dark, 1.0), (Appearance::Light, 1.5), (Appearance::Dark, 2.0),
    ];
    const NEW_OR_JOIN_ROWS: [LiveId; 3] = [id!(add_room_or_space), id!(add_article_editor), id!(add_join_rooms)];

    fn texts(widget: &WidgetRef, found: &mut Vec<String>) {
        found.push(widget.text());
        widget.children(&mut |_, child| texts(&child, found));
    }

    /// The sidebar and the app's menu overlay, registered under one appearance
    /// and text scale (1.0 is the default).
    fn column(appearance: Appearance, text_scale: f64) -> (Cx, RoomsSideBar, SidebarActionMenu) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (sidebar, menu) = cx.with_vm(|vm| {
            crate::theme::tests::install(vm, Selection { appearance, ..Default::default() });
            // The same theme value a large-text appearance package sets.
            let theme = vm.module(id!(theme));
            vm.bx.heap.set_value(theme, LiveId::from_str("octo_typography_scale").into(), text_scale.into(), NoTrap);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.RoomsSideBar {}});
            let sidebar = RoomsSideBar::script_from_value(vm, value);
            let value = script_eval!(vm, {mod.widgets.SidebarActionMenu {}});
            let menu = SidebarActionMenu::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Rooms sidebar registration: {errors:?}");
            (sidebar, menu)
        });
        (cx, sidebar, menu)
    }

    /// One instance the frame submitted to the GPU: its rect and named inputs.
    struct Drawn {
        rect: Rect,
        inputs: Vec<(LiveId, Vec<f32>)>,
    }
    impl Drawn {
        fn input(&self, id: LiveId) -> Option<&[f32]> {
            self.inputs.iter().find(|(name, _)| *name == id).map(|(_, value)| value.as_slice())
        }
    }

    /// Draws the column `width` wide in `layout`, right of the rail, then the
    /// menu overlay over the whole window as the app does. Returns every
    /// instance the frame submitted.
    fn draw(cx: &mut Cx, sidebar: &mut RoomsSideBar, menu: &mut SidebarActionMenu, layout: LiveId, width: f64) -> Vec<Drawn> {
        sidebar.view.set_variant_selector(move |_, _| layout);
        cx.redraw_all();
        let pass = DrawPass::new(cx);
        pass.set_size(cx, WINDOW);
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent::default();
        let list_id;
        {
            let mut draw = CxDraw::new(cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            list_id = cx2d.get_current_draw_list_id().unwrap();
            cx2d.begin_root_turtle(WINDOW, Layout::flow_right());
            // The rail's place, left of the column.
            cx2d.walk_turtle(Walk::fixed(COLUMN_X, WINDOW.y));
            // The rooms list reads the selected room from the app state.
            let mut state = crate::app::AppState::default();
            sidebar.draw_walk_all(&mut cx2d, &mut Scope::with_data(&mut state), Walk::fixed(width, WINDOW.y));
            menu.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk::fixed(WINDOW.x, WINDOW.y).with_abs_pos(DVec2::default()));
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        let list = &cx.draw_lists[list_id];
        let mut drawn = Vec::new();
        for index in 0..list.draw_items.len() {
            let item = &list.draw_items[index];
            let (Some(call), Some(instances)) = (item.draw_call(), item.instances.as_ref()) else {
                continue;
            };
            let mapping = &cx.draw_shaders.shaders[call.draw_shader_id.index].mapping;
            let (Some(pos), Some(size)) = (mapping.rect_pos, mapping.rect_size) else {
                continue;
            };
            for instance in instances.chunks_exact(mapping.instances.total_slots) {
                drawn.push(Drawn {
                    rect: Rect {
                        pos: dvec2(instance[pos] as f64, instance[pos + 1] as f64),
                        size: dvec2(instance[size] as f64, instance[size + 1] as f64),
                    },
                    inputs: mapping.instances.inputs.iter()
                        .map(|input| (input.id, instance[input.offset..input.offset + input.slots].to_vec()))
                        .collect(),
                });
            }
        }
        drawn
    }

    fn rect(cx: &mut Cx, sidebar: &RoomsSideBar, path: &[LiveId]) -> Rect {
        sidebar.view.widget(cx, path).area().rect(cx)
    }

    fn same(a: Rect, b: Rect) -> bool {
        (a.pos - b.pos).length() < 0.5 && (a.size - b.size).length() < 0.5
    }

    fn color(value: Option<&[f32]>) -> Vec4f {
        let value = value.expect("the shader has this color");
        vec4(value[0], value[1], value[2], value[3])
    }

    fn close(a: Vec4f, b: Vec4f) -> bool {
        (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01 && (a.z - b.z).abs() < 0.01 && (a.w - b.w).abs() < 0.01
    }

    /// Delivers `actions` to the sidebar and returns what it emitted.
    fn deliver(cx: &mut Cx, sidebar: &mut RoomsSideBar, actions: ActionsBuf) -> ActionsBuf {
        let mut state = crate::app::AppState::default();
        cx.capture_actions(|cx| sidebar.handle_event(cx, &Event::Actions(actions), &mut Scope::with_data(&mut state)))
    }

    fn click_new_or_join(cx: &mut Cx, sidebar: &mut RoomsSideBar) -> ActionsBuf {
        let uid = sidebar.view.navigation_bar_button(cx, ids!(new_or_join_button)).widget_uid();
        let clicked = cx.capture_actions(|cx| cx.widget_action(uid, NavigationBarButtonAction::Clicked));
        deliver(cx, sidebar, clicked)
    }

    fn pressed(cx: &mut Cx, sidebar: &RoomsSideBar) -> bool {
        let button = sidebar.view.navigation_bar_button(cx, ids!(new_or_join_button));
        let inner = button.borrow().expect("New or Join is drawn");
        inner.animator_in_state(cx, ids!(active.on))
    }

    #[test]
    fn rooms_list_has_no_new_workspace_button_or_file_transfer_row() {
        for layout in [id!(Desktop), id!(Mobile)] {
            let (mut cx, mut sidebar, mut menu) = column(Appearance::Light, 1.0);
            draw(&mut cx, &mut sidebar, &mut menu, layout, 320.0);
            assert_eq!(sidebar.view.active_variant(), Some(layout));
            for removed in [ids!(new_workspace), ids!(file_transfer_entry)] {
                assert!(sidebar.view.widget(&mut cx, removed).is_empty(), "{layout:?}: {removed:?} is gone");
            }
            let mut found = Vec::new();
            sidebar.children(&mut |_, child| texts(&child, &mut found));
            assert!(found.iter().any(|text| !text.is_empty()), "{layout:?}: the walk reaches the column's labels");
            for label in ["New workspace", "File Transfer"] {
                assert!(!found.iter().any(|text| text == crate::i18n::tr(label)), "{layout:?}: no {label} entry above the list: {found:?}");
            }
            // Only the bounded filters and existing mobile tab switch separate search and conversations.
            let filters = rect(&mut cx, &sidebar, ids!(room_filter_bubbles));
            let conversations = rect(&mut cx, &sidebar, ids!(conversations));
            let search = rect(&mut cx, &sidebar, ids!(room_filter_input_bar));
            assert!(conversations.size.y > 400.0, "{layout:?}: the list keeps the column: {conversations:?}");
            assert!((filters.size.y - 32.0).abs() < 0.5 && filters.pos.y >= search.pos.y + search.size.y - 0.5,
                "{layout:?}: the filter row stays bounded below search: {filters:?}");
            assert!(conversations.pos.y >= filters.pos.y + filters.size.y - 0.5 && conversations.pos.y - (filters.pos.y + filters.size.y) < 60.0,
                "{layout:?}: only the mobile tabs separate filters and list: {filters:?} {conversations:?}");
        }
    }

    #[test]
    fn new_or_join_sits_beside_the_filter_field_as_one_row() {
        for (appearance, text_scale) in LOOKS {
            // The column as the dock opens it, and dragged narrow.
            for width in [COLUMN_WIDTH, 220.0, 160.0] {
                let case = format!("{appearance:?} at {text_scale}x, {width} wide");
                let (mut cx, mut sidebar, mut menu) = column(appearance, text_scale);
                let drawn = draw(&mut cx, &mut sidebar, &mut menu, id!(Desktop), width);
                let theme = crate::theme::snapshot(&mut cx);
                let header = rect(&mut cx, &sidebar, ids!(rooms_list_header));
                let field = rect(&mut cx, &sidebar, ids!(room_filter_input_bar));
                let input = rect(&mut cx, &sidebar, ids!(room_filter_input_bar.input));
                let plus = rect(&mut cx, &sidebar, ids!(new_or_join_button));
                let glyph = rect(&mut cx, &sidebar, ids!(new_or_join_button.icon));
                let list = rect(&mut cx, &sidebar, ids!(conversations));

                // One row under the title: the field first, then a square "+" as
                // tall as the field, 8 points apart and centred on the same line.
                assert!(plus.size.y >= 35.0 && (plus.size.y - field.size.y).abs() < 0.5 && (plus.size.x - plus.size.y).abs() < 0.5,
                    "{case}: a square as tall as the field: {plus:?} {field:?}");
                assert!(((plus.pos.y + plus.size.y / 2.0) - (field.pos.y + field.size.y / 2.0)).abs() < 0.5, "{case}: same line: {plus:?} {field:?}");
                assert!((plus.pos.x - (field.pos.x + field.size.x) - 8.0).abs() < 0.5, "{case}: 8 points right of the field: {plus:?} {field:?}");
                assert!(field.pos.y >= header.pos.y + header.size.y && list.pos.y >= plus.pos.y + plus.size.y,
                    "{case}: between the title and the list: {header:?} {plus:?} {list:?}");
                // The row spans the column inside its 10-point margins: the field
                // gives up width, never the button.
                assert!((field.pos.x - (COLUMN_X + 10.0)).abs() < 0.5, "{case}: field starts at the margin: {field:?}");
                assert!((plus.pos.x + plus.size.x - (COLUMN_X + width - 10.0)).abs() < 0.5, "{case}: the + ends at the margin: {plus:?}");
                assert!(field.size.x >= 60.0, "{case}: the field stays usable: {field:?}");
                // The typed text stays inside the field, even with large text.
                assert!(input.pos.y >= field.pos.y - 0.5 && input.pos.y + input.size.y <= field.pos.y + field.size.y + 0.5,
                    "{case}: the text fits the field: {input:?} in {field:?}");
                assert!(((glyph.pos + glyph.size / 2.0) - (plus.pos + plus.size / 2.0)).length() < 0.75, "{case}: the + glyph is centred: {glyph:?} in {plus:?}");

                // Drawn with the field's own fill and border, in the theme's colors.
                let field_bg = drawn.iter().find(|d| same(d.rect, field) && d.input(id!(border_color)).is_some()).expect("the field draws its background");
                let plus_bg = drawn.iter().find(|d| same(d.rect, plus) && d.input(id!(down)).is_some()).expect("the + draws its background");
                assert!(close(color(plus_bg.input(id!(color))), color(field_bg.input(id!(color)))) && close(color(plus_bg.input(id!(color))), theme.field),
                    "{case}: the field's fill: {:?} {:?}", plus_bg.input(id!(color)), field_bg.input(id!(color)));
                assert!(close(color(plus_bg.input(id!(border_color))), color(field_bg.input(id!(border_color)))) && close(color(plus_bg.input(id!(border_color))), theme.border),
                    "{case}: the field's border");
                assert!(close(color(plus_bg.input(id!(ink))), theme.ink), "{case}: hover and press tint toward the text color");
                for state in [id!(hover), id!(down), id!(active)] {
                    assert_eq!(plus_bg.input(state), Some(&[0.0f32][..]), "{case}: at rest, not {state:?}");
                }
                // The SVG itself (it has an opacity; the Icon's own backdrop has none).
                let svg = drawn.iter()
                    .find(|d| d.input(id!(opacity)).is_some() && d.rect.pos.x >= plus.pos.x && d.rect.pos.y >= plus.pos.y
                        && d.rect.pos.x + d.rect.size.x <= plus.pos.x + plus.size.x && d.rect.pos.y + d.rect.size.y <= plus.pos.y + plus.size.y)
                    .expect("the + glyph is drawn inside the button");
                assert!(close(color(svg.input(id!(color))), theme.ink), "{case}: the glyph is in the text color: {:?}", svg.input(id!(color)));

                // The tooltip names it in the interface language (English in unit tests),
                // and the field keeps a short placeholder that fits beside it.
                let button = sidebar.view.navigation_bar_button(&mut cx, ids!(new_or_join_button));
                assert_eq!(button.borrow().expect("New or Join is drawn").tooltip_text(), "New or Join", "{case}");
                assert_eq!(sidebar.view.text_input(&mut cx, ids!(room_filter_input_bar.input)).empty_text(), "Search", "{case}");
            }
        }
        assert_eq!(text_for(Language::Chinese, "New or Join"), "新建或加入");
        assert_eq!(text_for(Language::Chinese, "Search"), "搜索");
    }

    #[test]
    fn new_or_join_opens_its_menu_under_the_button_inside_the_window() {
        for (appearance, text_scale) in LOOKS {
            for width in [COLUMN_WIDTH, 160.0] {
                let case = format!("{appearance:?} at {text_scale}x, {width} wide");
                let (mut cx, mut sidebar, mut menu) = column(appearance, text_scale);
                draw(&mut cx, &mut sidebar, &mut menu, id!(Desktop), width);
                let plus = rect(&mut cx, &sidebar, ids!(new_or_join_button));
                assert!(!pressed(&mut cx, &sidebar), "{case}: at rest");

                // A click asks the app for the New or Join menu and nothing else.
                let emitted = click_new_or_join(&mut cx, &mut sidebar);
                let opens: Vec<_> = emitted.iter().filter_map(|action| match action.downcast_ref() {
                    Some(SidebarMenuAction::Open {kind, pos}) => Some((*kind, *pos)),
                    _ => None,
                }).collect();
                assert_eq!(opens, vec![(SidebarMenuKind::Add, new_or_join_menu_pos(&mut cx, plus))], "{case}");
                assert!(!emitted.iter().any(|action| action.downcast_ref::<NavigationBarAction>().is_some()), "{case}: the rows, not the +, navigate");
                assert!(pressed(&mut cx, &sidebar), "{case}: the + stays pressed while its menu is open");

                // The app shows it as it shows every sidebar menu.
                let (_, pos) = opens[0];
                menu.show(&mut cx, SidebarMenuKind::Add, pos, Rect {pos: DVec2::default(), size: WINDOW});
                let drawn = draw(&mut cx, &mut sidebar, &mut menu, id!(Desktop), width);
                let card = menu.widget(&cx, ids!(main_content)).area().rect(&cx);
                assert!(card.pos.x >= 0.0 && card.pos.y >= 0.0 && card.pos.x + card.size.x <= WINDOW.x + 0.5 && card.pos.y + card.size.y <= WINDOW.y + 0.5,
                    "{case}: the card stays in the window: {card:?}");
                // It drops down from the button: just under it, right edges aligned,
                // unless the card is wider than the room left of the button.
                assert!((card.pos.y - (plus.pos.y + plus.size.y + 4.0)).abs() < 1.0, "{case}: just under the +: {card:?} {plus:?}");
                if card.size.x <= plus.pos.x + plus.size.x {
                    assert!((card.pos.x + card.size.x - (plus.pos.x + plus.size.x)).abs() < 1.0, "{case}: right edges aligned: {card:?} {plus:?}");
                } else {
                    assert!(card.pos.x.abs() < 0.5, "{case}: as far left as the window allows: {card:?}");
                }
                for row in NEW_OR_JOIN_ROWS {
                    let row = menu.widget(&cx, &[row]).area().rect(&cx);
                    assert!(row.size.y >= 44.0 && row.pos.y >= card.pos.y && row.pos.y + row.size.y <= card.pos.y + card.size.y + 0.5, "{case}: {row:?} in {card:?}");
                }
                // While open, the + shows pressed.
                let plus_bg = drawn.iter().find(|d| same(d.rect, plus) && d.input(id!(down)).is_some()).expect("the + draws its background");
                assert_eq!(plus_bg.input(id!(active)), Some(&[1.0f32][..]), "{case}: drawn pressed while open");

                // Escape closes the menu; the + is released and nothing ran.
                let escape = Event::KeyDown(KeyEvent {key_code: KeyCode::Escape, ..Default::default()});
                let closed = cx.capture_actions(|cx| menu.handle_event(cx, &escape, &mut Scope::empty()));
                assert!(!menu.visible, "{case}: Escape closes the menu");
                let emitted = deliver(&mut cx, &mut sidebar, closed);
                assert!(!pressed(&mut cx, &sidebar), "{case}: released when the menu closes");
                assert!(!emitted.iter().any(|action| action.downcast_ref::<SidebarMenuAction>().is_some() || action.downcast_ref::<NavigationBarAction>().is_some()), "{case}");
            }
        }
    }

    #[test]
    fn mobile_keeps_its_title_bar_plus_and_no_search_row_button() {
        let (mut cx, mut sidebar, mut menu) = column(Appearance::Light, 1.0);
        draw(&mut cx, &mut sidebar, &mut menu, id!(Mobile), 375.0);
        assert_eq!(sidebar.view.active_variant(), Some(id!(Mobile)));
        // Mobile's search is its own full-width field, without the desktop "+".
        assert!(sidebar.view.widget(&mut cx, ids!(new_or_join_button)).is_empty());
        let search = rect(&mut cx, &sidebar, ids!(room_filter_input_bar));
        assert!(search.size.x > 375.0 - 30.0, "the field keeps the width: {search:?}");
        // Its title bar "+" still starts a chat from Contacts.
        let uid = sidebar.view.button(&mut cx, ids!(title_bar.controls.right)).widget_uid();
        let clicked = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
        let emitted = deliver(&mut cx, &mut sidebar, clicked);
        assert!(emitted.iter().any(|action| matches!(action.downcast_ref(), Some(NavigationBarAction::GoToTab(SelectedTab::Contacts)))));
        assert!(!emitted.iter().any(|action| action.downcast_ref::<SidebarMenuAction>().is_some()));
    }
}
