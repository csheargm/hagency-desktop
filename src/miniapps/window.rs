//! Hosts the whole mini-app workspace in one native window. Dock/undock moves
//! every live surface and its tabs together, retaining order, selection, leases,
//! browser sessions and editor drafts. Closing the window closes all its tabs.
//!
//! Makepad creates a window's OS window as soon as its `Window` widget is built,
//! and has no way to re-show a window once hidden. So this host lives directly
//! under the app's `Root` (which forwards every event, including draws, to it),
//! builds the `MiniAppsWindow` widget when a surface first pops out, and drops
//! it once the user or the app closes that window.
use makepad_widgets::*;
use super::surface::{MiniAppsSurfaceRef, SurfaceKey};
use super::side::{MiniAppsSideRegionRef, MiniAppsSideRegionWidgetRefExt, MiniAppsWorkspace};
use super::ui::MiniAppsAction;
use crate::hagency::ui::HagencyAction;
use crate::shared::web_browser::{WebBrowserAction, WebBrowserRef};
use crate::article_app::ArticleAction;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.MiniAppsWindow = Window {
        window.inner_size: vec2(1000, 800)
        window.title: #(crate::i18n::tr("Hagency · Mini apps"))
        pass.clear_color: #FFFFFF00
        caption_bar +: {
            draw_bg.color: mod.widgets.RINX_PAGE
            caption_label +: {
                label +: {
                    draw_text +: { color: mod.widgets.RINX_INK }
                    text: #(crate::i18n::tr("Hagency · Mini apps")) i18n_text: "Hagency · Mini apps"
                }
            }
        }
        body +: {
            flow: Down
            toolbar := SolidView {
                width: Fill height: Fit flow: Right
                padding: 8 align: Align {x: 1.0 y: 0.5}
                draw_bg.color: RINX_SURFACE
                dock_back := RinxBackButton {
                    text: #(crate::i18n::tr("Dock back")) i18n_text: "Dock back"
                }
            }
            workspace := MiniAppsSideRegion {}
        }
    }

    mod.widgets.MiniAppsWindowHost = #(MiniAppsWindowHost::register_widget(vm)) {}
}

#[derive(Clone, Debug, Default)]
pub enum MiniAppsWindowAction {
    DockBack(WindowId),
    #[default]
    None,
}

#[derive(Script, WidgetRef, WidgetRegister)]
pub struct MiniAppsWindowHost {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[rust]
    area: Area,
    /// The mini-apps window, while it is open.
    #[rust]
    window: Option<WidgetRef>,
    /// Set once we've asked the OS to close `window`, which is dropped on `WindowClosed`.
    #[rust]
    closing: bool,
    #[rust]
    focused: bool,
}

impl ScriptHook for MiniAppsWindowHost {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        scope: &mut Scope,
        _value: ScriptValue,
    ) {
        if apply.is_script_reapply() {
            if let Some(window) = &mut self.window {
                let value = script_eval!(vm, { mod.widgets.MiniAppsWindow {} });
                window.script_apply(vm, apply, scope, value);
            }
        }
    }
}

impl WidgetNode for MiniAppsWindowHost {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn area(&self) -> Area {
        self.area
    }
    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        Walk::default()
    }
    fn redraw(&mut self, cx: &mut Cx) {
        if let Some(window) = self.window.as_ref() {
            window.redraw(cx);
        }
    }
    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        if let Some(window) = self.window.as_ref() {
            visit(id!(mini_apps_window), window.clone());
        }
    }
}

