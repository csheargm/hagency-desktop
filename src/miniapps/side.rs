//! The docked mini-apps region beside the chat, like WeChat desktop's side
//! window.
//!
//! On a desktop, opening a mini app (from the left bar, a chat link, the
//! assistant or Hagency's management) puts it in a tab here rather than in a
//! separate window, so the chat stays visible and usable. Each tab is one
//! opened item: the mini-apps catalog, one app, or Hagency's management pages.
//! Opening an item again focuses its tab; closing a tab stops its app and
//! revokes its lease; closing the last tab hides the region.
//!
//! Layout, chosen each frame by the HomeScreen:
//! * Docked: beside the chat, as wide as the person dragged it (kept in the app
//!   state), while the chat keeps at least [`CHAT_MIN`].
//! * Overlay: when the window is too narrow for both, and in the mobile
//!   layout, the region covers the chat, with Back to return to it. A resize
//!   alone never covers the chat: the region waits until something opens.
//!
//! The pop-out button moves every tab, live and in order, into the shared
//! workspace window (`window.rs`). Dock back returns that entire workspace.
//!
//! One instance serves both layouts through `CachedWidget`, so tabs survive
//! a switch between them, and every app keeps receiving its actions.
use makepad_widgets::*;
use makepad_widgets::splitter::DrawSplitter;
use super::surface::{MiniAppsSurfaceDeckRef, MiniAppsSurfaceDeckWidgetExt, MiniAppsSurfaceRef, MiniAppsSurfaceWidgetRefExt, SurfaceKey};
use super::ui::MiniAppsAction;
use crate::hagency::ui::HagencyAction;
use crate::shared::web_browser::{WebBrowserAction, WebBrowserRef};
use crate::article_app::ArticleAction;
use crate::shared::navigation_bar_button::{NavigationBarButtonWidgetExt, NavigationBarButtonWidgetRefExt};

/// The narrowest docked region: about the width at which the release probes
/// review the Hagency app's narrow layout.
pub const SIDE_MIN: f64 = 420.0;
/// The docked region's width until the person drags it.
pub const SIDE_DEFAULT: f64 = 440.0;
/// The chat keeps at least this much beside a docked region. Narrower, the
/// region shows over the chat instead of squeezing it.
pub const CHAT_MIN: f64 = 400.0;
/// The draggable strip between the chat and the region.
pub const HANDLE: f64 = crate::shared::pane_splitter::WIDTH;
/// Where the rooms list ends, when the dock cannot say.
pub const ROOMS_DEFAULT: f64 = 300.0;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // A header control: a full control-height square with a tooltip below.
    let SideIconButton = NavigationBarButton {
        width: (mod.widgets.RINX_CONTROL_HEIGHT) height: (mod.widgets.RINX_CONTROL_HEIGHT)
        align: Align{x: 0.5 y: 0.5} tooltip_below: true
        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_radius: 6}
        icon := Icon {icon_walk: Walk{width: 16 height: 16} draw_icon +: {color: mod.widgets.RINX_INK}}
    }

    // One tab: the opened item's title, and its close button.
    mod.widgets.MiniAppsSideTab = View {
        width: Fill{max: 220} height: Fit flow: Right align: Align{y: 0.5}
        select := NavigationBarButton {
            width: Fill height: (mod.widgets.RINX_CONTROL_HEIGHT) flow: Right
            padding: Inset{left: 10 right: 2} align: Align{x: 0 y: 0.5}
            draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_radius: 6}
            title := Label {
                width: Fill max_lines: 1 text_overflow: Ellipsis
                draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
            }
        }
        close := SideIconButton {
            tooltip_text: #(crate::i18n::tr("Close"))
            icon +: {icon_walk: Walk{width: 10 height: 10} draw_icon +: {svg: ICON_CLOSE color: mod.widgets.RINX_MUTED}}
        }
    }

    mod.widgets.MiniAppsTabStrip = #(MiniAppsTabStrip::register_widget(vm)) {
        width: Fill height: Fit flow: Right spacing: 2 align: Align{y: 0.5}
        Tab := mod.widgets.MiniAppsSideTab {}
    }

    mod.widgets.MiniAppsSideHandle = #(MiniAppsSideHandle::register_widget(vm)) {
        width: #(HANDLE) height: Fill
        // Use the very same shader and animation as the rooms-list splitter.
        draw_bg: mod.widgets.RobrixSplitter.draw_bg {is_vertical: 1.0}
        animator: mod.widgets.RobrixSplitter.animator
    }

    mod.widgets.MiniAppsSideRegion = #(MiniAppsSideRegion::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: 0 height: Fill flow: Right
        handle := mod.widgets.MiniAppsSideHandle {}
        frame := SolidView {
            width: Fill height: Fill flow: Down
            draw_bg.color: mod.widgets.RINX_PAGE
            header := SolidView {
                width: Fill height: Fit flow: Right align: Align{y: 0.5}
                padding: Inset{left: 4 right: 4 top: 2 bottom: 2} spacing: 2
                draw_bg.color: mod.widgets.RINX_SURFACE
                back := SideIconButton {
                    visible: false tooltip_text: #(crate::i18n::tr("Back to chat"))
                    icon +: {draw_icon +: {svg: ICON_CHEVRON_LEFT}}
                }
                tabs := mod.widgets.MiniAppsTabStrip {}
                hosting := NavigationBarButton {
                    visible: false width: Fit height: (mod.widgets.RINX_CONTROL_HEIGHT)
                    padding: Inset{left: 10 right: 10} align: Align{y: 0.5}
                    draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_radius: 6}
                    label := RinxLabel {text: #(crate::i18n::tr("Hosting"))}
                }
                pop_out := SideIconButton {
                    tooltip_text: #(crate::i18n::tr("Open in a new window"))
                    icon +: {draw_icon +: {svg: ICON_EXTERNAL_LINK}}
                }
            }
            SolidView {width: Fill height: 1 draw_bg.color: mod.widgets.RINX_BORDER}
            content := mod.widgets.MiniAppsSurfaceDeck {
                Surface := mod.widgets.MiniAppsSurface {
                    // The tabs sit above the panel; a narrow column keeps its edges close.
                    mini_apps_panel +: {padding: Inset{top: 8 right: 12 bottom: 12 left: 12}}
                }
            }
        }
    }
}

