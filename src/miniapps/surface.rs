//! One mini-apps host body and a deck of them.
//!
//! A [`MiniAppsSurface`] is what the mini-apps window used to hold directly:
//! the apps panel, Hagency's native management pages, and the bar that goes
//! back from management to the apps. The docked side region keeps one per tab
//! and a popped-out window holds one, so an opened app keeps its state, its
//! lease and its startup guard when it moves between them.
//!
//! A [`MiniAppsSurfaceDeck`] holds several surfaces and shows the front one.
//! Like a `PageFlip`, it gives input only to the front surface but actions,
//! signals and timers to every surface, so an app in a background tab still
//! receives its replies. A surface's own `MiniAppsAction::Close` (Back on its
//! catalog, or handing off to a native app) closes only that surface's tab.
use makepad_widgets::*;
use super::ui::{MiniAppsAction, MiniAppsPanelRef, MiniAppsPanelWidgetExt};
use crate::hagency::ui::{HagencyAction, HagencyPanelWidgetExt};
use crate::hagency::owner_dashboard::OwnerDashboardHostWidgetExt;
use crate::shared::web_browser::{WebBrowserAction, WebBrowserRef, WebBrowserWidgetRefExt};
use crate::article_app::{ArticleAction, ArticlePanelWidgetRefExt};
use crate::profile::{profile_side_panel::ProfileSidePanelWidgetRefExt, user_profile::UserProfilePaneInfo};
use super::native_panels::{native_template, native_title};
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let ManagementTab = NavigationBarButton {
        width: Fit height: 44 flow: Right spacing: 8 padding: Inset{left: 14 right: 14}
        align: Align{y: 0.5}
        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_radius: theme.corner_radius}
        icon := Icon {icon_walk: Walk{width: 18 height: 18} draw_icon.color: mod.widgets.RINX_INK}
        label := RinxLabel {}
    }
    mod.widgets.MiniAppsSurface = #(MiniAppsSurface::register_widget(vm)) {
        width: Fill height: Fill flow: Down
        management_navigation := View {
            visible: false width: Fill height: Fit flow: Right spacing: 8
            padding: Inset{left: 20 right: 20 top: 8 bottom: 8}
            show_bg: true draw_bg.color: mod.widgets.RINX_PAGE
            apps := ManagementTab {icon.draw_icon.svg: ICON_ARROW_BACK label.text: #(crate::i18n::tr("Back to Hagency")) label.i18n_text: "Back to Hagency"}
            context := RinxHint {width: Fill text: ""}
        }
        owner_navigation := View {
            visible: false width: Fill height: Fit flow: Right spacing: 8
            padding: Inset{left: 12 right: 12 top: 0 bottom: 0}
            show_bg: true draw_bg.color: mod.widgets.RINX_PAGE
            manage := ManagementTab {label.text: #(crate::i18n::tr("Manage")) label.i18n_text: "Manage"}
            hosting := ManagementTab {label.text: #(crate::i18n::tr("Hosting")) label.i18n_text: "Hosting"}
        }
        owner_host := OwnerDashboardHost {visible: false}
        mini_apps_panel := MiniAppsPanel {
            // A window's caption bar sits above the panel.
            padding: Inset{top: 12 right: 20 bottom: 20 left: 20}
        }
        management_panel := HagencyPanel {visible: false}
        native_content := View {visible: false width: Fill height: Fill}
    }
    // Each deck declares `Surface := mod.widgets.MiniAppsSurface {...}`, the
    // template its new surfaces are built from.
    mod.widgets.MiniAppsSurfaceDeck = #(MiniAppsSurfaceDeck::register_widget(vm)) {
        width: Fill height: Fill
    }
}

/// Which opened item a surface shows, so opening it again finds its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceKey {
    /// The mini-apps catalog or Hub: no app opened yet.
    Catalog,
    /// One app, by manifest id, while it is reviewed or running.
    App(String),
    /// Hagency's native management pages.
    Management,
    Reader,
    Articles,
    /// A person's or an Agent's profile.
    Profile,
}

impl SurfaceKey {
    /// The item an open action asks for. The assistant's reviewed app and the
    /// account-bound shell navigations are resolved by the host instead.
    pub fn for_action(action: &MiniAppsAction) -> Option<Self> {
        match action {
            MiniAppsAction::Open => Some(Self::Catalog),
            MiniAppsAction::OpenAgent | MiniAppsAction::OpenPalpo(_) | MiniAppsAction::ReviewOwnerApprovals { .. } => Some(Self::App(super::palpo::APP_ID.to_owned())),
            _ => None,
        }
    }
}

