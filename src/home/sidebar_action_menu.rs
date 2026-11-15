//! Creation shortcuts anchored beside permanent navigation rows.
//!
//! The "+" beside the desktop chat list's search field opens the
//! [`SidebarMenuKind::Add`] card, the one "+" menu (see [`plus_menu`]): New
//! chat, New room, Join a room, Create agent and, while a room is open, Add
//! agent to this room. Each row emits the same action its old entry point did.
// TODO(R4): over budget. Split along the menu kinds: the Project menu
// (ProjectMenuTarget, permission checks) and the render tests into their own files.
pub mod plus_menu;
use makepad_widgets::*;
use plus_menu::PlusItem;
use crate::shared::context_menu::{BUTTON_HEIGHT, ContextMenuClosed, expected_menu_size, menu_position_margin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMenuKind {
    Projects,
    Chats,
    Agents,
    Tools,
    /// The "New or Join" menu of the chat list's "+" button.
    Add,
}

/// The gap between a button and the menu card that drops down from it.
const DROP_DOWN_GAP: f64 = 4.0;

/// Where the "New or Join" card opens for the "+" button at `button` (its
/// window rect): just under the button, with the card's right edge on the
/// button's right edge, so the card hangs under the search row it belongs
/// to. [`SidebarActionMenu::show`] still keeps the card inside the window.
pub fn new_or_join_menu_pos(cx: &mut Cx, button: Rect) -> DVec2 {
    let size = menu_size(cx, SidebarMenuKind::Add);
    dvec2(
        button.pos.x + button.size.x - size.x,
        button.pos.y + button.size.y + DROP_DOWN_GAP,
    )
}

/// The card's size, for keeping it inside the window. It follows the DSL below:
/// the card widens with the text scale, and "New or Join" rows are one control
/// height (44 or more) tall.
fn menu_size(cx: &mut Cx, kind: SidebarMenuKind) -> DVec2 {
    let [scale, control_height] = cx.with_vm(|vm| {
        let widgets = vm.module(id!(widgets));
        [("RINX_TEXT_SCALE", 1.0), ("RINX_CONTROL_HEIGHT", 44.0)].map(|(name, fallback)| {
            vm.bx
                .heap
                .value(widgets, LiveId::from_str(name).into(), NoTrap)
                .as_number()
                .unwrap_or(fallback)
        })
    });
    let (rows, row_height) = match kind {
        SidebarMenuKind::Tools => (5, BUTTON_HEIGHT),
        SidebarMenuKind::Add => (plus_menu::items(plus_menu::open_room().is_some()).len(), control_height),
        _ => (1, BUTTON_HEIGHT),
    };
    let size = expected_menu_size(rows, 0);
    dvec2(size.x * scale, size.y + rows as f64 * (row_height - BUTTON_HEIGHT))
}
#[derive(Debug)]
pub enum SidebarMenuAction {
    Open {
        kind: SidebarMenuKind,
        pos: DVec2,
    },
    OpenProject {
        target: ProjectMenuTarget,
        pos: DVec2,
    },
    InviteProject {
        target: ProjectMenuTarget,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMenuTarget {
    pub project_id: String,
    pub space_id: matrix_sdk::ruma::OwnedRoomId,
    pub service_identity: String,
    pub owner_mxid: String,
    pub homeserver: String,
    pub epoch: u64,
}
impl ProjectMenuTarget {
    pub(crate) fn is_current(&self) -> bool {
        if self.epoch != crate::account_session::epoch() || crate::sliding_sync::account_changing()
        {
            return false;
        }
        let Ok(service) = crate::hagency::backend::Service::for_current() else {
            return false;
        };
        if service.identity() != self.service_identity {
            return false;
        }
        let Some(client) = crate::sliding_sync::get_client() else {
            return false;
        };
        let valid = client.homeserver().as_str() == self.homeserver
            && client
                .user_id()
                .is_some_and(|id| id.as_str() == self.owner_mxid)
            && client.get_room(&self.space_id).is_some_and(|room| {
                room.state() == matrix_sdk::RoomState::Joined && room.is_space()
            });
        valid
            && self.epoch == crate::account_session::epoch()
            && !crate::sliding_sync::account_changing()
    }
}
#[derive(Debug)]
struct ProjectPermission {
    target: ProjectMenuTarget,
    request: u64,
    can_invite: bool,
    invite: bool,
}
fn permission_matches(
    target: &ProjectMenuTarget,
    request: u64,
    permission: &ProjectPermission,
) -> bool {
    permission.request == request && &permission.target == target
}
fn next_request() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
fn project_navigation(
    target: &ProjectMenuTarget,
    button: LiveId,
) -> Option<crate::hagency::ui::HagencyAction> {
    use crate::hagency::ui::{HagencyAction, ProjectDetailSection};
    let section = match button {
        id!(project_overview) => ProjectDetailSection::Overview,
        id!(project_rooms) => ProjectDetailSection::Rooms,
        id!(project_members) => ProjectDetailSection::Members,
        _ => return None,
    };
    Some(HagencyAction::ProjectSection {
        project_id: target.project_id.clone(),
        section,
    })
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    // A "New or Join" row: a full control-height target (44 or more) that
    // grows with large text, with the action's icon before its label.
    mod.widgets.SidebarAddMenuItem = mod.widgets.ContextMenuButton {
        visible: false
        height: mod.widgets.RINX_CONTROL_HEIGHT
        icon_walk: Walk{width: 18 height: 18}
        draw_icon.color: mod.widgets.RINX_ACCENT
    }
    mod.widgets.SidebarActionMenu = set_type_default() do #(SidebarActionMenu::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fill flow: Overlay
        // A hit-testing overlay, not a full-window paint surface. Do not use
        // SolidView/transparent color: only the anchored card submits pixels.
        show_bg: false padding: 0 margin: 0 spacing: 0
        cursor: MouseCursor.Default align: Align{x: 0 y: 0}
        main_content := mod.widgets.ContextMenuContent {
            // Widens with large text so no label is cut off.
            width: (235 * mod.widgets.RINX_TEXT_SCALE) height: Fit flow: Down align: Align{x: 0 y: 0}
            padding: 6 spacing: 2 show_bg: true
            draw_bg +: {color: mod.widgets.RINX_SURFACE border_size: 1 border_color: mod.widgets.RINX_MUTED}
            // The "+" menu's rows, in `PlusItem::ALL` order.
            plus_new_chat := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("New chat")) i18n_text: "New chat" draw_icon.svg: ICON_PEOPLE}
            plus_new_room := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("New room")) i18n_text: "New room" draw_icon.svg: ICON_ADD}
            // TODO(R4): sidebar_action_menu.rs is over budget; move the "+" rows' DSL into plus_menu.rs.
            plus_new_space := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("New space")) i18n_text: "New space" draw_icon.svg: ICON_SQUARES}
            plus_join_room := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("Join or explore rooms")) i18n_text: "Join or explore rooms" draw_icon.svg: ICON_JOIN_ROOM}
            plus_create_agent := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("Create agent")) i18n_text: "Create agent" draw_icon.svg: ICON_ROBOT}
            plus_add_agent := mod.widgets.SidebarAddMenuItem {text: #(crate::i18n::tr("Add agent to this room")) i18n_text: "Add agent to this room" draw_icon.svg: ICON_ADD_USER}
            project_overview := mod.widgets.ContextMenuButton {visible: false icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Overview")) i18n_text: "Overview"}
            project_rooms := mod.widgets.ContextMenuButton {visible: false icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Discussion Rooms")) i18n_text: "Discussion Rooms"}
            project_members := mod.widgets.ContextMenuButton {visible: false icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Project members")) i18n_text: "Project members"}
            project_invite := mod.widgets.ContextMenuButton {visible: false enabled: false icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Invite members")) i18n_text: "Invite members"}
            project_space := mod.widgets.ContextMenuButton {visible: false icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Open Space")) i18n_text: "Open Space"}
            new_project := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("New project")) i18n_text: "New project"}
            new_chat := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("New chat")) i18n_text: "New chat"}
            new_agent := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("New agent")) i18n_text: "New agent"}
            open_discover := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Discover")) i18n_text: "Discover"}
            explore_rooms := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Explore rooms")) i18n_text: "Explore rooms"}
            open_moments := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Moments")) i18n_text: "Moments"}
            // File Transfer has its own rail button (mobile: Discover), so it is not repeated here.
            open_article := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Article editor")) i18n_text: "Article editor"}
            open_browser := mod.widgets.ContextMenuButton {icon_walk: Walk{width: 0 height: 0} spacing: 0 text: #(crate::i18n::tr("Browser")) i18n_text: "Browser"}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SidebarActionMenu {
    #[deref]
    view: View,
    #[rust]
    kind: Option<SidebarMenuKind>,
    #[rust]
    focus_after_draw: bool,
    #[rust]
    project: Option<ProjectMenuTarget>,
    #[rust]
    request: u64,
    /// The room open when the "+" menu opened: "Add agent to this room" acts on it.
    #[rust]
    plus_room: Option<crate::utils::RoomNameId>,
}
impl SidebarActionMenu {
    pub fn show(&mut self, cx: &mut Cx, kind: SidebarMenuKind, pos: DVec2, container: Rect) {
        self.project = None;
        self.request = next_request();
        self.project_buttons(cx, false);
        self.kind = Some(kind);
        self.plus_room = plus_menu::open_room().filter(|_| kind == SidebarMenuKind::Add);
        let plus_rows = plus_menu::items(self.plus_room.is_some());
        for (button, shown) in [
            (id!(new_project), kind == SidebarMenuKind::Projects),
            (id!(new_chat), kind == SidebarMenuKind::Chats),
            (id!(new_agent), kind == SidebarMenuKind::Agents),
        ]
        .into_iter()
        .chain(PlusItem::ALL.map(|item| (item.row(), kind == SidebarMenuKind::Add && plus_rows.contains(&item))))
        {
            let button = self.view.button(cx, &[button]);
            button.set_visible(cx, shown);
            button.reset_hover(cx);
        }
        for id in [
            id!(open_discover),
            id!(explore_rooms),
            id!(open_moments),
            id!(open_article),
            id!(open_browser),
        ] {
            let button = self.view.button(cx, &[id]);
            button.set_visible(cx, kind == SidebarMenuKind::Tools);
            button.reset_hover(cx);
        }
        let margin = menu_position_margin(container, pos, menu_size(cx, kind));
        let mut content = self.view.view(cx, ids!(main_content));
        script_apply_eval!(cx, content, {margin: #(margin)});
        self.visible = true;
        self.focus_after_draw = true;
        self.redraw(cx);
    }
    fn project_buttons(&self, cx: &mut Cx, visible: bool) {
        for id in [
            id!(project_overview),
            id!(project_rooms),
            id!(project_members),
            id!(project_invite),
            id!(project_space),
        ] {
            let button = self.view.button(cx, &[id]);
            button.set_visible(cx, visible);
            button.reset_hover(cx);
        }
        self.view
            .button(cx, ids!(project_invite))
            .set_enabled(cx, false);
        self.view
            .button(cx, ids!(project_invite))
            .set_disabled(cx, true);
    }
    fn check_permission(&mut self, target: ProjectMenuTarget, invite: bool) {
        let Ok(service) = crate::hagency::backend::Service::for_current() else {
            return;
        };
        if service.identity() != target.service_identity || !target.is_current() {
            return;
        }
        self.request = next_request();
        let request = self.request;
        crate::sliding_sync::spawn_async_task(async move {
            let can_invite = service
                .room_members(target.space_id.to_string(), true)
                .await
                .is_ok_and(|value| value["canInvite"] == true);
            Cx::post_action(ProjectPermission {
                target,
                request,
                can_invite,
                invite,
            });
        });
    }
    pub fn show_project(
        &mut self,
        cx: &mut Cx,
        target: ProjectMenuTarget,
        pos: DVec2,
        container: Rect,
    ) {
        if !target.is_current() {
            self.close(cx);
            return;
        }
        self.show(cx, SidebarMenuKind::Projects, pos, container);
        self.view
            .button(cx, ids!(new_project))
            .set_visible(cx, false);
        self.project_buttons(cx, true);
        let margin = menu_position_margin(container, pos, expected_menu_size(5, 0));
        let mut content = self.view.view(cx, ids!(main_content));
        script_apply_eval!(cx, content, {margin: #(margin)});
        self.project = Some(target.clone());
        self.check_permission(target, false);
    }
    fn close(&mut self, cx: &mut Cx) {
        self.visible = false;
        self.kind = None;
        self.project = None;
        self.plus_room = None;
        self.request = next_request();
        self.focus_after_draw = false;
        cx.revert_key_focus();
        cx.unblock_scrolling();
        cx.clear_all_hovers();
        cx.action(ContextMenuClosed);
        self.redraw(cx);
    }
}
impl SidebarActionMenuRef {
    pub fn show_project(
        &self,
        cx: &mut Cx,
        target: ProjectMenuTarget,
        pos: DVec2,
        container: Rect,
    ) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.show_project(cx, target, pos, container);
        }
    }

    pub fn show(&self, cx: &mut Cx, kind: SidebarMenuKind, pos: DVec2, container: Rect) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.show(cx, kind, pos, container);
        }
    }
}
impl Widget for SidebarActionMenu {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step = self.view.draw_walk(cx, scope, walk);
        if self.visible {
            let area = self.view.view(cx, ids!(main_content)).area();
            cx.block_scrolling_except_within(area);
            if step.is_done() && self.focus_after_draw {
                cx.set_key_focus(self.view.area());
                self.focus_after_draw = false;
            }
        }
        step
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Hidden or not, follow the open room for the "+" menu's last row.
        if let Event::Actions(actions) = event {
            plus_menu::observe(actions);
        }
        if !self.visible {
            return;
        }
        if self
            .project
            .as_ref()
            .is_some_and(|target| !target.is_current())
        {
            self.close(cx);
            return;
        }
        self.view.handle_event(cx, event, scope);
        let dismiss = event.back_pressed()
            || matches!(
                event,
                Event::KeyDown(KeyEvent {
                    key_code: KeyCode::Escape,
                    ..
                })
            )
            || matches!(event.hits_with_capture_overload(cx, self.view.area(), true), Hit::FingerUp(up) if up.is_over && !self.view.view(cx, ids!(main_content)).area().rect(cx).contains(up.abs));
        if dismiss {
            self.close(cx);
            return;
        }
        if let Event::Actions(actions) = event {
            if actions.iter().any(|action| {
                matches!(
                    action.downcast_ref(),
                    Some(crate::logout::logout_confirm_modal::LogoutAction::ClearAppState { .. })
                )
            }) {
                self.close(cx);
                return;
            }
            if let Some(target) = self.project.clone() {
                for action in actions {
                    if let Some(permission) = action.downcast_ref::<ProjectPermission>()
                        && permission_matches(&target, self.request, permission)
                        && target.is_current()
                    {
                        if permission.invite {
                            if permission.can_invite {
                                cx.action(SidebarMenuAction::InviteProject {
                                    target: target.clone(),
                                });
                            }
                            self.close(cx);
                            return;
                        }
                        self.view
                            .button(cx, ids!(project_invite))
                            .set_enabled(cx, permission.can_invite);
                        self.view
                            .button(cx, ids!(project_invite))
                            .set_disabled(cx, !permission.can_invite);
                    }
                }
                let selected = [
                    id!(project_overview),
                    id!(project_rooms),
                    id!(project_members),
                    id!(project_invite),
                    id!(project_space),
                ]
                .into_iter()
                .find(|id| self.view.button(cx, &[*id]).clicked(actions));
                if let Some(button) = selected {
                    if !target.is_current() {
                        self.close(cx);
                        return;
                    }
                    if button == id!(project_invite) {
                        if self
                            .view
                            .button(cx, ids!(project_invite))
                            .borrow()
                            .is_some_and(|button| button.enabled())
                        {
                            self.view
                                .button(cx, ids!(project_invite))
                                .set_enabled(cx, false);
                            self.view
                                .button(cx, ids!(project_invite))
                                .set_disabled(cx, true);
                            self.check_permission(target, true);
                        }
                        return;
                    }
                    if let Some(action) = project_navigation(&target, button) {
                        cx.action(action);
                    }
                    if button == id!(project_space) {
                        cx.action(crate::app::AppStateAction::NavigateToRoom {
                            room_to_close: None,
                            destination_room: crate::room::BasicRoomDetails::RoomId(
                                crate::utils::RoomNameId::empty(target.space_id),
                            ),
                        });
                    }
                    self.close(cx);
                    return;
                }
                return;
            }

            if self.kind == Some(SidebarMenuKind::Add) {
                let selected = PlusItem::ALL
                    .into_iter()
                    .find(|item| self.view.button(cx, &[item.row()]).clicked(actions));
                if let Some(item) = selected {
                    let room = self.plus_room.clone();
                    plus_menu::run(cx, item, room.as_ref());
                    self.close(cx);
                    return;
                }
            }
            if self.kind == Some(SidebarMenuKind::Tools) {
                let selected = [
                    id!(open_discover),
                    id!(explore_rooms),
                    id!(open_moments),
                    id!(open_article),
                    id!(open_browser),
                ]
                .into_iter()
                .find(|id| self.view.button(cx, &[*id]).clicked(actions));
                if let Some(id) = selected {
                    match id {
                        id!(open_discover) => {
                            cx.action(super::navigation_tab_bar::NavigationBarAction::GoToTab(
                                super::navigation_tab_bar::SelectedTab::Discover,
                            ))
                        }
                        id!(explore_rooms) => {
                            cx.action(super::navigation_tab_bar::NavigationBarAction::GoToAddRoom)
                        }
                        id!(open_moments) => {
                            cx.action(crate::moments::ui::MomentsAction::Open { author: None })
                        }
                        id!(open_article) => crate::system_apps::open_article(cx),
                        id!(open_browser) => {
                            cx.action(crate::shared::web_browser::WebBrowserAction::Open(
                                "about:blank".parse().unwrap(),
                            ))
                        }
                        _ => {}
                    }
                    self.close(cx);
                    return;
                }
            }
            if actions.iter().any(|a| {
                matches!(
                    a.downcast_ref(),
                    Some(crate::logout::logout_confirm_modal::LogoutAction::ClearAppState { .. })
                )
            }) {
                self.close(cx);
                return;
            }
            let selected = match self.kind {
                Some(SidebarMenuKind::Projects) => {
                    self.view.button(cx, ids!(new_project)).clicked(actions)
                }
                Some(SidebarMenuKind::Chats) => {
                    self.view.button(cx, ids!(new_chat)).clicked(actions)
                }
                Some(SidebarMenuKind::Agents) => {
                    self.view.button(cx, ids!(new_agent)).clicked(actions)
                }
                Some(SidebarMenuKind::Tools | SidebarMenuKind::Add) | None => false,
            };
            if selected {
                match self.kind {
                    Some(SidebarMenuKind::Projects) => {
                        cx.action(crate::hagency::ui::HagencyAction::NewProject)
                    }
                    Some(SidebarMenuKind::Chats) => {
                        cx.action(super::navigation_tab_bar::NavigationBarAction::NewChat)
                    }
                    Some(SidebarMenuKind::Agents) => {
                        cx.action(crate::hagency::create_agent::CreateAgentAction::Open)
                    }
                    Some(SidebarMenuKind::Tools | SidebarMenuKind::Add) | None => {}
                }
                self.close(cx);
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn target() -> ProjectMenuTarget {
        ProjectMenuTarget {
            project_id: "project-a".into(),
            space_id: matrix_sdk::ruma::room_id!("!space:server").to_owned(),
            service_identity: "old-owner/server/epoch".into(),
            owner_mxid: "@old:server".into(),
            homeserver: "https://server/".into(),
            epoch: 0,
        }
    }
    #[test]
    fn project_menu_permission_is_bound_to_identity_space_project_and_request() {
        let target = target();
        let permission = ProjectPermission {
            target: target.clone(),
            request: 5,
            can_invite: true,
            invite: true,
        };
        assert!(permission_matches(&target, 5, &permission));
        assert!(!permission_matches(&target, 6, &permission));
        for changed in [
            ProjectMenuTarget {
                service_identity: "new-owner/server/epoch".into(),
                ..target.clone()
            },
            ProjectMenuTarget {
                space_id: matrix_sdk::ruma::room_id!("!other:server").to_owned(),
                ..target.clone()
            },
            ProjectMenuTarget {
                project_id: "project-b".into(),
                ..target.clone()
            },
        ] {
            assert!(!permission_matches(&changed, 5, &permission));
        }
        use crate::hagency::ui::{HagencyAction, ProjectDetailSection};
        for (button, section) in [
            (id!(project_overview), ProjectDetailSection::Overview),
            (id!(project_rooms), ProjectDetailSection::Rooms),
            (id!(project_members), ProjectDetailSection::Members),
        ] {
            assert!(
                matches!(project_navigation(&target, button), Some(HagencyAction::ProjectSection {project_id, section: actual}) if project_id == target.project_id && actual == section)
            );
        }
        assert!(project_navigation(&target, id!(project_invite)).is_none());
        assert!(project_navigation(&target, id!(project_space)).is_none());
    }
    #[test]
    fn registered_project_menu_has_five_actions_and_stale_targets_cannot_navigate_or_invite() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut menu = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.SidebarActionMenu {}});
            let menu = SidebarActionMenu::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Project menu registration: {errors:?}");
            menu
        });
        let target = target();
        assert!(!target.is_current());
        let container = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(1280.0, 760.0),
        };
        for id in [
            id!(project_overview),
            id!(project_rooms),
            id!(project_members),
            id!(project_invite),
            id!(project_space),
        ] {
            menu.show(
                &mut cx,
                SidebarMenuKind::Projects,
                dvec2(200.0, 100.0),
                container,
            );
            menu.project_buttons(&mut cx, true);
            menu.project = Some(target.clone());
            assert!(menu.view.button(&mut cx, &[id]).visible());
            assert!(
                !menu
                    .view
                    .button(&mut cx, ids!(project_invite))
                    .borrow()
                    .unwrap()
                    .enabled()
            );
            let uid = menu.view.button(&mut cx, &[id]).widget_uid();
            let actions = cx.capture_actions(|cx| {
                cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default()));
                cx.action(ProjectPermission {
                    target: target.clone(),
                    request: menu.request,
                    can_invite: true,
                    invite: true,
                });
            });
            let emitted = cx.capture_actions(|cx| {
                menu.handle_event(cx, &Event::Actions(actions), &mut Scope::empty())
            });
            assert!(!emitted.iter().any(|action| {
                action
                    .downcast_ref::<crate::hagency::ui::HagencyAction>()
                    .is_some()
                    || action
                        .downcast_ref::<super::super::invite_modal::InviteModalAction>()
                        .is_some()
                    || action
                        .downcast_ref::<crate::app::AppStateAction>()
                        .is_some()
            }));
            assert!(!menu.visible && menu.project.is_none());
        }
        menu.show_project(&mut cx, target, dvec2(200.0, 100.0), container);
        assert!(!menu.visible, "An expired target cannot even reopen a menu");
        menu.show(
            &mut cx,
            SidebarMenuKind::Projects,
            dvec2(200.0, 100.0),
            container,
        );
        for id in [
            id!(project_overview),
            id!(project_rooms),
            id!(project_members),
            id!(project_invite),
            id!(project_space),
        ] {
            assert!(
                !menu.view.button(&mut cx, &[id]).visible(),
                "Normal create menu cannot leak old Project actions"
            );
        }
    }

    #[test]
    fn popup_records_only_anchored_card_pixels_and_preserves_underlay() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (mut background, mut menu) = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let background = script_eval!(vm, {mod.widgets.SolidView {
                draw_bg.color: #1378b5
            }});
            let background = View::script_from_value(vm, background);
            let menu = script_eval!(vm, {mod.widgets.SidebarActionMenu {}});
            let menu = SidebarActionMenu::script_from_value(vm, menu);
            let errors = vm.take_errors();
            assert!(
                errors.is_empty(),
                "Render fixture script errors: {errors:?}"
            );
            (background, menu)
        });
        let anchor = dvec2(210.0, 100.0);
        let viewport = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(1280.0, 760.0),
        };
        menu.show(&mut cx, SidebarMenuKind::Projects, anchor, viewport);
        cx.redraw_all();
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, viewport.size);
        let mut scene = DrawList2d::new(&mut cx);
        let mut popup = DrawList2d::new(&mut cx);
        let event = DrawEvent::default();
        let popup_list;
        {
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            scene.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(viewport.size, Layout::flow_overlay());
            let walk = Walk {
                width: Size::Fixed(1280.0),
                height: Size::Fixed(760.0),
                ..Walk::default()
            };
            background.draw_walk_all(&mut cx2d, &mut Scope::empty(), walk);
            popup.begin_always(&mut cx2d);
            popup_list = cx2d.get_current_draw_list_id().unwrap();
            menu.draw_walk_all(&mut cx2d, &mut Scope::empty(), walk);
            popup.end(&mut cx2d);
            cx2d.end_pass_sized_turtle();
            scene.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        let card = menu.view.view(&mut cx, ids!(main_content)).area().rect(&cx);
        assert!(
            (card.pos.x - anchor.x).abs() < 1.0 && (card.pos.y - anchor.y).abs() < 1.0,
            "Card is anchored below its row: {card:?}"
        );
        assert!(
            (card.size.x - 235.0).abs() < 1.0 && card.size.y > 38.0 && card.size.y < 80.0,
            "Single-action card uses Fit height: {card:?}"
        );
        // Inspect the actual submitted draw-command instance buffers, rather
        // than just asserting show_bg/color properties or widget rectangles.
        let list = &cx.draw_lists[popup_list];
        let outside = dvec2(900.0, 600.0);
        let mut primitives = 0;
        for index in 0..list.draw_items.len() {
            let item = &list.draw_items[index];
            let Some(call) = item.draw_call() else {
                continue;
            };
            let Some(instances) = item.instances.as_ref() else {
                continue;
            };
            let mapping = &cx.draw_shaders.shaders[call.draw_shader_id.index].mapping;
            let (Some(pos), Some(size)) = (mapping.rect_pos, mapping.rect_size) else {
                continue;
            };
            for instance in instances.chunks_exact(mapping.instances.total_slots) {
                let rect = Rect {
                    pos: dvec2(instance[pos] as f64, instance[pos + 1] as f64),
                    size: dvec2(instance[size] as f64, instance[size + 1] as f64),
                };
                assert!(
                    !rect.contains(outside),
                    "Popup submits no full-screen scrim over the underlying page: {rect:?}"
                );
                assert!(
                    rect.pos.x >= card.pos.x - 1.0
                        && rect.pos.y >= card.pos.y - 1.0
                        && rect.pos.x + rect.size.x <= card.pos.x + card.size.x + 1.0
                        && rect.pos.y + rect.size.y <= card.pos.y + card.size.y + 1.0,
                    "All popup drawing stays inside its card: {rect:?}, {card:?}"
                );
                primitives += 1;
            }
        }
        assert!(primitives > 0, "The card actually records visible drawing");
        let Area::Instance(underlay) = background.area() else {
            panic!("Underlying page must record a painted instance");
        };
        let item = &cx.draw_lists[underlay.draw_list_id].draw_items[underlay.draw_item_id];
        assert!(
            item.instances.as_ref().is_some_and(|data| !data.is_empty()),
            "Underlying page draw data remains recorded"
        );
        assert!(background.area().rect(&cx).contains(outside));
    }
}