/// Tabs that have just started showing an app another tab already showed, as
/// (that tab, the tab to focus instead). Only a tab whose item changed counts,
/// so two tabs that already shared an app (one docked back from a window) stay.
fn reopened_apps(before: &[(LiveId, SurfaceKey)], now: &[(LiveId, SurfaceKey)]) -> Vec<(LiveId, LiveId)> {
    let mut found = Vec::new();
    for (tab, key) in now {
        if !matches!(key, SurfaceKey::App(_)) || before.iter().any(|(old, old_key)| old == tab && old_key == key) {
            continue;
        }
        let original = before.iter().find(|(old, old_key)| old != tab && old_key == key && now.iter().any(|(id, _)| id == old));
        if let Some((original, _)) = original {
            found.push((*tab, *original));
        }
    }
    found
}

/// Actions the app shell handles for the region.
#[derive(Clone, Debug)]
pub enum MiniAppsSideAction {
    /// Move the entire workspace into its own window.
    PopOut,
}

/// A move, not a snapshot: each live surface has exactly one host. Native
/// readers carry all their own browser tabs, and editors retain their drafts.
pub struct MiniAppsWorkspace {
    surfaces: Vec<(LiveId, WidgetRef)>,
    selected: Option<LiveId>,
}

/// How the region is laid out this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SideLayout {
    /// No tabs.
    #[default]
    Hidden,
    /// Beside the chat.
    Docked,
    /// Over the chat (shown unless the person went back to the chat).
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabRequest {
    Select(LiveId),
    Close(LiveId),
}

/// The region's tabs: one select button and one close button per tab.
#[derive(Script, WidgetRef, WidgetRegister)]
pub struct MiniAppsTabStrip {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[rust]
    area: Area,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live(true)]
    #[apply_state]
    visible: bool,
    #[rust]
    template: Option<ScriptObjectRef>,
    #[rust]
    tabs: Vec<(LiveId, WidgetRef)>,
    #[rust]
    requests: Vec<TabRequest>,
}

impl ScriptHook for MiniAppsTabStrip {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.template = None;
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if apply.is_eval() {
            return;
        }
        let Some(object) = value.as_object() else { return };
        let mut template = None;
        vm.vec_with(object, |vm, vec| {
            for kv in vec {
                if kv.key.as_id() == Some(id!(Tab)) && let Some(template_object) = kv.value.as_object() {
                    template = Some((vm.bx.heap.new_object_ref(template_object), kv.value));
                }
            }
        });
        if let Some((template_ref, value)) = template {
            if !apply.is_new() {
                for (_, tab) in &mut self.tabs {
                    let selected = tab.navigation_bar_button(vm.cx_mut(), ids!(select)).borrow()
                        .is_some_and(|button| button.animator_in_state(vm.cx_mut(), ids!(active.on)));
                    tab.script_apply(vm, apply, scope, value);
                    tab.navigation_bar_button(vm.cx_mut(), ids!(select)).set_selected(vm.cx_mut(), selected);
                }
            }
            self.template = Some(template_ref);
        }
        vm.cx_mut().widget_tree_mark_dirty(self.uid);
    }
}