#[derive(Script, Widget)]
pub struct MiniAppsSurface {
    #[deref]
    view: View,
    /// `None`: the apps panel; `Some(false)`: Projects; `Some(true)`: Agents.
    #[rust]
    management_page: Option<bool>,
    /// Whether the host shows this surface in front of the person.
    #[rust]
    host_visible: bool,
    /// The visibility last told to the running app, so it hears only changes.
    #[rust]
    notified: Option<bool>,
    #[rust]
    native: Option<(SurfaceKey, WidgetRef)>,
    #[rust]
    owner_page: bool,
    /// The hosting entry stays usable while the management app is loading or
    /// cannot acquire a local engine because execution lives in the daemon.
    #[rust]
    agent_surface: bool,
    /// A tab host supplies the hosting switch in its existing header.
    #[rust]
    header_navigation: bool,
}

impl ScriptHook for MiniAppsSurface {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, _value: ScriptValue) {
        // A restyle reapplies the DSL's visibility defaults; keep the page shown.
        if apply.is_script_reapply() {
            // Native panels are mounted lazily, outside the DSL's child list.
            // Restyle their existing instances so documents and drafts survive.
            if let Some((key, panel)) = &mut self.native {
                let value = native_template(vm, key);
                panel.script_apply(vm, apply, scope, value);
            }
            self.update_page(vm.cx_mut());
        }
    }
}