/// The chat list's "New or Join" menu, built and drawn offscreen.
#[cfg(test)]
mod new_or_join_tests {
    use super::*;
    use crate::i18n::{text_for, Language};
    use crate::theme::{Appearance, Selection};
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    pub(super) const WINDOW: DVec2 = dvec2(1280.0, 760.0);
    /// The "+" beside the search field of a chat list column at its default
    /// width (300 points, right of the 76-point rail, 10-point margins).
    pub(super) const PLUS: Rect = Rect {pos: dvec2(331.0, 62.0), size: dvec2(35.0, 35.0)};
    /// Every command row the widget can show, whatever menu it is.
    pub(super) const ALL_ROWS: [LiveId; 19] = [
        id!(project_overview), id!(project_rooms), id!(project_members), id!(project_invite),
        id!(project_space), id!(new_project), id!(new_chat), id!(new_agent), id!(open_discover),
        id!(explore_rooms), id!(open_moments), id!(open_article), id!(open_browser),
        id!(plus_new_chat), id!(plus_new_room), id!(plus_new_space), id!(plus_join_room), id!(plus_create_agent), id!(plus_add_agent),
    ];
    /// The "+" menu's rows with no room open.
    fn add_rows() -> Vec<LiveId> {
        plus_menu::items(false).into_iter().map(PlusItem::row).collect()
    }