impl Widget for MiniAppsWindowHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.as_window().window_id();
        if let Event::Actions(actions) = event {
            if !self.closing && window.button(cx, ids!(dock_back)).clicked(actions) {
                if let Some(id) = window_id {
                    cx.action(MiniAppsWindowAction::DockBack(id));
                }
            }
        }
        let was_focused = self.focused;
        match event {
            Event::WindowGotFocus(id) => self.focused = Some(*id) == window_id,
            Event::WindowLostFocus(id) if Some(*id) == window_id => self.focused = false,
            // Window-targeted input also establishes focus when there is no OS
            // activation event, as with Makepad's hidden-window instrument input.
            Event::MouseDown(e) => self.focused = Some(e.window_id) == window_id,
            Event::KeyDown(_)
            | Event::KeyUp(_)
            | Event::TextInput(_)
            | Event::BackPressed { .. }
                if !self.focused =>
            {
                return;
            }
            _ => {}
        }
        let closed = matches!(event, Event::WindowClosed(e) if Some(e.window_id) == window_id);
        let visibility = script_visibility_signal(event, window_id, was_focused, self.focused, closed);
        if let Some(visible) = visibility {
            self.workspace(cx).set_host_visible(cx, visible);
        }
        // `Window` records its own geometry as the app-wide display context, which
        // drives the main window's desktop/mobile layout choice. This small window
        // must not flip the main window into its mobile layout, so undo that.
        let saved_display =
            matches!(event, Event::WindowGeomChange(e) if Some(e.window_id) == window_id).then(
                || {
                    let dc = &cx.display_context;
                    (dc.screen_size, dc.safe_area_insets, dc.updated_on_event_id)
                },
            );
        window.handle_event(cx, event, scope);
        if let Some((screen_size, safe_area_insets, updated_on_event_id)) = saved_display {
            cx.display_context.screen_size = screen_size;
            cx.display_context.safe_area_insets = safe_area_insets;
            cx.display_context.updated_on_event_id = updated_on_event_id;
        }
        // A tab can close itself without closing its neighbours.
        if !closed && !self.closing && self.workspace(cx).tabs().is_empty() {
            self.close(cx);
        }
        if closed {
            self.workspace(cx).close_all(cx);
            self.window = None;
            self.closing = false;
            self.focused = false;
            cx.widget_tree_mark_dirty(self.uid);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        if let Some(window) = self.window.as_ref() {
            self.workspace(cx).prepare_window(cx);
            let walk = window.walk(cx);
            window.draw_walk(cx, scope, walk)?;
        }
        DrawStep::done()
    }
}

impl MiniAppsWindowHost {
    fn workspace(&self, cx: &mut Cx) -> MiniAppsSideRegionRef {
        self.window
            .as_ref()
            .map(|window| window.mini_apps_side_region(cx, ids!(workspace)))
            .unwrap_or_default()
    }

    fn surface(&self, cx: &mut Cx) -> MiniAppsSurfaceRef {
        let workspace = self.workspace(cx);
        workspace.selected().map(|id| workspace.surface(cx, id)).unwrap_or_default()
    }

    /// What the window shows, while it is open.
    fn surface_key(&self, cx: &mut Cx) -> Option<SurfaceKey> {
        if self.window.is_none() || self.closing {
            return None;
        }
        self.surface(cx).key(cx)
    }

    /// Moves all tabs into this window. Existing tabs, if any, remain open.
    pub fn adopt_workspace(&mut self, cx: &mut Cx, workspace: MiniAppsWorkspace) {
        self.ensure_window(cx);
        self.workspace(cx).restore_workspace(cx, workspace);
        self.workspace(cx).set_host_visible(cx, self.focused);
        if let Some(window) = &self.window {
            window.redraw(cx);
        }
    }

    /// Detach the running app before closing its now-empty native window.
    /// A delayed click from an older window must not move a newer app.
    fn take_for_dock(&mut self, cx: &mut Cx, expected: WindowId) -> Option<MiniAppsWorkspace> {
        if self.closing || self.window.as_ref()?.as_window().window_id() != Some(expected) {
            return None;
        }
        let workspace = self.workspace(cx).take_workspace(cx)?;
        self.focused = false;
        self.closing = true;
        cx.push_unique_platform_op(CxOsOp::CloseWindow(expected));
        Some(workspace)
    }

    fn ensure_window(&mut self, cx: &mut Cx) {
        if self.window.is_none() || self.closing {
            let window = cx.with_vm(|vm| {
                let template = vm.eval(script! { mod.widgets.MiniAppsWindow });
                WidgetRef::script_from_value(vm, template)
            });
            self.window = Some(window);
            self.closing = false;
            self.focused = false;
            self.workspace(cx).prepare_window(cx);
            self.workspace(cx).set_host_visible(cx, false);
            cx.widget_tree_mark_dirty(self.uid);
        } else {
            // Reusing the mini-app window should bring it back in front of the chat.
            // JIT PORT: upstream queues ShowWindow, which brings the window in front
            // and activates the app without re-entering AppKit from this handler.
            if let Some(window) = self.window.as_ref() {
                window.as_window().set_visible(cx, true);
            }
        }
    }

    /// Routes a mini-app action to the surface in this window, bringing it to front.
    pub fn action(&mut self, cx: &mut Cx, action: &MiniAppsAction) {
        if matches!(action, MiniAppsAction::Close) {
            self.close(cx);
            return;
        }
        self.ensure_window(cx);
        self.workspace(cx).open(cx, action);
        if let Some(window) = &self.window {
            window.redraw(cx);
        }
    }