impl Widget for MiniAppsSurface {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::Actions(actions) = event {
            if self.view.navigation_bar_button(cx, ids!(owner_navigation.hosting)).clicked(actions) {
                self.owner_page = true;
                self.update_page(cx);
            } else if self.view.navigation_bar_button(cx, ids!(owner_navigation.manage)).clicked(actions) {
                self.owner_page = false;
                self.update_page(cx);
            }
        }
        let emitted = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        for action in emitted {
            let close = match self.native.as_ref().map(|(kind, _)| kind) {
                Some(SurfaceKey::Reader) => matches!(action.downcast_ref(), Some(WebBrowserAction::Close)),
                Some(SurfaceKey::Articles) => matches!(action.downcast_ref(), Some(ArticleAction::Close)),
                _ => false,
            };
            if close { cx.action(MiniAppsAction::Close); } else { cx.extend_actions(vec![action]); }
        }
        if let Event::Actions(actions) = event
            && self.view.navigation_bar_button(cx, ids!(management_navigation.apps)).clicked(actions)
        {
            self.show_apps(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl MiniAppsSurface {
    fn is_agent(&self, cx: &mut Cx) -> bool {
        self.agent_surface || self.panel(cx).current_app().is_some_and(|(id, _)| id == super::palpo::APP_ID)
    }
    fn panel(&self, cx: &mut Cx) -> MiniAppsPanelRef {
        self.view.mini_apps_panel(cx, ids!(mini_apps_panel))
    }

    fn update_page(&mut self, cx: &mut Cx) {
        let owner_available = self.is_agent(cx) && self.management_page.is_none() && self.native.is_none();
        let owner_visible = owner_available && self.owner_page;
        self.view.navigation_bar_button(cx, ids!(owner_navigation.manage)).set_selected(cx, !owner_visible);
        self.view.navigation_bar_button(cx, ids!(owner_navigation.hosting)).set_selected(cx, owner_visible);
        self.view.widget(cx, ids!(owner_navigation)).set_visible(cx, owner_available && !self.header_navigation);
        self.view.widget(cx, ids!(owner_host)).set_visible(cx, owner_visible);
        self.view.widget(cx, ids!(mini_apps_panel)).set_visible(cx, self.management_page.is_none() && self.native.is_none() && !owner_visible);
        self.view.widget(cx, ids!(management_panel)).set_visible(cx, self.management_page.is_some());
        self.view.view(cx, ids!(management_navigation)).set_visible(cx, self.management_page.is_some());
        self.view.label(cx, ids!(management_navigation.context)).set_text(cx, crate::i18n::tr(
            if self.management_page == Some(true) { "Local agents · Device and room settings" }
            else { "Agent spaces · Rooms, members and access" }
        ));
        self.view.widget(cx, ids!(native_content)).set_visible(cx, self.native.is_some());
        self.sync_script_visibility(cx);
        self.view.redraw(cx);
    }

    fn show_apps(&mut self, cx: &mut Cx) {
        self.view.hagency_panel(cx, ids!(management_panel)).action(cx, &HagencyAction::Close);
        self.management_page = None;
        self.update_page(cx);
    }

    /// Only the Hagency app listens, and only while its apps page is in front.
    fn sync_script_visibility(&mut self, cx: &mut Cx) {
        let available = self.host_visible && self.management_page.is_none() && self.native.is_none();
        let owner_visible = available && self.owner_page && self.is_agent(cx);
        self.view.owner_dashboard_host(cx, ids!(owner_host)).set_host_visible(cx, owner_visible);
        let visible = available && !owner_visible;
        if let Some((SurfaceKey::Reader, reader)) = &self.native {
            reader.as_web_browser().set_host_visible(cx, self.host_visible);
        }
        if self.notified != Some(visible) {
            self.notified = Some(visible);
            self.panel(cx).script_visibility(cx, visible);
        }
    }

    fn key(&self, cx: &mut Cx) -> SurfaceKey {
        if let Some((key, _)) = &self.native { return key.clone(); }
        if self.management_page.is_some() {
            return SurfaceKey::Management;
        }
        if self.agent_surface { return SurfaceKey::App(super::palpo::APP_ID.to_owned()); }
        match self.panel(cx).current_app() {
            Some((id, _)) => SurfaceKey::App(id),
            None => SurfaceKey::Catalog,
        }
    }

    fn title(&self, cx: &mut Cx) -> String {
        if let Some((key, _)) = &self.native {
            return native_title(key).to_owned();
        }
        if self.agent_surface && self.management_page.is_none() { return crate::i18n::tr("Agent").into(); }
        match self.management_page {
            Some(true) => crate::i18n::tr("Local agents").to_owned(),
            Some(false) => crate::i18n::tr("Agent spaces").to_owned(),
            None => self.panel(cx).current_app()
                .map(|(id, name)| if id == super::palpo::APP_ID {crate::i18n::tr("Agent").to_owned()} else {name})
                .unwrap_or_else(|| crate::i18n::tr("Mini apps").to_owned()),
        }
    }

    fn action(&mut self, cx: &mut Cx, action: &MiniAppsAction) {
        if matches!(action, MiniAppsAction::Close) {
            self.close(cx);
            return;
        }
        // Re-selecting Agent preserves both its script page and any native
        // management page opened over it, including unsaved form values.
        if matches!(action, MiniAppsAction::OpenAgent)
            && self.is_agent(cx)
        {
            self.panel(cx).action(cx, ModalRef::default(), action);
            return;
        }
        match action {
            MiniAppsAction::ReviewOwnerApprovals { .. } => { self.agent_surface = true; self.owner_page = false; },
            MiniAppsAction::OpenAgent | MiniAppsAction::OpenPalpo(_) => self.agent_surface = true,
            MiniAppsAction::Open | MiniAppsAction::OpenReviewed => { self.agent_surface = false; self.owner_page = false; },
            _ => {},
        }
        self.show_apps(cx);
        self.panel(cx).action(cx, ModalRef::default(), action);
        self.update_page(cx);
        self.view.redraw(cx);
    }

    fn hagency_action(&mut self, cx: &mut Cx, action: &HagencyAction) {
        if matches!(action, HagencyAction::Close) {
            self.show_apps(cx);
            return;
        }
        // As in the window: management opens over the apps, so its Back
        // returns to the apps.
        if !self.panel(cx).is_open() {
            self.panel(cx).action(cx, ModalRef::default(), &MiniAppsAction::Open);
        }
        self.management_page = Some(matches!(action,
            HagencyAction::OpenAgents | HagencyAction::OpenAgent {..} | HagencyAction::NewAgent {..} | HagencyAction::AgentRun {..}));
        self.view.hagency_panel(cx, ids!(management_panel)).action(cx, action);
        self.update_page(cx);
    }

    fn native_panel(&mut self, cx: &mut Cx, key: SurfaceKey) -> (WidgetRef, bool) {
        if let Some((existing, panel)) = &self.native {
            if *existing == key { return (panel.clone(), false); }
        }
        let panel = cx.with_vm(|vm| {
            let value = native_template(vm, &key);
            WidgetRef::script_from_value(vm, value)
        });
        if let Some(mut slot) = self.view.view(cx, ids!(native_content)).borrow_mut() {
            slot.children = vec![(id!(panel), panel.clone())].into();
            cx.widget_tree_mark_dirty(slot.widget_uid());
        }
        self.native = Some((key, panel.clone()));
        self.management_page = None;
        self.update_page(cx);
        (panel, true)
    }

    fn web_action(&mut self, cx: &mut Cx, action: &WebBrowserAction) {
        let (panel, _) = self.native_panel(cx, SurfaceKey::Reader);
        panel.as_web_browser().action(cx, ModalRef::default(), action);
        self.update_page(cx);
    }

    // TODO(R4): over budget; move the native-panel openers (reader, articles,
    // profile) into native_panels.rs.
    fn show_profile(&mut self, cx: &mut Cx, info: &UserProfilePaneInfo) {
        let (panel, _) = self.native_panel(cx, SurfaceKey::Profile);
        panel.as_profile_side_panel().show(cx, info.clone());
        self.update_page(cx);
    }

    fn article_action(&mut self, cx: &mut Cx, action: &ArticleAction) {
        let (panel, fresh) = self.native_panel(cx, SurfaceKey::Articles);
        // Selecting an already-open publisher must retain its draft and current page.
        if fresh || !matches!(action, ArticleAction::Open) {
            panel.as_article_panel().action(cx, ModalRef::default(), action);
        }
        self.update_page(cx);
    }

    /// Stops the app (revoking its lease) and closes management.
    fn close(&mut self, cx: &mut Cx) {
        self.view.owner_dashboard_host(cx, ids!(owner_host)).set_host_visible(cx, false);
        if let Some((kind, panel)) = self.native.take() {
            if kind == SurfaceKey::Reader {
                panel.as_web_browser().action(cx, ModalRef::default(), &WebBrowserAction::Close);
            } else if kind == SurfaceKey::Profile {
                // A profile holds no session or draft to end.
            } else {
                panel.as_article_panel().action(cx, ModalRef::default(), &ArticleAction::Close);
            }
        }
        self.view.hagency_panel(cx, ids!(management_panel)).action(cx, &HagencyAction::Close);
        self.panel(cx).action(cx, ModalRef::default(), &MiniAppsAction::Close);
        self.notified = Some(false);
    }
}

impl MiniAppsSurfaceRef {
    pub fn hosting_state(&self, cx: &mut Cx) -> Option<bool> {
        let inner = self.borrow()?;
        (inner.is_agent(cx) && inner.management_page.is_none() && inner.native.is_none()).then_some(inner.owner_page)
    }
    pub fn toggle_hosting(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.is_agent(cx) && inner.management_page.is_none() && inner.native.is_none() {
                inner.owner_page = !inner.owner_page;
                inner.update_page(cx);
            }
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn owner_fixture(&self, cx: &mut Cx, state: owner_dashboard::DashboardState) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.view.owner_dashboard_host(cx, ids!(owner_host)).load_fixture(cx, state);
            inner.owner_page = true;
            inner.agent_surface = true;
            inner.management_page = None;
            inner.update_page(cx);
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn update_owner_fixture(&self, cx: &mut Cx, snapshot: owner_dashboard::dto::DashboardSnapshot) {
        if let Some(inner) = self.borrow() { inner.view.owner_dashboard_host(cx, ids!(owner_host)).update_fixture(cx, snapshot); }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn owner_fixture_scope(&self, cx: &mut Cx, owner: Option<&str>) {
        if let Some(inner) = self.borrow() { inner.view.owner_dashboard_host(cx, ids!(owner_host)).fixture_scope(cx, owner); }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn owner_fixture_locale(&self, cx: &mut Cx, locale: owner_dashboard::Locale) {
        if let Some(inner) = self.borrow() { inner.view.owner_dashboard_host(cx, ids!(owner_host)).fixture_locale(cx, locale); }
    }
    /// Present a synthetic hosting state on this surface's Hosting page.
    #[cfg(feature = "onboarding-test-support")]
    pub fn owner_fixture_hosting(&self, cx: &mut Cx, view: &crate::hagency::hosting::View, keep_running: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.view.owner_dashboard_host(cx, ids!(owner_host)).fixture_hosting(cx, view, keep_running);
            inner.owner_page = true;
            inner.agent_surface = true;
            inner.management_page = None;
            inner.update_page(cx);
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn owner_fixture_view(&self, cx: &mut Cx) -> Option<owner_dashboard::ViewState> {
        self.borrow().and_then(|inner| inner.view.owner_dashboard_host(cx, ids!(owner_host)).fixture_view(cx))
    }
    pub fn web_action(&self, cx: &mut Cx, action: &WebBrowserAction) {
        if let Some(mut inner) = self.borrow_mut() { inner.web_action(cx, action); }
    }
    pub fn article_action(&self, cx: &mut Cx, action: &ArticleAction) {
        if let Some(mut inner) = self.borrow_mut() { inner.article_action(cx, action); }
    }
    pub fn show_profile(&self, cx: &mut Cx, info: &UserProfilePaneInfo) {
        if let Some(mut inner) = self.borrow_mut() { inner.show_profile(cx, info); }
    }
    pub fn reader(&self) -> WebBrowserRef {
        self.borrow().and_then(|inner| inner.native.as_ref().filter(|(key, _)| *key == SurfaceKey::Reader)
            .map(|(_, panel)| panel.as_web_browser())).unwrap_or_default()
    }

    pub fn key(&self, cx: &mut Cx) -> Option<SurfaceKey> {
        self.borrow().map(|inner| inner.key(cx))
    }
    pub fn title(&self, cx: &mut Cx) -> String {
        self.borrow().map(|inner| inner.title(cx)).unwrap_or_default()
    }
    pub fn action(&self, cx: &mut Cx, action: &MiniAppsAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.action(cx, action);
        }
    }
    pub fn hagency_action(&self, cx: &mut Cx, action: &HagencyAction) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.hagency_action(cx, action);
        }
    }
    pub fn close(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.close(cx);
        }
    }
    /// Whether the host shows this surface; the running app hears changes.
    pub fn set_host_visible(&self, cx: &mut Cx, visible: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.host_visible = visible;
            inner.sync_script_visibility(cx);
        }
    }
    pub fn panel(&self, cx: &mut Cx) -> MiniAppsPanelRef {
        self.borrow().map(|inner| inner.panel(cx)).unwrap_or_default()
    }
    /// Tells the assistant what this surface's panel shows.
    pub fn publish_to_assistant(&self, cx: &mut Cx) {
        if let Some(inner) = self.borrow() {
            inner.panel(cx).publish_to_assistant();
        }
    }
}

/// Several surfaces, one in front. See the module docs for event routing.
#[derive(Script, WidgetRef, WidgetRegister)]
pub struct MiniAppsSurfaceDeck {
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
    /// The `Surface := ...` template new surfaces are built from.
    #[rust]
    template: Option<ScriptObjectRef>,
    #[rust]
    surfaces: Vec<(LiveId, WidgetRef)>,
    #[rust]
    front: Option<LiveId>,
    /// Surfaces whose own panel asked to close, for the host to close.
    #[rust]
    close_requests: Vec<LiveId>,
    /// Whether one of these surfaces just sent the person to a chat (an agent
    /// chat, an actions room, a sign-up or tool approval).
    #[rust]
    navigated: bool,
}

impl ScriptHook for MiniAppsSurfaceDeck {
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
                if kv.key.as_id() == Some(id!(Surface)) && let Some(template_object) = kv.value.as_object() {
                    template = Some((vm.bx.heap.new_object_ref(template_object), kv.value));
                }
            }
        });
        if let Some((template_ref, value)) = template {
            // A restyle reaches the open surfaces too, as it would DSL children.
            if !apply.is_new() {
                for (_, surface) in &mut self.surfaces {
                    surface.script_apply(vm, apply, scope, value);
                }
            }
            self.template = Some(template_ref);
        }
        vm.cx_mut().widget_tree_mark_dirty(self.uid);
    }
}