    /// A registered menu under one appearance and text scale (1.0 is the default).
    pub(super) fn menu(appearance: Appearance, text_scale: f64) -> (Cx, SidebarActionMenu) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let menu = cx.with_vm(|vm| {
            crate::theme::tests::install(vm, Selection { appearance, ..Default::default() });
            // The same theme value a large-text appearance package sets.
            let theme = vm.module(id!(theme));
            vm.bx.heap.set_value(theme, LiveId::from_str("octo_typography_scale").into(), text_scale.into(), NoTrap);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.SidebarActionMenu {}});
            let menu = SidebarActionMenu::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Menu registration: {errors:?}");
            menu
        });
        (cx, menu)
    }

    /// Draws the open menu over a window and returns the card's rect and every
    /// rect the card submitted to the GPU.
    fn draw(cx: &mut Cx, menu: &mut SidebarActionMenu) -> (Rect, Vec<Rect>) {
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
            cx2d.begin_root_turtle(WINDOW, Layout::flow_overlay());
            let walk = Walk {width: Size::Fixed(WINDOW.x), height: Size::Fixed(WINDOW.y), ..Walk::default()};
            menu.draw_walk_all(&mut cx2d, &mut Scope::empty(), walk);
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        let card = menu.view.view(cx, ids!(main_content)).area().rect(cx);
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
                drawn.push(Rect {
                    pos: dvec2(instance[pos] as f64, instance[pos + 1] as f64),
                    size: dvec2(instance[size] as f64, instance[size + 1] as f64),
                });
            }
        }
        (card, drawn)
    }

    pub(super) fn visible_rows(cx: &mut Cx, menu: &SidebarActionMenu) -> Vec<LiveId> {
        ALL_ROWS.into_iter().filter(|id| menu.view.button(cx, &[*id]).visible()).collect()
    }

    #[test]
    fn new_or_join_rows_are_full_targets_in_light_dark_and_large_text() {
        for (appearance, text_scale) in [(Appearance::Light, 1.0), (Appearance::Dark, 1.0), (Appearance::Light, 1.5), (Appearance::Dark, 2.0)] {
            let case = format!("{appearance:?} at {text_scale}x");
            let (mut cx, mut menu) = menu(appearance, text_scale);
            let theme = crate::theme::snapshot(&mut cx);
            let row_height = theme.control_height.max(24.0 * text_scale + 20.0);
            // Under the chat list's "+", and also low and far right, where it must be kept inside the window.
            for anchor in [new_or_join_menu_pos(&mut cx, PLUS), dvec2(1270.0, 750.0)] {
                menu.show(&mut cx, SidebarMenuKind::Add, anchor, Rect {pos: DVec2::default(), size: WINDOW});
                let (card, drawn) = draw(&mut cx, &mut menu);
                let expected = menu_size(&mut cx, SidebarMenuKind::Add);
                assert!((card.size - expected).length() < 1.0, "{case}: card {card:?} matches the size used to place it {expected:?}");
                assert!((card.size.x - 235.0 * text_scale).abs() < 1.0, "{case}: the card widens with the text: {card:?}");
                assert!(card.pos.x >= 0.0 && card.pos.y >= 0.0 && card.pos.x + card.size.x <= WINDOW.x + 0.5 && card.pos.y + card.size.y <= WINDOW.y + 0.5, "{case}: card stays in the window: {card:?}");
                if anchor.y < 400.0 {
                    // Clamped only on the left, where large text widens it past the window edge.
                    assert!((card.pos - dvec2(anchor.x.max(0.0), anchor.y)).length() < 1.0, "{case}: card opens at its anchor {anchor:?}: {card:?}");
                }
                for row in add_rows() {
                    let button = menu.view.button(&mut cx, &[row]);
                    let rect = button.area().rect(&cx);
                    assert!(rect.size.y >= 44.0 && (rect.size.y - row_height).abs() < 0.5 && rect.size.x >= 200.0, "{case}: {row:?} is a full target: {rect:?}");
                    assert!(rect.pos.y >= card.pos.y && rect.pos.y + rect.size.y <= card.pos.y + card.size.y + 0.5, "{case}: {row:?} is inside the card");
                    assert_eq!(button.borrow().unwrap().draw_icon.color, theme.accent, "{case}: icons follow the theme");
                }
                // Labels, icons and background: nothing spills past the card, even with large text.
                assert!(drawn.len() > add_rows().len());
                for rect in &drawn {
                    assert!(rect.pos.x >= card.pos.x - 1.0 && rect.pos.y >= card.pos.y - 1.0
                        && rect.pos.x + rect.size.x <= card.pos.x + card.size.x + 1.0
                        && rect.pos.y + rect.size.y <= card.pos.y + card.size.y + 1.0,
                        "{case}: drawing stays inside the card: {rect:?} in {card:?}");
                }
            }
        }
    }

    #[test]
    fn new_or_join_card_drops_down_from_the_plus_and_stays_in_the_window() {
        for text_scale in [1.0, 1.5, 2.0] {
            let (mut cx, mut menu) = menu(Appearance::Light, text_scale);
            // The default column, a column dragged out to the window's right edge,
            // and a small window, where large text makes the card wider than the
            // room left of the button.
            for (window, plus) in [
                (WINDOW, PLUS),
                (WINDOW, Rect {pos: dvec2(WINDOW.x - 45.0, 62.0), size: PLUS.size}),
                (dvec2(640.0, 420.0), Rect {pos: dvec2(201.0, 62.0), size: PLUS.size}),
            ] {
                let case = format!("{text_scale}x, + at {:?} in {window:?}", plus.pos);
                let pos = new_or_join_menu_pos(&mut cx, plus);
                menu.show(&mut cx, SidebarMenuKind::Add, pos, Rect {pos: DVec2::default(), size: window});
                let (card, _) = draw(&mut cx, &mut menu);
                assert!(card.pos.x >= 0.0 && card.pos.y >= 0.0 && card.pos.x + card.size.x <= window.x + 0.5 && card.pos.y + card.size.y <= window.y + 0.5,
                    "{case}: the card stays in the window: {card:?}");
                let below = plus.pos.y + plus.size.y + DROP_DOWN_GAP;
                if below + card.size.y <= window.y + 0.5 {
                    assert!((card.pos.y - below).abs() < 1.0, "{case}: just under the +: {card:?}");
                } else {
                    // Five rows at 2x text do not fit under the + of a 420-point window.
                    assert!((card.pos.y + card.size.y - window.y).abs() < 1.0, "{case}: as low as the window allows: {card:?}");
                }
                let right = plus.pos.x + plus.size.x;
                if card.size.x <= right {
                    assert!((card.pos.x + card.size.x - right).abs() < 1.0, "{case}: its right edge is the +'s: {card:?}");
                } else {
                    assert!(card.pos.x.abs() < 0.5, "{case}: as far left as the window allows: {card:?}");
                }
            }
        }
    }
}