impl WidgetNode for MiniAppsTabStrip {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn visible(&self) -> bool {
        self.visible
    }
    fn set_visible(&mut self, cx: &mut Cx, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.redraw(cx);
        }
    }
    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        for (id, tab) in &self.tabs {
            visit(*id, tab.clone());
        }
    }
}

impl Widget for MiniAppsTabStrip {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.visible && event.requires_visibility() {
            return;
        }
        for (id, tab) in &self.tabs {
            tab.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                if tab.navigation_bar_button(cx, ids!(select)).clicked(actions) {
                    self.requests.push(TabRequest::Select(*id));
                }
                if tab.navigation_bar_button(cx, ids!(close)).clicked(actions) {
                    self.requests.push(TabRequest::Close(*id));
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible {
            return DrawStep::done();
        }
        cx.begin_turtle(walk, self.layout);
        for (_, tab) in &self.tabs {
            tab.draw_all(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

impl MiniAppsTabStrip {
    /// Shows these tabs, in this order, with these titles and this selection.
    fn set_tabs(&mut self, cx: &mut Cx, items: &[(LiveId, String, bool)]) {
        let before: Vec<LiveId> = self.tabs.iter().map(|(id, _)| *id).collect();
        let mut tabs = Vec::with_capacity(items.len());
        for (id, title, selected) in items {
            let existing = self.tabs.iter().position(|(old, _)| old == id).map(|index| self.tabs.remove(index));
            let tab = match existing {
                Some((_, tab)) => tab,
                None => {
                    let Some(template) = self.template.as_ref() else { continue };
                    let value: ScriptValue = template.as_object().into();
                    let tab = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
                    cx.widget_tree_insert_child_deep(self.uid, *id, tab.clone());
                    tab
                }
            };
            // Each of these leaves an unchanged tab alone, so drawing never
            // schedules another frame by itself.
            tab.label(cx, ids!(select.title)).set_text(cx, title);
            let select = tab.navigation_bar_button(cx, ids!(select));
            if select.borrow().is_some_and(|button| button.animator_in_state(cx, ids!(active.on)) != *selected) {
                select.set_selected(cx, *selected);
            }
            if let Some(mut close) = tab.navigation_bar_button(cx, ids!(close)).borrow_mut() {
                close.set_tooltip_text(crate::i18n::tr("Close"));
            }
            tabs.push((*id, tab));
        }
        // Only adding, removing or reordering tabs changes the strip itself.
        if tabs.iter().map(|(id, _)| *id).ne(before.iter().copied()) {
            cx.widget_tree_mark_dirty(self.uid);
            self.redraw(cx);
        }
        self.tabs = tabs;
    }
}

/// The strip between the chat and the docked region: dragging it resizes the region.
#[derive(Script, ScriptHook, Widget, Animator)]
pub struct MiniAppsSideHandle {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live(true)]
    #[apply_state]
    #[visible]
    visible: bool,
    #[apply_default]
    animator: Animator,
    #[redraw]
    #[live]
    draw_bg: DrawSplitter,
    /// How far the current drag has moved, while one is under way.
    #[rust]
    drag: Option<f64>,
    #[rust]
    released: bool,
}

impl Widget for MiniAppsSideHandle {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.visible && event.requires_visibility() {
            return;
        }
        if self.animator_handle_event(cx, event).must_redraw() {
            self.draw_bg.redraw(cx);
        }
        // Match Makepad Splitter's native pointer behavior; only the owning
        // pane's resize calculation differs (the right pane grows leftward).
        match event.hits_with_options(cx, self.draw_bg.area(), HitOptions::new()
            .with_margin(Inset {left: crate::shared::pane_splitter::HIT_MARGIN, right: crate::shared::pane_splitter::HIT_MARGIN, top: 0.0, bottom: 0.0})
            .with_touch_margin(Inset {left: 8.0, right: 8.0, top: 0.0, bottom: 0.0})) {
            Hit::FingerHoverIn(_) => {
                cx.set_cursor(MouseCursor::ColResize);
                self.animator_play(cx, ids!(hover.on));
            }
            Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::ColResize),
            Hit::FingerHoverOut(_) => self.animator_play(cx, ids!(hover.off)),
            Hit::FingerDown(fe) if self.drag.is_none() && fe.is_primary_hit() => {
                cx.set_cursor(MouseCursor::ColResize);
                self.animator_play(cx, ids!(hover.drag));
                self.drag = Some(0.0);
            }
            Hit::FingerMove(fe) if self.drag.is_some() => {
                cx.set_cursor(MouseCursor::ColResize);
                self.drag = Some(fe.abs.x - fe.abs_start.x);
            }
            Hit::FingerUp(fe) if self.drag.is_some() => {
                self.released = true;
                self.animator_play(cx, if fe.is_over && fe.device.has_hovers() {ids!(hover.on)} else {ids!(hover.off)});
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.visible {
            self.draw_bg.draw_walk(cx, walk);
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct MiniAppsSideRegion {
    #[deref]
    view: View,
    /// Tab ids, left to right. The deck holds their surfaces, the strip their buttons.
    #[rust]
    tabs: Vec<LiveId>,
    #[rust]
    selected: Option<LiveId>,
    /// What each tab showed after the last event, to notice an app opened twice.
    #[rust]
    keys: Vec<(LiveId, SurfaceKey)>,
    /// The docked width the person chose, or the default.
    #[rust(SIDE_DEFAULT)]
    width: f64,
    /// The widest docked region that leaves the chat its minimum.
    #[rust(SIDE_DEFAULT)]
    max_width: f64,
    /// The width a drag started from, while one is under way.
    #[rust]
    drag_from: Option<f64>,
    #[rust]
    layout_mode: SideLayout,
    /// Overlay only: the person went back to the chat. Opening anything clears it.
    #[rust]
    dismissed: bool,
    #[rust(true)]
    foreground: bool,
    /// The tab titles last drawn, so an item that changes (a catalog tab
    /// opening its app) redraws its title, and nothing else does.
    #[rust]
    drawn_titles: Vec<(LiveId, String)>,
    #[rust]
    floating: bool,
    #[rust(true)]
    host_visible: bool,
}

impl Widget for MiniAppsSideRegion {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event {
            Event::Background => self.foreground = false,
            Event::Foreground => self.foreground = true,
            _ => {}
        }

        let (drag, released) = self
            .view
            .widget(cx, ids!(handle))
            .borrow_mut::<MiniAppsSideHandle>()
            .map(|mut handle| (handle.drag, std::mem::take(&mut handle.released)))
            .unwrap_or_default();
        if let Some(dx) = drag {
            let from = *self.drag_from.get_or_insert(self.width);
            let width = (from - dx).clamp(SIDE_MIN, self.max_width.max(SIDE_MIN));
            if width != self.width {
                self.width = width;
                // The chat beside the region resizes with it.
                cx.redraw_all();
            }
        }
        if released {
            if let Some(mut handle) = self.view.widget(cx, ids!(handle)).borrow_mut::<MiniAppsSideHandle>() {
                handle.drag = None;
            }
            self.drag_from = None;
            // Kept with the layout state, so the next session docks it this wide.
            if let Some(state) = scope.data.get_mut::<crate::app::AppState>() {
                state.mini_apps_width = Some(self.width);
            }
        }

        let requests = self
            .view
            .widget(cx, ids!(frame.header.tabs))
            .borrow_mut::<MiniAppsTabStrip>()
            .map(|mut strip| std::mem::take(&mut strip.requests))
            .unwrap_or_default();
        for request in requests {
            match request {
                TabRequest::Select(id) => {
                    self.select(cx, id);
                    self.refresh(cx);
                }
                TabRequest::Close(id) => self.close_tab(cx, id),
            }
        }
        for id in self.deck(cx).take_close_requests() {
            self.close_tab(cx, id);
        }

        if let Event::Actions(actions) = event {
            if self.view.navigation_bar_button(cx, ids!(frame.header.hosting)).clicked(actions) {
                if let Some(id) = self.selected { self.surface(cx, id).toggle_hosting(cx); }
                self.view.redraw(cx);
            }
            if self.view.navigation_bar_button(cx, ids!(frame.header.back)).clicked(actions) {
                self.dismissed = true;
                self.refresh(cx);
            }
            if !self.floating && self.view.navigation_bar_button(cx, ids!(frame.header.pop_out)).clicked(actions) && self.selected.is_some() {
                cx.action(MiniAppsSideAction::PopOut);
            }
            // Going to a chat from an app, or to another page from the left
            // bar, steps an overlay aside; a docked region stays beside the chat.
            if actions.iter().any(|action| {
                action.downcast_ref::<crate::hagency::ui::ProjectRoomNavigation>().is_some_and(|nav| nav.current())
                    || matches!(action.downcast_ref(), Some(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats))
                    || matches!(action.downcast_ref(), Some(crate::home::navigation_tab_bar::NavigationBarAction::TabSelected(_)))
            }) {
                self.reveal_chat(cx);
            }
            self.focus_existing_tab_for_reopened_app(cx);
            if !self.tabs.is_empty() {
                // A background panel may have told the assistant about itself;
                // the assistant follows the tab in front.
                self.publish_front(cx);
                if self.titles(cx) != self.drawn_titles {
                    self.view.redraw(cx);
                }
            }
        }
        self.sync_visibility(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Titles follow what each tab shows (a catalog tab becomes its app).
        let hosting = self.selected.and_then(|id| self.surface(cx, id).hosting_state(cx));
        self.view.widget(cx, ids!(frame.header.hosting)).set_visible(cx, hosting.is_some());
        self.view.label(cx, ids!(frame.header.hosting.label)).set_text(cx, crate::i18n::tr(if hosting == Some(true) {"Manage"} else {"Hosting"}));
        self.drawn_titles = self.titles(cx);
        let items: Vec<(LiveId, String, bool)> = self
            .drawn_titles
            .iter()
            .map(|(id, title)| (*id, title.clone(), Some(*id) == self.selected))
            .collect();
        if let Some(mut strip) = self.view.widget(cx, ids!(frame.header.tabs)).borrow_mut::<MiniAppsTabStrip>() {
            strip.set_tabs(cx, &items);
        }
        // Tooltips follow the interface language, including after a switch.
        for (path, text) in [(ids!(frame.header.back), "Back to chat"), (ids!(frame.header.pop_out), "Open in a new window")] {
            if let Some(mut button) = self.view.navigation_bar_button(cx, path).borrow_mut() {
                button.set_tooltip_text(crate::i18n::tr(text));
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl MiniAppsSideRegion {
    fn deck(&self, cx: &mut Cx) -> MiniAppsSurfaceDeckRef {
        self.view.mini_apps_surface_deck(cx, ids!(frame.content))
    }

    fn surface(&self, cx: &mut Cx, id: LiveId) -> MiniAppsSurfaceRef {
        self.deck(cx).surface(id)
    }

    fn titles(&self, cx: &mut Cx) -> Vec<(LiveId, String)> {
        let deck = self.deck(cx);
        self.tabs.iter().map(|id| (*id, deck.surface(*id).title(cx))).collect()
    }

    fn key_of(&self, cx: &mut Cx, id: LiveId) -> Option<SurfaceKey> {
        self.surface(cx, id).key(cx)
    }

    fn find(&self, cx: &mut Cx, key: &SurfaceKey) -> Option<LiveId> {
        self.tabs.iter().copied().find(|id| self.key_of(cx, *id).as_ref() == Some(key))
    }

    /// Chooses docked or overlay layout for this frame and sizes the region.
    /// `page_width` is the width beside the left bar; returns whether a
    /// desktop overlay covers that page.
    fn prepare(&mut self, cx: &mut Cx, page_width: f64, rooms_width: f64, desktop: bool, saved_width: Option<f64>) -> bool {
        if self.drag_from.is_none() {
            self.width = saved_width.unwrap_or(SIDE_DEFAULT).max(SIDE_MIN);
        }
        let room = page_width - rooms_width - HANDLE - CHAT_MIN;
        let mode = if self.tabs.is_empty() {
            SideLayout::Hidden
        } else if desktop && room >= SIDE_MIN {
            SideLayout::Docked
        } else {
            SideLayout::Overlay
        };
        // A window that shrinks under a docked region doesn't cover the chat by itself.
        if mode == SideLayout::Overlay && self.layout_mode == SideLayout::Docked {
            self.dismissed = true;
        }
        self.layout_mode = mode;
        self.max_width = room.max(SIDE_MIN);
        let shown = match mode {
            SideLayout::Hidden => false,
            SideLayout::Docked => true,
            SideLayout::Overlay => !self.dismissed,
        };
        self.view.walk.width = match mode {
            SideLayout::Docked => Size::Fixed(self.width.min(self.max_width) + HANDLE),
            _ if shown => Size::fill(),
            // Its CachedWidget wrapper reports itself visible, so the row would
            // still give a hidden Fill region half the width: take none.
            _ => Size::Fixed(0.0),
        };
        self.view.widget(cx, ids!(handle)).set_visible(cx, mode == SideLayout::Docked);
        self.view.widget(cx, ids!(frame.header.back)).set_visible(cx, mode == SideLayout::Overlay);
        if self.view.visible != shown {
            self.view.visible = shown;
            // This runs while drawing: tell the apps on the next frame's event,
            // not from inside the draw.
            cx.new_next_frame();
        }
        shown && mode == SideLayout::Overlay && desktop
    }

    fn select(&mut self, cx: &mut Cx, id: LiveId) {
        if !self.tabs.contains(&id) {
            return;
        }
        self.selected = Some(id);
        self.deck(cx).set_front(cx, Some(id));
        self.dismissed = false;
        // Routing actions can be dispatched before the next layout/draw.
        // Make the selected surface active before its queued Restore arrives.
        self.view.visible = true;
        self.publish_front(cx);
    }

    fn new_tab(&mut self, cx: &mut Cx) -> Option<LiveId> {
        let id = LiveId::unique();
        self.deck(cx).create(cx, id)?;
        self.tabs.push(id);
        Some(id)
    }

    /// Runs a routing call on one tab. A surface that closes itself in
    /// response (handing off to a native app) closes only its own tab.
    fn run_on(&mut self, cx: &mut Cx, id: LiveId, run: impl FnOnce(&mut Cx, &MiniAppsSurfaceRef)) {
        let surface = self.surface(cx, id);
        let emitted = cx.capture_actions(|cx| run(cx, &surface));
        let mut kept = ActionsBuf::new();
        let mut closed = false;
        for action in emitted {
            if matches!(action.downcast_ref(), Some(MiniAppsAction::Close)) {
                closed = true;
            } else {
                kept.push(action);
            }
        }
        cx.extend_actions(kept);
        if closed {
            self.close_tab(cx, id);
        }
    }

    /// Opens a mini-apps item in its tab, focusing the tab that already shows it.
    fn open(&mut self, cx: &mut Cx, action: &MiniAppsAction) {
        if matches!(action, MiniAppsAction::Close) {
            return;
        }
        let existing = match SurfaceKey::for_action(action) {
            Some(key) => self.find(cx, &key).or_else(|| {
                matches!(action, MiniAppsAction::OpenAgent).then(|| self.find(cx, &SurfaceKey::Management)).flatten()
            }),
            // The assistant opens the app reviewed in the tab in front, or in another app tab.
            None if matches!(action, MiniAppsAction::OpenReviewed) => self
                .selected
                .filter(|id| matches!(self.key_of(cx, *id), Some(SurfaceKey::App(_))))
                .or_else(|| self.tabs.iter().copied().find(|id| matches!(self.key_of(cx, *id), Some(SurfaceKey::App(_))))),
            None => return,
        };
        let Some(id) = existing.or_else(|| self.new_tab(cx)) else { return };
        self.select(cx, id);
        self.run_on(cx, id, |cx, surface| surface.action(cx, action));
        self.remember_keys(cx);
        self.refresh(cx);
    }

    /// Opens Hagency's management: in its tab, or over the Hagency app (so its
    /// Back returns to the app), or in a new tab.
    fn hagency_action(&mut self, cx: &mut Cx, action: &HagencyAction) {
        let hagency = SurfaceKey::App(octosense_app_contract::hagency::APP_ID.to_owned());
        let existing = self.find(cx, &SurfaceKey::Management).or_else(|| self.find(cx, &hagency));
        if matches!(action, HagencyAction::Close) {
            if let Some(id) = self.find(cx, &SurfaceKey::Management) {
                self.run_on(cx, id, |cx, surface| surface.hagency_action(cx, action));
                self.refresh(cx);
            }
            return;
        }
        let Some(id) = existing.or_else(|| self.new_tab(cx)) else { return };
        self.select(cx, id);
        self.run_on(cx, id, |cx, surface| surface.hagency_action(cx, action));
        self.remember_keys(cx);
        self.refresh(cx);
    }

    fn native_tab(&mut self, cx: &mut Cx, key: SurfaceKey, close: bool) -> Option<LiveId> {
        let existing = self.find(cx, &key);
        if close { if let Some(id) = existing { self.close_tab(cx, id); } return None; }
        let id = existing.or_else(|| self.new_tab(cx))?;
        self.select(cx, id);
        Some(id)
    }

    fn web_action(&mut self, cx: &mut Cx, action: &WebBrowserAction) {
        let Some(id) = self.native_tab(cx, SurfaceKey::Reader, matches!(action, WebBrowserAction::Close)) else { return };
        self.run_on(cx, id, |cx, surface| surface.web_action(cx, action));
        self.remember_keys(cx); self.refresh(cx);
    }

    fn article_action(&mut self, cx: &mut Cx, action: &ArticleAction) {
        let Some(id) = self.native_tab(cx, SurfaceKey::Articles, matches!(action, ArticleAction::Close)) else { return };
        self.run_on(cx, id, |cx, surface| surface.article_action(cx, action));
        self.remember_keys(cx); self.refresh(cx);
    }

    // TODO(R4): over budget; move the native-tab openers (reader, articles,
    // profile) out with surface.rs's native panels.
    fn show_profile(&mut self, cx: &mut Cx, info: &crate::profile::user_profile::UserProfilePaneInfo) {
        let Some(id) = self.native_tab(cx, SurfaceKey::Profile, false) else { return };
        self.run_on(cx, id, |cx, surface| surface.show_profile(cx, info));
        self.remember_keys(cx); self.refresh(cx);
    }

    /// Stops the tab's app, revoking its lease, and removes the tab.
    fn close_tab(&mut self, cx: &mut Cx, id: LiveId) {
        let Some(index) = self.tabs.iter().position(|tab| *tab == id) else { return };
        if let Some(surface) = self.deck(cx).remove(cx, id) {
            surface.as_mini_apps_surface().close(cx);
        }
        self.tabs.remove(index);
        self.keys.retain(|(tab, _)| *tab != id);
        if self.selected == Some(id) {
            self.selected = self.tabs.get(index.min(self.tabs.len().saturating_sub(1))).copied();
            self.deck(cx).set_front(cx, self.selected);
        }
        if self.tabs.is_empty() {
            // The review went with its tab: the assistant no longer has an app to open.
            crate::assistant::set_mini_apps(None, None);
        } else {
            self.publish_front(cx);
        }
        self.refresh(cx);
    }

    fn take_workspace(&mut self, cx: &mut Cx) -> Option<MiniAppsWorkspace> {
        if self.tabs.is_empty() {return None;}
        let deck = self.deck(cx);
        let selected = self.selected.take();
        let surfaces = std::mem::take(&mut self.tabs).into_iter().filter_map(|id| {
            let surface = deck.remove(cx, id)?;
            surface.as_mini_apps_surface().set_host_visible(cx, false);
            Some((id, surface))
        }).collect();
        self.keys.clear();
        self.drawn_titles.clear();
        deck.set_front(cx, None);
        deck.take_navigated();
        self.refresh(cx);
        Some(MiniAppsWorkspace {surfaces, selected})
    }

    fn restore_workspace(&mut self, cx: &mut Cx, workspace: MiniAppsWorkspace) {
        let deck = self.deck(cx);
        for (id, surface) in workspace.surfaces {
            deck.insert(cx, id, surface);
            if !self.tabs.contains(&id) {self.tabs.push(id);}
        }
        if let Some(id) = workspace.selected.or_else(|| self.tabs.first().copied()) {
            self.select(cx, id);
        }
        self.remember_keys(cx);
        self.refresh(cx);
    }

    fn prepare_window(&mut self, cx: &mut Cx) {
        self.floating = true;
        self.dismissed = false;
        self.view.walk.width = Size::fill();
        self.view.visible = !self.tabs.is_empty();
        for path in [ids!(handle).as_slice(), ids!(frame.header.back).as_slice(), ids!(frame.header.pop_out).as_slice()] {
            self.view.widget(cx, path).set_visible(cx, false);
        }
    }

    fn close_all(&mut self, cx: &mut Cx) {
        for id in self.tabs.clone() {
            self.close_tab(cx, id);
        }
    }

    /// Lets the person see the chat: an overlay steps aside, a docked region stays.
    fn reveal_chat(&mut self, cx: &mut Cx) {
        if !self.floating && self.layout_mode == SideLayout::Overlay && !self.dismissed {
            self.dismissed = true;
            self.refresh(cx);
        }
    }

    fn publish_front(&self, cx: &mut Cx) {
        if let Some(id) = self.selected {
            self.surface(cx, id).publish_to_assistant(cx);
        }
    }

    fn remember_keys(&mut self, cx: &mut Cx) {
        self.keys = self.tabs.iter().filter_map(|id| Some((*id, self.key_of(cx, *id)?))).collect();
    }

    /// An app picked from a catalog tab while another tab already shows it:
    /// focus that tab instead of keeping two copies.
    fn focus_existing_tab_for_reopened_app(&mut self, cx: &mut Cx) {
        let now: Vec<(LiveId, SurfaceKey)> = self.tabs.iter().filter_map(|id| Some((*id, self.key_of(cx, *id)?))).collect();
        for (duplicate, original) in reopened_apps(&self.keys, &now) {
            self.close_tab(cx, duplicate);
            self.select(cx, original);
            self.refresh(cx);
        }
        self.remember_keys(cx);
    }

    /// Tells each tab's app whether it is in front of the person.
    fn sync_visibility(&mut self, cx: &mut Cx) {
        let shown = self.view.visible && self.foreground && self.host_visible;
        let deck = self.deck(cx);
        for id in &self.tabs {
            deck.surface(*id).set_host_visible(cx, shown && Some(*id) == self.selected);
        }
    }

    /// Redraws everything: showing, hiding or resizing the region moves the chat.
    fn refresh(&mut self, cx: &mut Cx) {
        if self.floating {self.prepare_window(cx);}
        self.sync_visibility(cx);
        cx.redraw_all();
    }
}

impl MiniAppsSideRegionRef {
    pub fn web_action(&self, cx: &mut Cx, action: &WebBrowserAction) {
        if let Some(mut inner) = self.borrow_mut() { inner.web_action(cx, action); }
    }
    pub fn article_action(&self, cx: &mut Cx, action: &ArticleAction) {
        if let Some(mut inner) = self.borrow_mut() { inner.article_action(cx, action); }
    }
    pub fn show_profile(&self, cx: &mut Cx, info: &crate::profile::user_profile::UserProfilePaneInfo) {
        if let Some(mut inner) = self.borrow_mut() { inner.show_profile(cx, info); }
    }
    pub fn reader(&self, cx: &mut Cx) -> WebBrowserRef {
        self.borrow().and_then(|inner| inner.find(cx, &SurfaceKey::Reader).map(|id| inner.surface(cx, id).reader())).unwrap_or_default()
    }

    /// See [`MiniAppsSideRegion::prepare`].
    pub fn prepare(&self, cx: &mut Cx, page_width: f64, rooms_width: f64, desktop: bool, saved_width: Option<f64>) -> bool {
        self.borrow_mut()
            .is_some_and(|mut inner| inner.prepare(cx, page_width, rooms_width, desktop, saved_width))
    }
    pub fn open(&self, cx: &mut Cx, action: &MiniAppsAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.open(cx, action);
        }
    }
    pub fn hagency_action(&self, cx: &mut Cx, action: &HagencyAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.hagency_action(cx, action);
        }
    }
    pub fn take_workspace(&self, cx: &mut Cx) -> Option<MiniAppsWorkspace> {
        self.borrow_mut()?.take_workspace(cx)
    }
    pub fn restore_workspace(&self, cx: &mut Cx, workspace: MiniAppsWorkspace) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.restore_workspace(cx, workspace);
        }
    }
    pub fn prepare_window(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {inner.prepare_window(cx);}
    }
    pub fn set_host_visible(&self, cx: &mut Cx, visible: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.host_visible = visible;
            inner.sync_visibility(cx);
        }
    }
    pub fn close_all(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.close_all(cx);
        }
    }
    pub fn reveal_chat(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.reveal_chat(cx);
        }
    }
    /// The open tabs, left to right.
    pub fn tabs(&self) -> Vec<LiveId> {
        self.borrow().map(|inner| inner.tabs.clone()).unwrap_or_default()
    }
    pub fn selected(&self) -> Option<LiveId> {
        self.borrow().and_then(|inner| inner.selected)
    }
    pub fn layout(&self) -> SideLayout {
        self.borrow().map(|inner| inner.layout_mode).unwrap_or_default()
    }
    pub fn key(&self, cx: &mut Cx, id: LiveId) -> Option<SurfaceKey> {
        self.borrow().and_then(|inner| inner.key_of(cx, id))
    }
    pub fn surface(&self, cx: &mut Cx, id: LiveId) -> MiniAppsSurfaceRef {
        self.borrow().map(|inner| inner.surface(cx, id)).unwrap_or_default()
    }
    /// Whether any tab shows an app (reviewed or running).
    pub fn has_app(&self, cx: &mut Cx) -> bool {
        self.borrow().is_some_and(|inner| {
            inner.tabs.iter().any(|id| matches!(inner.key_of(cx, *id), Some(SurfaceKey::App(_))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_picked_again_from_a_catalog_focuses_the_tab_already_showing_it() {
        let (a, b, c) = (LiveId(1), LiveId(2), LiveId(3));
        let app = |id: &str| SurfaceKey::App(id.into());
        // Tab b's catalog picked the app tab a already shows: focus a, drop b.
        let before = [(a, app("hagency")), (b, SurfaceKey::Catalog), (c, SurfaceKey::Management)];
        let now = [(a, app("hagency")), (b, app("hagency")), (c, SurfaceKey::Management)];
        assert_eq!(reopened_apps(&before, &now), vec![(b, a)]);
        // A different app, management or the catalog never counts.
        assert!(reopened_apps(&before, &[(a, app("hagency")), (b, app("palpo")), (c, SurfaceKey::Management)]).is_empty());
        assert!(reopened_apps(&[(a, SurfaceKey::Management), (b, SurfaceKey::Catalog)], &[(a, SurfaceKey::Management), (b, SurfaceKey::Management)]).is_empty());
        // Two tabs that already showed the same app (one docked back from its window) stay.
        assert!(reopened_apps(&[(a, app("palpo")), (b, app("palpo"))], &[(a, app("palpo")), (b, app("palpo"))]).is_empty());
        // A closed original is no reason to drop the new tab.
        assert!(reopened_apps(&before, &[(b, app("hagency")), (c, SurfaceKey::Management)]).is_empty());
    }
}