impl WidgetNode for MiniAppsSurfaceDeck {
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
    /// Every surface, not only the front one: a language switch or restyle
    /// reaches background tabs too.
    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        for (id, surface) in &self.surfaces {
            visit(*id, surface.clone());
        }
    }
}

impl Widget for MiniAppsSurfaceDeck {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let input = event.requires_visibility();
        if input && !self.visible {
            return;
        }
        for (id, surface) in &self.surfaces {
            if input && Some(*id) != self.front {
                continue;
            }
            let emitted = cx.capture_actions(|cx| surface.handle_event(cx, event, scope));
            let mut kept = ActionsBuf::new();
            for action in emitted {
                if matches!(action.downcast_ref(), Some(MiniAppsAction::Close)) {
                    if !self.close_requests.contains(id) {
                        self.close_requests.push(*id);
                    }
                    continue;
                }
                if matches!(action.downcast_ref(), Some(
                    MiniAppsAction::OpenSignupApproval(_) | MiniAppsAction::OpenAgentChat(_)
                    | MiniAppsAction::OpenActionsRoom(_) | MiniAppsAction::OpenToolApprovals(_)
                )) {
                    self.navigated = true;
                }
                kept.push(action);
            }
            cx.extend_actions(kept);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible {
            return DrawStep::done();
        }
        cx.begin_turtle(walk, self.layout);
        if let Some(surface) = self.front_surface() {
            surface.draw_all(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

impl MiniAppsSurfaceDeck {
    fn front_surface(&self) -> Option<WidgetRef> {
        let front = self.front?;
        self.surfaces.iter().find(|(id, _)| *id == front).map(|(_, surface)| surface.clone())
    }

    /// A new surface from this deck's template, added in front.
    fn create(&mut self, cx: &mut Cx, id: LiveId) -> Option<MiniAppsSurfaceRef> {
        let template = self.template.as_ref()?;
        let value: ScriptValue = template.as_object().into();
        let surface = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        self.insert(cx, id, surface.clone());
        Some(surface.as_mini_apps_surface())
    }

    /// Adds an existing surface (one moving in from another host) in front.
    fn insert(&mut self, cx: &mut Cx, id: LiveId, surface: WidgetRef) {
        if let Some(mut inner) = surface.borrow_mut::<MiniAppsSurface>() {
            inner.header_navigation = true;
            inner.update_page(cx);
        }
        self.surfaces.retain(|(old, _)| *old != id);
        cx.widget_tree_insert_child_deep(self.uid, id, surface.clone());
        self.surfaces.push((id, surface));
        self.front = Some(id);
        cx.widget_tree_mark_dirty(self.uid);
        self.redraw(cx);
    }

    /// Removes a surface without closing it, for the host to close or move.
    fn remove(&mut self, cx: &mut Cx, id: LiveId) -> Option<WidgetRef> {
        let index = self.surfaces.iter().position(|(old, _)| *old == id)?;
        let (_, surface) = self.surfaces.remove(index);
        self.close_requests.retain(|request| *request != id);
        if self.front == Some(id) {
            self.front = None;
        }
        cx.widget_tree_mark_dirty(self.uid);
        self.redraw(cx);
        Some(surface)
    }
}

impl MiniAppsSurfaceDeckRef {
    pub fn create(&self, cx: &mut Cx, id: LiveId) -> Option<MiniAppsSurfaceRef> {
        self.borrow_mut()?.create(cx, id)
    }
    pub fn insert(&self, cx: &mut Cx, id: LiveId, surface: WidgetRef) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.insert(cx, id, surface);
        }
    }
    pub fn remove(&self, cx: &mut Cx, id: LiveId) -> Option<WidgetRef> {
        self.borrow_mut()?.remove(cx, id)
    }
    pub fn surface(&self, id: LiveId) -> MiniAppsSurfaceRef {
        self.borrow()
            .and_then(|inner| inner.surfaces.iter().find(|(old, _)| *old == id).map(|(_, surface)| surface.as_mini_apps_surface()))
            .unwrap_or_default()
    }
    pub fn surface_widget(&self, id: LiveId) -> Option<WidgetRef> {
        self.borrow()?.surfaces.iter().find(|(old, _)| *old == id).map(|(_, surface)| surface.clone())
    }
    pub fn set_front(&self, cx: &mut Cx, id: Option<LiveId>) {
        if let Some(mut inner) = self.borrow_mut()
            && inner.front != id
        {
            inner.front = id;
            inner.redraw(cx);
        }
    }
    pub fn front(&self) -> Option<LiveId> {
        self.borrow().and_then(|inner| inner.front)
    }
    pub fn ids(&self) -> Vec<LiveId> {
        self.borrow().map(|inner| inner.surfaces.iter().map(|(id, _)| *id).collect()).unwrap_or_default()
    }
    /// Surfaces whose own panel asked to close since the last call.
    pub fn take_close_requests(&self) -> Vec<LiveId> {
        self.borrow_mut().map(|mut inner| std::mem::take(&mut inner.close_requests)).unwrap_or_default()
    }
    /// Whether a surface sent the person to a chat since the last call.
    pub fn take_navigated(&self) -> bool {
        self.borrow_mut().is_some_and(|mut inner| std::mem::take(&mut inner.navigated))
    }
}