    /// Routes Hagency management to the surface in this window, bringing it to front.
    pub fn hagency_action(&mut self, cx: &mut Cx, action: &HagencyAction) {
        log!("Hagency management {action:?} in the mini-apps window");
        self.ensure_window(cx);
        self.workspace(cx).hagency_action(cx, action);
        if let Some(window) = &self.window {
            window.redraw(cx);
        }
    }

    /// Closes the mini-apps window, if open, stopping its app.
    pub fn close(&mut self, cx: &mut Cx) {
        let Some(window) = self.window.clone() else {
            return;
        };
        self.workspace(cx).close_all(cx);
        if !self.closing {
            if let Some(window_id) = window.as_window().window_id() {
                cx.push_unique_platform_op(CxOsOp::CloseWindow(window_id));
            }
            self.closing = true;
        }
    }
}

impl MiniAppsWindowHostRef {
    pub fn adopt_workspace(&self, cx: &mut Cx, workspace: MiniAppsWorkspace) {
        if let Some(mut inner) = self.borrow_mut() {inner.adopt_workspace(cx, workspace);}
    }
    pub fn take_for_dock(&self, cx: &mut Cx, expected: WindowId) -> Option<MiniAppsWorkspace> {
        self.borrow_mut()?.take_for_dock(cx, expected)
    }
    pub fn is_open(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.window.is_some() && !inner.closing)
    }
    pub fn workspace(&self, cx: &mut Cx) -> MiniAppsSideRegionRef {
        self.borrow().map(|inner| inner.workspace(cx)).unwrap_or_default()
    }
    pub fn reader(&self, cx: &mut Cx) -> WebBrowserRef {self.workspace(cx).reader(cx)}
    pub fn web_action(&self, cx: &mut Cx, action: &WebBrowserAction) {
        if let Some(mut inner) = self.borrow_mut() {
            if !matches!(action, WebBrowserAction::Close) {inner.ensure_window(cx);}
            inner.workspace(cx).web_action(cx, action);
        }
    }
    pub fn article_action(&self, cx: &mut Cx, action: &ArticleAction) {
        if let Some(mut inner) = self.borrow_mut() {
            if !matches!(action, ArticleAction::Close) {inner.ensure_window(cx);}
            inner.workspace(cx).article_action(cx, action);
        }
    }
    pub fn hagency_action(&self, cx: &mut Cx, action: &HagencyAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.hagency_action(cx, action);
        }
    }
    /// What the window shows, while it is open.
    pub fn surface_key(&self, cx: &mut Cx) -> Option<SurfaceKey> {
        self.borrow().and_then(|inner| inner.surface_key(cx))
    }
    pub fn surface(&self, cx: &mut Cx) -> MiniAppsSurfaceRef {
        self.borrow().map(|inner| inner.surface(cx)).unwrap_or_default()
    }

    pub fn window_id(&self) -> Option<WindowId> {
        self.borrow().and_then(|inner| {
            inner
                .window
                .as_ref()
                .and_then(|window| window.as_window().window_id())
        })
    }

    pub fn action(&self, cx: &mut Cx, action: &MiniAppsAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.action(cx, action);
        }
    }
    pub fn close(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.close(cx);
        }
    }
}

// Keep global application activation separate from this window's focus. A
// foreground event never starts an observation for another active window.
fn script_visibility_signal(
    event: &Event,
    window_id: Option<WindowId>,
    was_focused: bool,
    focused: bool,
    closed: bool,
) -> Option<bool> {
    match event {
        Event::Background => Some(false),
        Event::Foreground => Some(focused),
        Event::WindowGotFocus(id) if Some(*id) == window_id && !was_focused => Some(true),
        _ if closed => Some(false),
        _ if was_focused != focused => Some(focused),
        _ => None,
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    #[test]
    fn native_hagency_refresh_visibility_is_bound_to_the_miniapp_window() {
        let own = WindowId(7, 1);
        let other = WindowId(8, 1);
        let signal = |event: Event, was, focused| {
            script_visibility_signal(&event, Some(own), was, focused, false)
        };
        assert_eq!(signal(Event::WindowGotFocus(other), false, false), None);
        assert_eq!(signal(Event::WindowGotFocus(own), false, true), Some(true));
        assert_eq!(signal(Event::WindowGotFocus(own), true, true), None);
        assert_eq!(signal(Event::WindowLostFocus(other), true, true), None);
        assert_eq!(signal(Event::WindowLostFocus(own), true, false), Some(false));
        assert_eq!(signal(Event::WindowGotFocus(other), true, false), Some(false));
        assert_eq!(signal(Event::Background, true, true), Some(false));
        assert_eq!(signal(Event::Foreground, false, false), Some(false));
        assert_eq!(signal(Event::Foreground, true, true), Some(true));
    }
}
