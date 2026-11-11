//! Mobile room information backed by the current Matrix room and push rules.

use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use matrix_sdk::{
    notification_settings::RoomNotificationMode,
    ruma::{OwnedUserId, OwnedRoomId},
    RoomMemberships,
};

use crate::{
    home::{invite_modal::InviteModalAction, rooms_list::RoomNotificationModeUpdated},
    logout::logout_confirm_modal::LogoutAction,
    profile::user_profile::{ShowUserProfileAction, UserProfile, UserProfileAndRoomId},
    shared::{
        avatar::{AvatarState, AvatarWidgetRefExt},
        navigation_bar_button::{NavigationBarButtonWidgetRefExt, NavigationBarButtonWidgetExt},
    },
    sliding_sync::{
        current_user_id, get_client, spawn_async_task, submit_async_request, MatrixRequest,
    },
    utils::{self, RoomNameId},
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.MobileChatInfo = #(MobileChatInfo::register_widget(vm)) {
        ..mod.widgets.SolidView
        width: Fill height: Fill flow: Down
        draw_bg.color: mod.widgets.MOBILE_BG
        summary := View {width: Fill height: Fit flow: Down padding: 16 spacing: 8
            summary_details := View {width: Fill height: Fit flow: Down spacing: 8
                summary_name := Label {width: Fill height: Fit max_lines: 2 text_overflow: Ellipsis
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (15 * mod.widgets.RINX_TEXT_SCALE)}}}
                View {width: Fill height: 32 flow: Right spacing: 8 align: Align{y: 0.5}
                    summary_count := Label {width: Fill height: Fit draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
                    history_search := NavigationBarButton {width: 32 height: 32 padding: 7 tooltip_text: #(crate::i18n::tr("Search Chat History"))
                        Icon {icon_walk: Walk{width: 16 height: 16} draw_icon +: {svg: ICON_SEARCH color: mod.widgets.RINX_MUTED}}}
                    history_media := NavigationBarButton {width: 32 height: 32 padding: 7 tooltip_text: #(crate::i18n::tr("Shared Attachments"))
                        Icon {icon_walk: Walk{width: 16 height: 16} draw_icon +: {svg: ICON_ADD_ATTACHMENT color: mod.widgets.RINX_MUTED}}}
                }
            }
            View {width: Fill height: 36 flow: Right spacing: 8
                overview_tab := RobrixNeutralIconButton {width: Fill height: 36 text: #(crate::i18n::tr("Overview")) icon_walk: Walk{width: 0 height: 0}}
                members_tab := RobrixNeutralIconButton {width: Fill height: 36 text: #(crate::i18n::tr("Members")) icon_walk: Walk{width: 0 height: 0}}
            }
        }
        status := Label {
            width: Fill height: Fit margin: 16
            flow: Flow.Right{wrap: true}
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        retry := RobrixNeutralIconButton {visible: false text: #(crate::i18n::tr("Retry")) i18n_text: "Retry" height: 44 margin: 12}
        services_panel := View {visible: false width: Fill height: Fit flow: Down padding: Inset{left: 16 right: 16 bottom: 12} spacing: 10
            services_back := RinxBackButton {}
            Label {width: Fill height: Fit text: #(crate::i18n::tr("Room Agent services")) i18n_text: "Room Agent services"
                draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
            Label {width: Fill height: Fit flow: Flow.Right{wrap: true}
                text: #(crate::i18n::tr("Only your Agents connected to this Room are shown. Service settings apply to the selected Agent across its connected Rooms."))
                i18n_text: "Only your Agents connected to this Room are shown. Service settings apply to the selected Agent across its connected Rooms."
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}}
            services_status := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_MUTED}
            room_services := RobrixSettingsDropDown {width: Fill height: 40 margin: 0 labels: ["Choose an Agent"]
                popup_menu_position: PopupMenuPosition.BelowInput
                popup_menu: RobrixSettingsPopupMenu {width: 256 menu_item: RobrixSettingsPopupMenuItem {height: 36}}}
            View {width: Fill height: 36 flow: Right spacing: 8
                configure_service := RobrixNeutralIconButton {text: #(crate::i18n::tr("Open Service")) i18n_text: "Open Service" height: 36 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} width: Fill icon_walk: Walk{width: 0 height: 0}}
                refresh_services := RobrixNeutralIconButton {text: #(crate::i18n::tr("Refresh")) height: 36 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}}
            }
        }
        member_controls := View {visible: false width: Fill height: Fit flow: Down padding: Inset{left: 12 right: 12 bottom: 12} spacing: 8
            View {width: Fill height: 32 flow: Right spacing: 6
                invite_people := RobrixNeutralIconButton {text: #(crate::i18n::tr("Invite people")) i18n_text: "Invite people" width: Fill height: 32 padding: 6 spacing: 5
                    icon_walk: Walk{width: 14 height: 14} draw_icon.svg: ICON_ADD_USER}
                invite_my_agent := RobrixNeutralIconButton {text: #(crate::i18n::tr("Add agent")) i18n_text: "Add agent" width: Fill height: 32 padding: 6 spacing: 5
                    icon_walk: Walk{width: 14 height: 14} draw_icon.svg: ICON_ROBOT}
            }
            members_empty := Label {visible: false width: Fill height: Fit text: #(crate::i18n::tr("No members available.")) draw_text.color: mod.widgets.RINX_MUTED}
        }
        ownership_status := Label {visible: false width: Fill height: Fit margin: Inset{left: 16 right: 16 bottom: 8}
            flow: Flow.Right{wrap: true}
            text: #(crate::i18n::tr("Some Agent ownership could not be verified. Unmatched members are shown separately."))
            i18n_text: "Some Agent ownership could not be verified. Unmatched members are shown separately."
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}}
        information_list := View {width: Fill height: Fill
            list := PortalList {
                width: Fill height: Fill flow: Down
                Header := mod.widgets.MobileSection {
                    padding: Inset{left: 16 right: 16 top: 4 bottom: 12} spacing: 6 margin: 0
                    Label {width: Fill text: #(crate::i18n::tr("Room details")) draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
                    topic := Label {width: Fill height: Fit flow: Flow.Right{wrap: true}
                        draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
                    View {width: Fill height: 28 flow: Right spacing: 6 align: Align{y: 0.5}
                        room_id := Label {width: Fill height: Fit max_lines: 1 text_overflow: Ellipsis
                            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}}
                        copy_room_id := NavigationBarButton {width: 28 height: 28 padding: 6 tooltip_text: #(crate::i18n::tr("Copy Room ID"))
                            Icon {icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: ICON_COPY color: mod.widgets.RINX_MUTED}}}
                    }
                }
                Member := mod.widgets.MobileSection {
                    // Clicking a member opens their profile (DM, block, ...).
                    row := NavigationBarButton {
                        width: Fill height: 56 flow: Right spacing: 8 padding: Inset{left: 16 right: 16}
                        align: Align{y: 0.5}
                        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_HOVER border_radius: 0}
                        avatar := mod.widgets.MobileAvatar {width: 32 height: 32}
                        View {
                            width: Fill height: Fit flow: Down spacing: 2 padding: 0
                            name := Label {
                                width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                                draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                            }
                            user_id := Label {
                                width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
                            }
                        }
                    }
                }
                Back := mod.widgets.MobileSection {
                    padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
                    row := RinxBackButton {}
                }
                Action := mod.widgets.MobileSection {
                    row := NavigationBarButton {
                        width: Fill height: 56 flow: Right spacing: 10
                        padding: Inset{left: 20 right: 20} align: Align{y: 0.5}
                        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_HOVER border_radius: 0}
                        title := Label {
                            width: Fill max_lines: 1 text_overflow: Ellipsis
                            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}}
                        }
                        value := Label {
                            max_lines: 1
                            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                        }
                        Icon {
                            icon_walk: Walk{width: 8 height: 13}
                            draw_icon +: {color: mod.widgets.RINX_DISABLED svg: ICON_CHEVRON_RIGHT}
                        }
                    }
                    mod.widgets.MobileDivider {margin: Inset{left: 20}}
                }
                Filler := View {height: 100 width: Fill}
            }
        }
        agent_picker_modal := Modal {
            content := SmallModal {
                title := ModalTitle {text: #(crate::i18n::tr("Add agent")) i18n_text: "Add agent"}
                agent_room := ModalBody {margin: Inset{bottom: 12}}
                my_agents := RobrixSettingsDropDown {width: Fill height: 40 margin: 0 labels: ["Choose my Agent"]
                    draw_bg.border_radius: 4
                    popup_menu_position: PopupMenuPosition.BelowInput
                    draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
                    popup_menu: RobrixSettingsPopupMenu {width: 310 menu_item: RobrixSettingsPopupMenuItem {height: 36}}}
                ModalBody {margin: Inset{top: 12}
                    text: #(crate::i18n::tr("Only available Agents are listed. If a pool Agent is missing, check its resource authorization in Agent → My Agents."))
                    i18n_text: "Only available Agents are listed. If a pool Agent is missing, check its resource authorization in Agent → My Agents."
                    draw_text.color: mod.widgets.RINX_MUTED
                }
                buttons_view := ModalButtonsRow {
                    close_agent_picker := RobrixNeutralIconButton {
                        width: Fit{min: FitBound.Abs(120.0)} height: 44 align: Align{x: 0.5 y: 0.5} padding: 12
                        text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"
                        draw_icon.svg: ICON_FORBIDDEN icon_walk: Walk{width: 16 height: 16 margin: Inset{left: -2 right: -1}}
                    }
                    invite_selected_agent := RobrixPositiveIconButton {
                        width: Fit{min: FitBound.Abs(120.0)} height: 44 align: Align{x: 0.5 y: 0.5} padding: 12
                        text: #(crate::i18n::tr("Add agent")) i18n_text: "Add agent"
                        draw_icon.svg: ICON_ROBOT icon_walk: Walk{width: 16 height: 16 margin: Inset{left: -2 right: -1}}
                    }
                }
                agent_status := ModalBody {}
                resume_agent_requests := RobrixNeutralIconButton {visible: false text: #(crate::i18n::tr("Resume original request in Agents")) i18n_text: "Resume original request in Agents" height: 40 margin: Inset{top: 12} icon_walk: Walk{width: 0 height: 0}}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InfoTab { #[default] Overview, Members }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InfoRow { Overview, Member(usize), Notifications, Mode(usize), Back, AgentServices, Filler }
fn info_rows(tab: InfoTab, members: usize, choosing: bool) -> usize {
    match tab {InfoTab::Members => members, InfoTab::Overview if choosing => 6, InfoTab::Overview => 3}
}
fn info_row(tab: InfoTab, members: usize, choosing: bool, index: usize) -> InfoRow {
    if index >= info_rows(tab, members, choosing) {return InfoRow::Filler;}
    match tab {
        InfoTab::Members => InfoRow::Member(index),
        InfoTab::Overview if index == 0 => InfoRow::Overview,
        InfoTab::Overview if choosing => if index <= 4 {InfoRow::Mode(index - 1)} else {InfoRow::Back},
        InfoTab::Overview => match index {1 => InfoRow::Notifications, 2 => InfoRow::AgentServices, _ => InfoRow::Filler},
    }
}
#[derive(Clone, Debug)]
pub(crate) struct ChatMembershipChanged {
    pub room_id: OwnedRoomId,
    pub epoch: u64,
}

#[derive(Clone, Debug)]
struct ChatInfoData {
    room: RoomNameId,
    topic: String,
    members: Vec<UserProfile>,
    banned_members: HashSet<OwnedUserId>,
    mode: Option<RoomNotificationMode>,
}


#[derive(Debug)]
struct MemberOwnershipLoaded {
    owner: OwnedUserId, origin: String, epoch: u64, room_id: OwnedRoomId, request: u64,
    result: Result<(HashMap<OwnedUserId, OwnedUserId>, bool), String>,
}
fn add_member_owners(owners: &mut HashMap<OwnedUserId, OwnedUserId>, conflicts: &mut HashSet<OwnedUserId>, rows: &serde_json::Value, fallback_owner: Option<&OwnedUserId>) {
    for row in rows.as_array().into_iter().flatten() {
        let Some(agent) = row["puppetMxid"].as_str().and_then(|id| OwnedUserId::try_from(id).ok()) else {continue;};
        let owner = row["ownerMxid"].as_str().and_then(|id| OwnedUserId::try_from(id).ok()).or_else(|| fallback_owner.cloned());
        let Some(owner) = owner else {continue;};
        if agent == owner || agent.localpart() == "_hagency_service" || conflicts.contains(&agent) {continue;}
        if owners.get(&agent).is_some_and(|old| old != &owner) {
            owners.remove(&agent);conflicts.insert(agent);
        } else {owners.insert(agent,owner);}
    }
}
fn group_members(members: &mut Vec<UserProfile>, owners: &HashMap<OwnedUserId, OwnedUserId>) -> HashSet<OwnedUserId> {
    members.sort_by_key(|p| (p.displayable_name().to_lowercase(), p.user_id.clone()));
    let present: HashSet<_> = members.iter().map(|p|p.user_id.clone()).collect();
    let grouped: HashSet<_> = members.iter().filter(|p|p.user_id.localpart() != "_hagency_service" && owners.get(&p.user_id).is_some_and(|owner| present.contains(owner) && !owners.contains_key(owner) && owner.localpart() != "_hagency_service" && *owner != p.user_id)).map(|p|p.user_id.clone()).collect();
    let mut ordered = Vec::with_capacity(members.len());
    for member in members.iter().filter(|p|!grouped.contains(&p.user_id)) {
        ordered.push(member.clone());
        ordered.extend(members.iter().filter(|p|grouped.contains(&p.user_id) && owners.get(&p.user_id) == Some(&member.user_id)).cloned());
    }
    *members = ordered;
    grouped
}
async fn fetch_member_owners(room: &str, owner: &OwnedUserId) -> Result<(HashMap<OwnedUserId, OwnedUserId>, bool), String> {
    use crate::hagency::{backend::Service,model::Command};
    let service = Service::for_current()?;
    let roster = service.execute(Command::Agents).await?;
    if roster["ownerMxid"].as_str() != Some(owner.as_str()) {return Err("matrix_account_changed".into());}
    let rows=roster["agents"].as_array().ok_or("room_discovery_unavailable")?;
    if rows.len()>128 {return Err("room_discovery_incomplete".into());}
    let mut owners = HashMap::new();let mut conflicts = HashSet::new();
    add_member_owners(&mut owners, &mut conflicts, &roster["agents"], Some(owner));
    let incomplete = match service.agents_for_registered_room(room).await {
        Ok(context) => {
            let project = context["projectId"].as_str().filter(|id|!id.is_empty()).ok_or("room_discovery_unavailable")?;
            match service.execute(Command::RoomRoster {project_id:project.into(),room_id:room.into()}).await {
                Ok(roster) if roster["agents"].as_array().is_some_and(|rows|rows.len()<=128) => {add_member_owners(&mut owners,&mut conflicts,&roster["agents"],None);false},
                Ok(_) => true,
                Err(_) => true,
            }
        },
        // Private/non-project Rooms can still group this owner's known Agents.
        Err(error) if error == "room_project_registration_required" => false,
        Err(_) => true,
    };
    Ok((owners,incomplete || !conflicts.is_empty()))
}

#[derive(Debug)]
struct RoomAgentsLoaded {
    owner: OwnedUserId,
    origin: String,
    epoch: u64,
    room_id: OwnedRoomId,
    request: u64,
    binding: bool,
    result: Result<serde_json::Value, String>,
}
#[derive(Debug)]
struct RoomAgentsSlow {
    owner: OwnedUserId, origin: String, epoch: u64, room_id: OwnedRoomId,
    request: u64, binding: bool,
}
impl RoomAgentsSlow {
    fn call(&self) -> RoomAgentsLoaded {
        RoomAgentsLoaded {owner:self.owner.clone(),origin:self.origin.clone(),epoch:self.epoch,
            room_id:self.room_id.clone(),request:self.request,binding:self.binding,result:Err(String::new())}
    }
}

pub(super) fn agent_binding_error(code: &str) -> &'static str {
    match code {
        "room_project_registration_required" => "To chat with Agents, use an Agent space. Open + → Create a space or room, choose Agent space, then create a room inside it.",
        "room_join_required" | "room_membership_required" => "Join this room and its Agent space before adding an Agent.",
        "service_invitation_denied" | "agent_service_invite_permission_required" => "The Hagency service needs permission to invite Agents. Ask a room administrator to allow it to invite members, then retry.",
        "agent_service_banned" => "The Hagency service is banned from this room. Ask a room administrator to unban it, then retry.",
        "matrix_permission_denied" => "You do not have permission to invite members to this room. Ask a room administrator to enable invitations.",
        "agent_creation_denied" => "This space or room does not allow you to add Agents. Ask its owner to update the Agent permissions.",
        "agent_owner_binding_required" => "This Agent is not available to add. Check that it belongs to you and its resource authorization is still valid in Agent → My Agents.",
        "agent_retired" => "This Agent has been retired and cannot join another room.",
        "agent_command_recovery_required" => "An original binding request needs confirmation. Resume it in Agents.",
        "sign_in_required" | "matrix_account_changed" => "Your account session changed. Reopen this room after signing in.",
        "invalid_server_response" => "The server returned an invalid binding result. Retry to check the original request.",
        "agent_binding_suspended" => "The Agent could not join this room. Check its membership and room permissions, then check the request again.",
        _ => "Could not confirm the binding. Retry to check the original request; no duplicate Agent will be created.",
    }
}
#[derive(Debug)]
struct ChatInfoLoaded {
    owner: OwnedUserId,
    origin: String,
    epoch: u64,
    room_id: OwnedRoomId,
    request: u64,
    result: Result<ChatInfoData, String>,
}

#[derive(Debug)]
struct RoomServicesLoaded {
    owner: OwnedUserId,
    origin: String,
    epoch: u64,
    room_id: OwnedRoomId,
    request: u64,
    result: Result<serde_json::Value, String>,
}

fn room_service_status(service: &serde_json::Value) -> String {
    if service["ownerServicePaused"] == true {return crate::i18n::tr("Paused in this Room. Requests receive a pause notice.").into();}
    if service["bindingState"] == "suspended" {return crate::i18n::tr("Room access unavailable. Check membership and permissions.").into();}
    if service["bindingState"] == "pending" {return crate::i18n::tr("Joining this Room…").into();}
    if service["thisDevice"] != true {return crate::i18n::tr("Not assigned to this device.").into();}
    if service["statusAvailable"] != true {return crate::i18n::tr("Could not verify this Room's service. Refresh to retry.").into();}
    crate::hagency::ui::runtime_summary(&service["runtime"], service["agentId"].as_str().unwrap_or(""), service["bindingId"].as_str())
}

#[derive(Script, ScriptHook, Widget)]
pub struct MobileChatInfo {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    owner: Option<OwnedUserId>,
    #[rust]
    origin: String,
    #[rust]
    epoch: u64,
    #[rust]
    room: Option<RoomNameId>,
    #[rust]
    request: u64,
    #[rust]
    data: Option<ChatInfoData>,
    #[rust] member_owners: HashMap<OwnedUserId, OwnedUserId>,
    #[rust] indented_members: HashSet<OwnedUserId>,
    #[rust] ownership_unavailable: bool,
    #[rust]
    status: String,
    #[rust]
    failed: bool,
    #[rust]
    choosing_notifications: bool,
    #[rust]
    tab: InfoTab,
    #[rust] drawn_tab: InfoTab,
    #[rust] rendered_rows: HashMap<WidgetUid, InfoRow>,
    #[rust] rendered_members: HashMap<WidgetUid, UserProfile>,
    #[rust] agent_request: u64,
    #[rust] agent_busy: bool,
    #[rust] agent_added: bool,
    #[rust] agent_pending: bool,
    #[rust] agent_slow: bool,
    #[rust] agent_project: String,
    #[rust] agents: Vec<serde_json::Value>,
    #[rust] agent_command: Option<(String, String)>,
    #[rust] agent_picker_open: bool,
    #[rust] services_open: bool,
    #[rust] services_busy: bool,
    #[rust] services_request: u64,
    #[rust] services: Vec<serde_json::Value>,
    #[cfg(feature = "onboarding-test-support")]
    #[rust] review_mode: bool,
}

fn is_hagency_service(user: &matrix_sdk::ruma::UserId, owner: &matrix_sdk::ruma::UserId) -> bool {
    user.localpart() == "_hagency_service" && user.server_name() == owner.server_name()
}

fn modes() -> [(Option<RoomNotificationMode>, &'static str); 4] {
    [
        (None, crate::i18n::tr("Use Account Defaults")),
        (
            Some(RoomNotificationMode::AllMessages),
            crate::i18n::tr("All Messages"),
        ),
        (
            Some(RoomNotificationMode::MentionsAndKeywordsOnly),
            crate::i18n::tr("Mentions and Keywords"),
        ),
        (
            Some(RoomNotificationMode::Mute),
            crate::i18n::tr("Mute Notifications"),
        ),
    ]
}

impl MobileChatInfo {
    fn services_response_matches(&self, loaded: &RoomServicesLoaded) -> bool {
        self.owner.as_ref() == Some(&loaded.owner) && self.origin == loaded.origin
            && self.epoch == loaded.epoch && self.services_request == loaded.request
            && self.room.as_ref().is_some_and(|room|room.room_id() == &loaded.room_id)
            && loaded.result.as_ref().map_or(true, |v|v["roomId"] == loaded.room_id.as_str())
    }
    fn load_services(&mut self, cx: &mut Cx) {
        if self.services_busy || !self.pin_is_current() {return;}
        let (Some(owner), Some(room)) = (self.owner.clone(), self.room.clone()) else {return;};
        let Ok(service) = crate::hagency::backend::Service::for_current() else {
            self.services.clear();
            self.view.drop_down(cx, ids!(room_services)).set_labels(cx, vec![crate::i18n::tr("Choose an Agent").into()]);
            self.view.drop_down(cx, ids!(room_services)).set_selected_item(cx, 0);
            self.view.label(cx, ids!(services_status)).set_text(cx, crate::i18n::tr("Could not load Agent services. Refresh to retry."));
            self.view.redraw(cx);
            return;
        };
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.services_request = request;
        self.services_busy = true;
        self.services_open = true;
        self.services.clear();
        self.view.drop_down(cx, ids!(room_services)).set_labels(cx, vec![crate::i18n::tr("Choose an Agent").into()]);
        self.view.drop_down(cx, ids!(room_services)).set_selected_item(cx, 0);
        self.view.label(cx, ids!(services_status)).set_text(cx, crate::i18n::tr("Checking Agent services in this Room…"));
        let origin = self.origin.clone(); let epoch = self.epoch; let room_id = room.room_id().clone();
        spawn_async_task(async move {
            let result = service.room_agent_services(room_id.as_str()).await;
            Cx::post_action(RoomServicesLoaded {owner, origin, epoch, room_id, request, result});
        });
        self.view.redraw(cx);
    }
    fn select_tab(&mut self, cx: &mut Cx, tab: InfoTab) {
        self.close_agent_picker(cx);
        self.tab = tab;
        self.choosing_notifications = false;
        self.services_open = false;
        self.view.portal_list(cx, ids!(list)).set_first_id_and_scroll(0, 0.0);
        // Tabs change layout and controls in draw_walk, outside the cached
        // children that a partial View redraw would update.
        cx.redraw_all();
    }
    fn load(&mut self, cx: &mut Cx, room: RoomNameId) {
        self.clear();
        self.fetch_info(cx, room);
    }
    // Refresh metadata without clearing an in-flight Agent invitation or its original key.
    fn fetch_info(&mut self, cx: &mut Cx, room: RoomNameId) {
        self.room = Some(room.clone());
        let epoch = crate::account_session::epoch();
        let Some(client) = get_client() else { return };
        let Some(owner) = client.user_id().map(ToOwned::to_owned) else {
            return;
        };
        if epoch != crate::account_session::epoch() || crate::sliding_sync::account_changing() {
            return;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.request = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.owner = Some(owner.clone());
        let request = self.request;
        self.origin = client.homeserver().to_string();
        self.epoch = epoch;
        let origin = self.origin.clone();
        let epoch = self.epoch;
        let room_id = room.room_id().clone();
        self.member_owners.clear();self.indented_members.clear();self.ownership_unavailable=false;
        if let Some(data)=self.data.as_mut() {group_members(&mut data.members,&self.member_owners);}
        let ownership_owner=owner.clone();let ownership_origin=origin.clone();let ownership_room=room_id.clone();
        spawn_async_task(async move {
            let result=tokio::time::timeout(std::time::Duration::from_secs(30),fetch_member_owners(ownership_room.as_str(),&ownership_owner)).await.unwrap_or_else(|_|Err("room_discovery_unavailable".into()));
            Cx::post_action(MemberOwnershipLoaded {owner:ownership_owner,origin:ownership_origin,epoch,room_id:ownership_room,request,result});
        });
        self.status = crate::i18n::tr("Loading chat information…").into();
        self.view
            .portal_list(cx, ids!(list))
            .set_first_id_and_scroll(0, 0.0);
        self.view.redraw(cx);
        spawn_async_task(async move {
            let result = async {
                let room = client
                    .get_room(room.room_id())
                    .ok_or(crate::i18n::tr("This chat is no longer available."))?;
                let members = room
                    .members(RoomMemberships::JOIN | RoomMemberships::BAN)
                    .await
                    .map_err(|e| e.to_string())?;
                let banned_members = members.iter().filter(|m| *m.membership() == ruma::events::room::member::MembershipState::Ban)
                    .map(|m| m.user_id().to_owned()).collect();
                let mut members: Vec<_> = members
                    .into_iter()
                    .map(|member| UserProfile {
                        user_id: member.user_id().to_owned(),
                        username: member.display_name().map(ToOwned::to_owned),
                        avatar_state: AvatarState::Known(
                            member.avatar_url().map(ToOwned::to_owned),
                        ),
                    })
                    .collect();
                members.sort_by_key(|p| (p.displayable_name().to_lowercase(), p.user_id.clone()));
                Ok(ChatInfoData {
                    room: RoomNameId::from_room(&room).await,
                    topic: room.topic().unwrap_or_default(),
                    members,
                    banned_members,
                    mode: room.user_defined_notification_mode().await,
                })
            }
            .await;
            Cx::post_action(ChatInfoLoaded {
                owner,
                origin,
                epoch,
                room_id,
                request,
                result,
            });
        });
    }

    fn pin_is_current(&self) -> bool {
        #[cfg(feature = "onboarding-test-support")]
        if self.review_mode {return get_client().is_none();}
        self.owner == current_user_id()
            && self.epoch == crate::account_session::epoch()
            && !crate::sliding_sync::account_changing()
            && get_client().is_some_and(|client| client.homeserver().as_str() == self.origin)
    }
    fn accepts(&self, loaded: &ChatInfoLoaded) -> bool {
        self.pin_is_current() && self.matches_response(loaded)
    }
    fn matches_response(&self, loaded: &ChatInfoLoaded) -> bool {
        self.owner.as_ref() == Some(&loaded.owner)
            && self.origin == loaded.origin
            && self.epoch == loaded.epoch
            && self.request == loaded.request
            && self
                .room
                .as_ref()
                .is_some_and(|room| room.room_id() == &loaded.room_id)
            && match &loaded.result {
                Ok(data) => data.room.room_id() == &loaded.room_id,
                Err(_) => true,
            }
    }
    fn ownership_response_matches(&self, loaded: &MemberOwnershipLoaded) -> bool {
        self.owner.as_ref()==Some(&loaded.owner) && self.origin==loaded.origin && self.epoch==loaded.epoch && self.request==loaded.request
            && self.room.as_ref().is_some_and(|room|room.room_id()==&loaded.room_id)
    }
    fn agent_call_matches(&self, loaded: &RoomAgentsLoaded) -> bool {
        self.owner.as_ref() == Some(&loaded.owner) && self.origin == loaded.origin && self.epoch == loaded.epoch && self.agent_request == loaded.request
            && self.room.as_ref().is_some_and(|room| room.room_id() == &loaded.room_id)
    }
    fn agent_response_matches(&self, loaded: &RoomAgentsLoaded) -> bool {
        self.agent_call_matches(loaded)
            && loaded.result.as_ref().map_or(true, |value| if loaded.binding {value["creation"]["binding"]["roomId"] == loaded.room_id.as_str()} else {value["roomId"] == loaded.room_id.as_str()})
    }
    fn close_agent_picker(&mut self, cx: &mut Cx) {
        let modal = self.view.modal(cx, ids!(agent_picker_modal));
        if self.agent_picker_open || modal.is_open() {
            self.agent_picker_open = false;
            // Modal dismissal does not close a dropdown's separate popup.
            // Release its sweep lock before hiding the containing modal.
            if let Some(mut dropdown) = self.view.drop_down(cx, ids!(my_agents)).borrow_mut() {
                dropdown.set_closed(cx);
            }
            modal.close(cx);
            cx.redraw_all();
        }
    }
    fn show_agent_slow(&mut self, cx: &mut Cx, binding: bool) {
        self.agent_slow=true;
        self.view.label(cx,ids!(agent_status)).set_text(cx,crate::i18n::tr(if binding {
            "Still waiting for the server. You can close this panel; the original request will continue."
        } else {"Checking is taking longer than expected. You can close this panel and return later."}));
        self.view.redraw(cx);
    }
    fn finish_agent_request(&mut self, cx: &mut Cx, loaded: &RoomAgentsLoaded) {
        // A response for this exact call always releases its waiting state.
        // A malformed/cross-room payload is an error, not an ignored completion.
        self.agent_busy=false;
        self.agent_slow=false;
        self.agent_added=false;
        self.agent_pending=false;
        self.view.button(cx,ids!(resume_agent_requests)).set_visible(cx,false);
        let result = if self.agent_response_matches(loaded) {loaded.result.clone()}
            else {Err("invalid_server_response".into())};
        let mut error = None;
        let message = match &result {
            Ok(value) if loaded.binding => match value["commandState"].as_str() {
                Some("active") if value["creation"]["binding"]["state"] == "active" => {
                    self.agent_added=true;
                    "Agent added. In a group, @ the Agent to request a reply."
                }
                Some("pending") => {
                    self.agent_pending=true;
                    "The request is saved. The Agent is still joining. You can close this panel or check its status."
                }
                Some("suspended") => {error=Some("agent_binding_suspended");agent_binding_error("agent_binding_suspended")}
                _ => {error=Some("invalid_server_response");agent_binding_error("invalid_server_response")}
            },
            Ok(value) => {
                self.agent_project=value["projectId"].as_str().unwrap_or("").into();
                self.agents=value["agents"].as_array().map(|rows|rows.iter().filter(|a|a["state"] == "active").cloned().collect()).unwrap_or_default();
                let labels=std::iter::once(crate::i18n::tr("Choose my Agent").to_owned()).chain(self.agents.iter().map(|a|a["displayName"].as_str().filter(|name|!name.is_empty()).unwrap_or("Agent").to_owned())).collect();
                self.view.drop_down(cx,ids!(my_agents)).set_labels(cx,labels);
                self.view.drop_down(cx,ids!(my_agents)).set_selected_item(cx,0);
                if self.agents.is_empty() {"No active Agents owned by you are available."}
                else {"Choose an Agent you own to bind to this Room."}
            },
            Err(code) => {error=Some(code.as_str());agent_binding_error(code)},
        };
        let mut label=self.view.label(cx,ids!(agent_status));
        label.set_text(cx,crate::i18n::tr(message));
        if error.is_some() {script_apply_eval!(cx,label,{draw_text +: {color: mod.widgets.RINX_DANGER_FG}});}
        else if self.agent_added {script_apply_eval!(cx,label,{draw_text +: {color: mod.widgets.RINX_SUCCESS_FG}});}
        else {script_apply_eval!(cx,label,{draw_text +: {color: mod.widgets.RINX_MUTED}});}
        self.view.button(cx,ids!(resume_agent_requests)).set_visible(cx,error==Some("agent_command_recovery_required"));
        cx.redraw_all();
    }
    fn request_agents(&mut self, cx: &mut Cx, binding: bool) {
        if self.agent_busy || !self.pin_is_current() {return;}
        let (Some(owner),Some(room)) = (self.owner.clone(),self.room.clone()) else {return;};
        let service = crate::hagency::backend::Service::for_current().ok();
        #[cfg(feature = "onboarding-test-support")]
        let offline_review = self.review_mode && get_client().is_none();
        #[cfg(not(feature = "onboarding-test-support"))]
        let offline_review = false;
        if service.is_none() && !offline_review {return;}
        let command = if binding {
            let Some(index) = self.view.drop_down(cx,ids!(my_agents)).selected_item().checked_sub(1) else {return;};
            let Some(agent) = self.agents.get(index).and_then(|a| a["id"].as_str()) else {return;};
            if self.agent_project.is_empty() {return;}
            let fingerprint = serde_json::to_string(&(agent,&self.agent_project,room.room_id())).unwrap_or_default();
            if self.agent_command.as_ref().is_none_or(|(old,_)|old != &fingerprint) {self.agent_command=Some((fingerprint,uuid::Uuid::new_v4().to_string()));}
            Some(crate::hagency::model::Command::BindAgent {agent_id:agent.into(),input:serde_json::json!({"projectId":self.agent_project,"roomId":room.room_id(),"idempotencyKey":self.agent_command.as_ref().unwrap().1})})
        } else {None};
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.agent_request=NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        let request=self.agent_request;
        let origin=self.origin.clone();
        let epoch=self.epoch;
        let room_id=room.room_id().clone();
        self.agent_busy=true;
        self.agent_slow=false;
        self.agent_added=false;
        self.agent_pending=false;
        self.tab=InfoTab::Members;
        self.agent_picker_open=true;
        self.view.label(cx, ids!(agent_room)).set_text(cx, &room.to_string());
        self.view.modal(cx, ids!(agent_picker_modal)).open(cx);
        self.view.label(cx,ids!(agent_status)).set_text(cx,crate::i18n::tr(if binding {"Adding Agent to this Room…"} else {"Checking this Room and your Agents…"}));
        let mut status = self.view.label(cx,ids!(agent_status));
        script_apply_eval!(cx,status,{draw_text +: {color: mod.widgets.RINX_MUTED}});
        self.view.button(cx,ids!(resume_agent_requests)).set_visible(cx,false);
        if !binding {self.view.button(cx, ids!(resume_agent_requests)).set_visible(cx, false);self.agents.clear(); self.agent_project.clear(); self.view.drop_down(cx,ids!(my_agents)).set_labels(cx,vec![crate::i18n::tr("Choose my Agent").into()]);}
        if let Some(service) = service {spawn_async_task(async move {
            let future = async {
                if let Some(command)=command {service.bind_agent_to_room(command).await}
                else {service.agents_for_registered_room(room_id.as_str()).await}
            };
            tokio::pin!(future);
            let result = tokio::select! {
                result = &mut future => result,
                _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {
                    Cx::post_action(RoomAgentsSlow {owner:owner.clone(),origin:origin.clone(),epoch,room_id:room_id.clone(),request,binding});
                    future.await
                }
            };
            Cx::post_action(RoomAgentsLoaded {owner,origin,epoch,room_id:room_id.clone(),request,binding,result});
        });}
        cx.redraw_all();
    }
    fn clear(&mut self) {
        self.owner = None;
        self.origin.clear();
        self.epoch = 0;
        self.room = None;
        self.request = 0;
        self.data = None;
        self.member_owners.clear();self.indented_members.clear();self.ownership_unavailable=false;
        self.status.clear();
        self.failed = false;
        self.choosing_notifications = false;
        self.tab = InfoTab::Overview;
        self.rendered_rows.clear(); self.rendered_members.clear();
        self.agent_request = 0;
        self.agent_busy = false;
        self.agent_added = false;
        self.agent_pending = false;
        self.agent_slow = false;
        self.agent_project.clear();
        self.agents.clear();
        self.agent_command = None;
        self.agent_picker_open = false;
        self.services_open = false;
        self.services_busy = false;
        self.services_request = 0;
        self.services.clear();
    }
}

impl Widget for MobileChatInfo {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.owner.is_some() && !self.pin_is_current() {
            self.clear();
        }
        if matches!(event, Event::Signal) {
            crate::avatar_cache::process_avatar_updates(cx);
            self.view.redraw(cx);
        }
        if !self.agent_picker_open {
            self.close_agent_picker(cx);
        }
        // Let the overlay consume input before the underlying members panel.
        if self.agent_picker_open && !matches!(event, Event::Actions(_) | Event::Signal) {
            self.view.modal(cx, ids!(agent_picker_modal)).handle_event(cx, event, scope);
        } else {
            self.view.handle_event(cx, event, scope);
        }
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(loaded) = action.downcast_ref::<RoomServicesLoaded>()
                    && self.pin_is_current() && self.services_response_matches(loaded) {
                    self.services_busy = false;
                    match &loaded.result {
                        Ok(value) => {
                            self.services = value["services"].as_array().cloned().unwrap_or_default();
                            let labels = std::iter::once(crate::i18n::tr("Choose an Agent").to_owned()).chain(self.services.iter().map(|s|s["displayName"].as_str().filter(|n|!n.is_empty()).unwrap_or("Agent").to_owned())).collect();
                            self.view.drop_down(cx, ids!(room_services)).set_labels(cx, labels);
                            let selected = usize::from(!self.services.is_empty());
                            self.view.drop_down(cx, ids!(room_services)).set_selected_item(cx, selected);
                            let status = self.services.first().map(room_service_status).unwrap_or_else(||crate::i18n::tr("None of your Agents is bound to this Room. Add one in Members.").into());
                            self.view.label(cx, ids!(services_status)).set_text(cx, &status);
                        }
                        Err(_) => {self.view.label(cx, ids!(services_status)).set_text(cx, crate::i18n::tr("Could not load Agent services. Refresh to retry."));}
                    }
                    self.view.redraw(cx);
                }
                if let Some(update) = action.downcast_ref::<ChatMembershipChanged>()
                    && update.epoch == self.epoch && self.pin_is_current()
                    && self.room.as_ref().is_some_and(|room| room.room_id() == &update.room_id)
                    && let Some(room) = self.room.clone()
                { self.fetch_info(cx, room); }
                if matches!(
                    action.downcast_ref::<LogoutAction>(),
                    Some(LogoutAction::ClearAppState { .. })
                ) {
                    self.clear();
                }
                if let Some(slow) = action.downcast_ref::<RoomAgentsSlow>() {
                    let call = slow.call();
                    if self.pin_is_current() && self.agent_call_matches(&call) && self.agent_busy {
                        self.show_agent_slow(cx, slow.binding);
                    }
                }
                if let Some(loaded)=action.downcast_ref::<RoomAgentsLoaded>() {
                    if self.pin_is_current() && self.agent_call_matches(loaded) {
                        self.finish_agent_request(cx, loaded);
                    }
                }
                if let Some(loaded) = action.downcast_ref::<MemberOwnershipLoaded>() {
                    if self.pin_is_current() && self.ownership_response_matches(loaded) {
                        match &loaded.result {
                            Ok((owners,incomplete)) => {self.member_owners=owners.clone();self.ownership_unavailable=*incomplete;},
                            Err(_) => {self.member_owners.clear();self.ownership_unavailable=true;},
                        }
                        if let Some(data)=self.data.as_mut() {self.indented_members=group_members(&mut data.members,&self.member_owners);}
                        self.view.redraw(cx);
                    }
                }
                if let Some(loaded) = action.downcast_ref::<ChatInfoLoaded>() {
                    if self.accepts(loaded) {
                        match &loaded.result {
                            Ok(data) => {
                                let mut data=data.clone();
                                self.indented_members=group_members(&mut data.members,&self.member_owners);
                                self.data = Some(data);
                                self.failed = false;
                                self.status.clear();
                            }
                            Err(error) => {
                                self.status = crate::i18n::format(
                                    "Could not load chat information: {error}",
                                    &[("error", (error).to_string())],
                                );
                                self.failed = true;
                            }
                        }
                        self.view.redraw(cx);
                    }
                }
                if let Some(update) = action.downcast_ref::<RoomNotificationModeUpdated>() {
                    if let Some(data) = self.data.as_mut() {
                        if data.room.room_id() == &update.room_id {
                            data.mode = update.mode;
                            self.view.redraw(cx);
                        }
                    }
                }
            }
            if self.view.button(cx, ids!(services_back)).clicked(actions) {self.services_open=false;self.view.redraw(cx);return;}
            if self.view.button(cx, ids!(overview_tab)).clicked(actions) {self.select_tab(cx, InfoTab::Overview);return;}
            if self.view.button(cx, ids!(members_tab)).clicked(actions) {self.select_tab(cx, InfoTab::Members);return;}
            if self.tab == InfoTab::Overview && self.services_open && self.view.button(cx, ids!(refresh_services)).clicked(actions) {self.load_services(cx);return;}
            if self.view.drop_down(cx, ids!(room_services)).changed(actions).is_some()
                && let Some(index) = self.view.drop_down(cx, ids!(room_services)).selected_item().checked_sub(1)
                && let Some(service) = self.services.get(index) {
                self.view.label(cx, ids!(services_status)).set_text(cx, &room_service_status(service));
            }
            if self.tab == InfoTab::Overview && self.services_open && self.view.button(cx, ids!(configure_service)).clicked(actions) && self.pin_is_current() && !self.services_busy
                && let Some(index) = self.view.drop_down(cx, ids!(room_services)).selected_item().checked_sub(1)
                && let Some(service) = self.services.get(index)
                && let (Some(agent), Some(binding)) = (service["agentId"].as_str(),service["bindingId"].as_str()) {
                cx.action(crate::hagency::ui::HagencyAction::AgentRun {agent_id:agent.into(),binding_id:binding.into()});
                return;
            }
            if self.view.navigation_bar_button(cx, ids!(history_search)).clicked(actions) || self.view.navigation_bar_button(cx, ids!(history_media)).clicked(actions) {
                if self.pin_is_current() && let Some(data) = self.data.as_ref() {
                    cx.action(super::room_history::RoomHistoryAction::Open {room: data.room.clone(), filter: if self.view.navigation_bar_button(cx, ids!(history_search)).clicked(actions) {super::room_history::HistoryFilter::Messages} else {super::room_history::HistoryFilter::Media}});
                }
                return;
            }
            if self.tab == InfoTab::Members && self.view.button(cx, ids!(invite_people)).clicked(actions) {
                if self.pin_is_current() && let Some(data)=self.data.as_ref() {cx.action(InviteModalAction::Open(data.room.clone()));}
                return;
            }
            if self.view.button(cx, ids!(close_agent_picker)).clicked(actions)
                || self.view.modal(cx, ids!(agent_picker_modal)).dismissed(actions) {
                self.close_agent_picker(cx);
                return;
            }
            if self.view.drop_down(cx,ids!(my_agents)).changed(actions).is_some() && !self.agent_busy {
                self.agent_added=false;self.agent_pending=false;
                self.view.label(cx,ids!(agent_status)).set_text(cx,crate::i18n::tr("Choose an Agent you own to bind to this Room."));
                cx.redraw_all();
            }
            if self.tab == InfoTab::Members && self.view.button(cx,ids!(invite_my_agent)).clicked(actions) {
                if self.agent_busy {self.agent_picker_open=true;self.view.modal(cx,ids!(agent_picker_modal)).open(cx);self.view.redraw(cx);}
                else {self.request_agents(cx,false);}return;
            }
            if self.tab == InfoTab::Members && self.view.button(cx,ids!(invite_selected_agent)).clicked(actions) {self.request_agents(cx,true);return;}
            if self.view.button(cx,ids!(resume_agent_requests)).clicked(actions) {self.close_agent_picker(cx);cx.action(crate::hagency::ui::HagencyAction::OpenAgents);return;}
            if self.view.button(cx, ids!(retry)).clicked(actions) {
                if let Some(room) = self.room.clone() {
                    self.load(cx, room);
                }
                return;
            }
            let list = self.view.portal_list(cx, ids!(list));
            for (_index, widget) in list.items_with_actions(actions) {
                if widget.navigation_bar_button(cx, ids!(copy_room_id)).clicked(actions) {
                    if self.pin_is_current() && self.tab == self.drawn_tab
                        && self.rendered_rows.get(&widget.widget_uid()) == Some(&InfoRow::Overview)
                        && let Some(data) = self.data.as_ref() {
                        cx.copy_to_clipboard(data.room.room_id().as_str());
                    }
                    continue;
                }
                if list.was_scrolling()
                    || !(widget.navigation_bar_button(cx, ids!(row)).clicked(actions) || widget.button(cx, ids!(row)).clicked(actions))
                {
                    continue;
                }
                if !self.pin_is_current() {continue;}
                let Some(data) = self.data.as_ref() else {continue;};
                if self.tab != self.drawn_tab {continue;}
                let Some(row) = self.rendered_rows.get(&widget.widget_uid()).copied() else {continue;};
                match row {
                    InfoRow::Member(_) => {
                        let Some(profile)=self.rendered_members.get(&widget.widget_uid()) else {continue;};
                        cx.widget_action(self.widget_uid(), ShowUserProfileAction::ShowUserProfile(UserProfileAndRoomId {user_profile:profile.clone(), room_id:data.room.room_id().clone()}));
                    }
                    InfoRow::Notifications => self.choosing_notifications = true,
                    InfoRow::Mode(mode) => {
                        if let Some((mode, _))=modes().get(mode) {submit_async_request(MatrixRequest::SetRoomNotificationMode {room_id:data.room.room_id().clone(),mode:*mode});}
                    }
                    InfoRow::Back => self.choosing_notifications = false,
                    InfoRow::AgentServices => {self.services_open = !self.services_open;if self.services_open {self.load_services(cx);}},
                    InfoRow::Overview | InfoRow::Filler => continue,
                }
                self.view.redraw(cx);
                // PortalList returns an item once per grouped child action.
                // A single pointer event can include hover/focus and click;
                // handle its click only once, especially for Matrix writes.
                break;
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.owner.is_some() && !self.pin_is_current() {
            self.clear();
        }
        let members = self.tab == InfoTab::Members;
        self.view.label(cx,ids!(ownership_status)).set_visible(cx,members && self.ownership_unavailable);
        self.view.view(cx, ids!(services_panel)).set_visible(cx, !members && self.services_open);
        self.view.view(cx, ids!(information_list)).set_visible(cx, members || !self.services_open);
        let service_selected = self.view.drop_down(cx, ids!(room_services)).selected_item().checked_sub(1).is_some_and(|i| i < self.services.len());
        self.view.button(cx, ids!(configure_service)).set_enabled(cx, service_selected && !self.services_busy && self.pin_is_current());
        self.view.button(cx, ids!(refresh_services)).set_enabled(cx, !self.services_busy);
        self.view.view(cx, ids!(member_controls)).set_visible(cx, members);
        self.view.button(cx,ids!(invite_my_agent)).set_visible(cx,members);
        if !members || !self.agent_picker_open {
            self.close_agent_picker(cx);
        }
        self.view.button(cx, ids!(invite_people)).set_enabled(cx, self.data.is_some() && self.owner.is_some());
        self.view.label(cx, ids!(members_empty)).set_visible(cx, members && self.data.as_ref().is_some_and(|d|d.members.is_empty()));
        let summary_room = self.data.as_ref().map(|data|&data.room).or(self.room.as_ref());
        self.view.label(cx, ids!(summary_name)).set_text(cx, &summary_room.map_or_else(|| crate::i18n::tr("Chat information").to_owned(), |room|room.display().into_owned()));
        let count = self.data.as_ref().map_or_else(|| crate::i18n::tr("Loading members…").to_owned(), |data| crate::i18n::format("{0} members", &[("0",data.members.len().saturating_sub(data.banned_members.len()).to_string())]));
        self.view.label(cx, ids!(summary_count)).set_text(cx, &count);
        for (id, selected) in [(ids!(overview_tab), !members), (ids!(members_tab), members)] {
            let mut button = self.view.button(cx, id);
            if selected {script_apply_eval!(cx, button, {draw_bg +: {color: mod.widgets.RINX_SELECTED} draw_text +: {color: mod.widgets.RINX_ACCENT}});}
            else {script_apply_eval!(cx, button, {draw_bg +: {color: mod.widgets.RINX_SURFACE} draw_text +: {color: mod.widgets.RINX_INK}});}
        }
        self.view.button(cx,ids!(invite_my_agent)).set_enabled(cx,self.owner.is_some());
        let selected=self.view.drop_down(cx,ids!(my_agents)).selected_item().checked_sub(1).is_some_and(|i|i<self.agents.len());
        self.view.button(cx,ids!(invite_selected_agent)).set_enabled(cx,selected && !self.agent_project.is_empty() && !self.agent_busy && !self.agent_added);
        self.view.drop_down(cx,ids!(my_agents)).set_disabled(cx,self.agent_busy);
        self.view.button(cx,ids!(invite_selected_agent)).set_text(cx,crate::i18n::tr(if self.agent_added {"Added"} else if self.agent_pending {"Check status"} else {"Add agent"}));
        self.view.button(cx,ids!(close_agent_picker)).set_text(cx,crate::i18n::tr(if self.agent_added {"Done"} else {"Close"}));
        self.view.label(cx, ids!(status)).set_text(cx, &self.status);
        self.view
            .label(cx, ids!(status))
            .set_visible(cx, !self.status.is_empty());
        self.view
            .button(cx, ids!(retry))
            .set_visible(cx, self.failed);
        self.drawn_tab = self.tab;
        self.rendered_rows.clear(); self.rendered_members.clear();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                let count = self.data.as_ref().map_or(0, |d| info_rows(self.tab, d.members.len(), self.choosing_notifications));
                list.set_item_range(cx, 0, count);
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(data) = self.data.as_mut().filter(|_| index < count) else {
                        list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                        continue;
                    };
                    let row = info_row(self.tab, data.members.len(), self.choosing_notifications, index);
                    let widget = match row {
                        InfoRow::Overview => {
                            let widget = list.item(cx, index, id!(Header));
                            widget.label(cx, ids!(room_id)).set_text(cx, data.room.room_id().as_str());
                            widget.label(cx, ids!(topic)).set_text(cx, if data.topic.is_empty() {crate::i18n::tr("No topic set.")} else {&data.topic});
                            widget
                        }
                        InfoRow::Member(member) => {
                            let Some(profile)=data.members.get_mut(member) else {continue;};
                        let widget = list.item(cx, index, id!(Member));
                        self.rendered_members.insert(widget.widget_uid(), profile.clone());
                        if let Some(mut row)=widget.navigation_bar_button(cx,ids!(row)).borrow_mut() {
                            let agent=self.indented_members.contains(&profile.user_id);
                            row.view.layout.padding.left=if agent {36.0} else {16.0};
                            row.view.walk.height=Size::Fixed(if agent {48.0} else {56.0});
                            row.view.layout.align.y=if agent {0.25} else {0.5};
                        }

                        let service = self.owner.as_ref().is_some_and(|owner| is_hagency_service(&profile.user_id, owner));
                        widget.label(cx, ids!(name)).set_text(cx, if service {crate::i18n::tr("Hagency service")} else {profile.displayable_name()});
                        let detail = crate::agent_access::framework_label(&profile.user_id)
                            .map_or_else(
                                || profile.user_id.to_string(),
                                |framework| format!("{framework} · {}", profile.user_id),
                            );
                        let detail = if data.banned_members.contains(&profile.user_id) {crate::i18n::format("Banned · {user}", &[("user", detail)])} else {detail};
                        widget.label(cx, ids!(user_id)).set_text(cx, &detail);
                        let avatar = widget.avatar(cx, ids!(avatar));
                        let loaded =
                            profile
                                .avatar_state
                                .update_from_cache(cx)
                                .is_some_and(|image| {
                                    avatar
                                        .show_image(cx, None, |cx, img| {
                                            utils::load_avatar_image(&img, cx, image)
                                        })
                                        .is_ok()
                                });
                        if !loaded {
                            avatar.show_user_text(cx, &profile.user_id, profile.displayable_name());
                        }
                        widget
                        }
                        InfoRow::Back => list.item(cx, index, id!(Back)),
                        InfoRow::Filler => list.item(cx, index, id!(Filler)),
                        _ => {
                            let widget = list.item(cx, index, id!(Action));
                            let (title, value) = match row {
                                InfoRow::Mode(mode) => modes().get(mode).map(|(mode, title)|(*title, if *mode == data.mode {"✓"} else {""})).unwrap_or(("", "")),
                                InfoRow::Back => (crate::i18n::tr("Back"), ""),
                                InfoRow::Notifications => (crate::i18n::tr("Notifications"), match data.mode {
                                    Some(RoomNotificationMode::Mute) => crate::i18n::tr("Muted"),
                                    Some(RoomNotificationMode::AllMessages) => crate::i18n::tr("All"),
                                    Some(RoomNotificationMode::MentionsAndKeywordsOnly) => crate::i18n::tr("Mentions"),
                                    None => crate::i18n::tr("Default"),
                                }),
                                InfoRow::AgentServices => (crate::i18n::tr("Room Agent services"), ""),
                                _ => ("", ""),
                            };
                            widget.label(cx, ids!(title)).set_text(cx, title);
                            widget.label(cx, ids!(value)).set_text(cx, value);
                            widget
                        }
                    };
                    self.rendered_rows.insert(widget.widget_uid(), row);
                    widget.draw_all(cx, scope);
                }
            }
        }
        DrawStep::done()
    }
}

impl MobileChatInfoRef {
    /// Isolated native UX fixtures: no signed-in client or network commands.
    #[cfg(feature = "onboarding-test-support")]
    pub fn review_agent_picker(&self, cx: &mut Cx, scenario: &str) {
        assert!(get_client().is_none(), "Agent picker review requires an offline profile");
        let Some(mut panel) = self.borrow_mut() else {return;};
        panel.review_mode=true;
        panel.owner=Some("@review:example.invalid".try_into().unwrap());
        panel.origin="https://example.invalid/".into();
        let room: OwnedRoomId="!review:example.invalid".try_into().unwrap();
        panel.room=Some(RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Team room".into()),room.clone()));
        panel.tab=InfoTab::Members;
        if scenario=="members" {
            panel.data=Some(ChatInfoData {room:panel.room.clone().unwrap(),topic:String::new(),members:Vec::new(),banned_members:HashSet::new(),mode:None});
            panel.select_tab(cx,InfoTab::Members);
            panel.view.redraw(cx);
            return;
        }
        panel.agent_picker_open=true;
        panel.view.modal(cx,ids!(agent_picker_modal)).open(cx);
        panel.view.label(cx,ids!(agent_room)).set_text(cx,"Team room");
        if scenario=="slow" {panel.show_agent_slow(cx,true);return;}
        let result=match scenario {
            "ready" => Ok(serde_json::json!({"roomId":room,"projectId":"review-project","agents":[{"id":"review-agent","displayName":"小白菜","state":"active"}]})),
            "success" => Ok(serde_json::json!({"commandState":"active","creation":{"binding":{"roomId":room,"state":"active"}}})),
            "pending" => Ok(serde_json::json!({"commandState":"pending","creation":{"binding":{"roomId":room,"state":"pending"}}})),
            "malformed" => Ok(serde_json::json!({"commandState":"active"})),
            error => Err(error.to_owned()),
        };
        let loaded=RoomAgentsLoaded {owner:panel.owner.clone().unwrap(),origin:panel.origin.clone(),epoch:panel.epoch,
            room_id:room,request:panel.agent_request,binding:scenario!="ready",result};
        panel.finish_agent_request(cx,&loaded);
        if scenario=="ready" {panel.view.drop_down(cx,ids!(my_agents)).set_selected_item(cx,1);}
        panel.view.redraw(cx);
    }
    #[cfg(test)]
    pub(crate) fn displayed_room_id(&self) -> Option<OwnedRoomId> {
        self.borrow()
            .and_then(|inner| inner.room.as_ref().map(|room| room.room_id().clone()))
    }
    pub fn ensure_room(&self, cx: &mut Cx, room: RoomNameId) {
        if let Some(mut inner) = self.borrow_mut()
            && (!inner.pin_is_current()
                || inner
                    .room
                    .as_ref()
                    .is_none_or(|current| current.room_id() != room.room_id()))
        {
            inner.load(cx, room);
        }
    }
    pub fn show(&self, cx: &mut Cx, room: RoomNameId) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.load(cx, room);
        }
    }

    pub fn clear(&self) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.clear();
        }
    }
}

#[cfg(test)]
mod information_target_tests {
    use super::*;
    fn member(id: &str, name: &str) -> UserProfile {
        UserProfile {user_id:OwnedUserId::try_from(id).unwrap(),username:Some(name.into()),avatar_state:AvatarState::Known(None)}
    }
    #[test]
    fn agents_follow_only_their_present_human_owner_without_losing_members() {
        let mut members=vec![member("@alice:s","Alice"),member("@bob:s","Bob"),member("@a:s","Z agent"),member("@b:s","A agent"),member("@orphan:s","Orphan"),member("@external:s","External"),member("@_hagency_service:s","Hagency service")];
        let mut owners=HashMap::new();let mut conflicts=HashSet::new();
        add_member_owners(&mut owners,&mut conflicts,&serde_json::json!([
            {"puppetMxid":"@a:s","ownerMxid":"@alice:s"},
            {"puppetMxid":"@b:s","ownerMxid":"@bob:s"},
            {"puppetMxid":"@orphan:s","ownerMxid":"@missing:s"},
            {"puppetMxid":"@_hagency_service:s","ownerMxid":"@alice:s"}
        ]),None);
        let indented=group_members(&mut members,&owners);
        let ids:Vec<_>=members.iter().map(|p|p.user_id.as_str()).collect();
        assert_eq!(ids,vec!["@alice:s","@a:s","@bob:s","@b:s","@external:s","@_hagency_service:s","@orphan:s"]);
        assert_eq!(indented.len(),2);
        assert!(indented.contains(&OwnedUserId::try_from("@a:s").unwrap()));
        assert!(!indented.contains(&OwnedUserId::try_from("@orphan:s").unwrap()));
        assert_eq!(group_members(&mut members,&owners),indented,"refresh preserves grouping");
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut info=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            let value=script_eval!(vm,{mod.widgets.MobileChatInfo {}});
            MobileChatInfo::script_from_value(vm,value)
        });
        info.data=Some(ChatInfoData {room:RoomNameId::empty(OwnedRoomId::try_from("!r:s").unwrap()),topic:String::new(),members,banned_members:HashSet::new(),mode:None});
        info.indented_members=indented;
        info.select_tab(&mut cx,InfoTab::Members);draw_information(&mut info,&mut cx);
        let list=info.view.portal_list(&mut cx,ids!(list));
        let parent=list.item(&mut cx,0,id!(Member)).avatar(&mut cx,ids!(avatar)).area().rect(&cx);
        let child=list.item(&mut cx,1,id!(Member)).avatar(&mut cx,ids!(avatar)).area().rect(&cx);
        assert_eq!(parent.size,dvec2(32.0,32.0));assert_eq!(child.size,parent.size);
        assert_eq!(child.pos.x-parent.pos.x,20.0);
        assert_eq!(child.pos.y-parent.pos.y,48.0,"Agent sits closer to its owner");
        let next_owner=list.item(&mut cx,2,id!(Member)).avatar(&mut cx,ids!(avatar)).area().rect(&cx);
        assert_eq!(next_owner.pos.y-child.pos.y,56.0,"keep the gap before the next owner");
        for index in [0,1] {
            let widget=list.item(&mut cx,index,id!(Member));
            let name=widget.label(&mut cx,ids!(name)).area().rect(&cx);
            let user_id=widget.label(&mut cx,ids!(user_id)).area().rect(&cx);
            let avatar=widget.avatar(&mut cx,ids!(avatar)).area().rect(&cx);
            assert!((name.pos.x-(avatar.pos.x+avatar.size.x)-8.0).abs()<0.1,"avatar and text use a compact 8px gap");
            let gap=user_id.pos.y-(name.pos.y+name.size.y);
            assert!((gap-2.0).abs()<0.1,"name and Matrix ID stay together: {gap}");
        }
        cx.with_vm(|vm|assert!(vm.take_errors().is_empty()));

    }
    #[test]
    fn conflicting_ownership_and_cycles_never_create_false_grouping() {
        let mut owners=HashMap::new();let mut conflicts=HashSet::new();
        add_member_owners(&mut owners,&mut conflicts,&serde_json::json!([
            {"puppetMxid":"@agent:s","ownerMxid":"@alice:s"},
            {"puppetMxid":"@agent:s","ownerMxid":"@bob:s"},
            {"puppetMxid":"@agent:s","ownerMxid":"@alice:s"},
            {"puppetMxid":"@a:s","ownerMxid":"@b:s"},
            {"puppetMxid":"@b:s","ownerMxid":"@a:s"},
            {"puppetMxid":"invalid","ownerMxid":"@alice:s"}
        ]),None);
        assert!(!owners.contains_key(&OwnedUserId::try_from("@agent:s").unwrap()));
        let mut members=vec![member("@a:s","Same"),member("@b:s","Same"),member("@agent:s","Same")];
        assert!(group_members(&mut members,&owners).is_empty());assert_eq!(members.len(),3);
    }
    #[test]
    fn owner_grouping_rejects_stale_room_account_and_request_results() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut info=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            let value=script_eval!(vm,{mod.widgets.MobileChatInfo {}});
            MobileChatInfo::script_from_value(vm,value)
        });
        info.owner=Some(OwnedUserId::try_from("@alice:s").unwrap());info.origin="https://s".into();info.epoch=4;info.request=8;
        info.room=Some(RoomNameId::empty(OwnedRoomId::try_from("!r:s").unwrap()));
        let mut result=MemberOwnershipLoaded{owner:info.owner.clone().unwrap(),origin:info.origin.clone(),epoch:4,request:8,room_id:OwnedRoomId::try_from("!r:s").unwrap(),result:Ok((HashMap::new(),false))};
        assert!(info.ownership_response_matches(&result));
        result.request=7;assert!(!info.ownership_response_matches(&result));result.request=8;
        result.epoch=3;assert!(!info.ownership_response_matches(&result));result.epoch=4;
        result.room_id=OwnedRoomId::try_from("!other:s").unwrap();assert!(!info.ownership_response_matches(&result));result.room_id=OwnedRoomId::try_from("!r:s").unwrap();
        result.owner=OwnedUserId::try_from("@bob:s").unwrap();assert!(!info.ownership_response_matches(&result));
    }
    #[test]
    fn explicit_room_pause_and_membership_loss_have_different_service_messages() {
        assert!(room_service_status(&serde_json::json!({"bindingState":"suspended","ownerServicePaused":true})).contains("pause notice"));
        assert!(room_service_status(&serde_json::json!({"bindingState":"suspended","ownerServicePaused":false})).contains("access unavailable"));
    }
    #[test]
    fn room_service_status_never_substitutes_a_running_private_chat() {
        let service = serde_json::json!({"agentId":"agent","bindingId":"project-room","thisDevice":true,"statusAvailable":true,"runtime":{"agentId":"agent","bindingId":"private-chat","phase":"online","bindings":[{"bindingId":"private-chat","phase":"online"}]}});
        assert!(room_service_status(&service).starts_with("This Room is not running"));
        let mut running = service.clone();
        running["runtime"]["bindings"][0]["bindingId"] = "project-room".into();
        assert!(room_service_status(&running).starts_with("Responding in this Room"));
        running["thisDevice"] = false.into();
        assert_eq!(room_service_status(&running), "Not assigned to this device.");
        running["thisDevice"] = true.into();running["statusAvailable"] = false.into();
        assert!(room_service_status(&running).starts_with("Could not verify"));
    }
    #[test]
    fn service_results_cannot_cross_room_or_account_or_request() {
        let mut cx = Cx::new(Box::new(|_,_|{}));
        let mut info = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.MobileChatInfo {}});
            MobileChatInfo::script_from_value(vm, value)
        });
        let owner = matrix_sdk::ruma::user_id!("@owner:server").to_owned();
        let room = matrix_sdk::ruma::room_id!("!target:server").to_owned();
        info.owner = Some(owner.clone());info.origin = "https://server/".into();info.epoch = 4;info.room = Some(RoomNameId::empty(room.clone()));info.services_request = 8;
        let mut result = RoomServicesLoaded {owner,origin:info.origin.clone(),epoch:4,room_id:room,request:8,result:Ok(serde_json::json!({"roomId":"!target:server"}))};
        assert!(info.services_response_matches(&result));
        result.result = Ok(serde_json::json!({"roomId":"!other:server"}));assert!(!info.services_response_matches(&result));
        result.result = Err("offline".into());result.request = 7;assert!(!info.services_response_matches(&result));
        result.request = 8;result.epoch = 3;assert!(!info.services_response_matches(&result));
        result.epoch = 4;result.owner = matrix_sdk::ruma::user_id!("@other:server").to_owned();assert!(!info.services_response_matches(&result));
        info.clear();assert!(!info.services_response_matches(&result));
    }
    #[test]
    fn overview_actions_and_member_indices_never_share_notification_or_profile_targets() {
        for members in [0, 1, 5, 200] {
            for choosing in [false, true] {
                assert_eq!(info_row(InfoTab::Members, members, choosing, members), InfoRow::Filler);
                for index in 0..members {assert_eq!(info_row(InfoTab::Members, members, choosing, index), InfoRow::Member(index));}
                assert_eq!(info_row(InfoTab::Overview, members, choosing, 0), InfoRow::Overview);
                if choosing {
                    for mode in 0..4 {assert_eq!(info_row(InfoTab::Overview, members, true, mode+1), InfoRow::Mode(mode));}
                    assert_eq!(info_row(InfoTab::Overview, members, true, 5), InfoRow::Back);
                    assert_eq!(info_row(InfoTab::Overview, members, true, 6), InfoRow::Filler);
                } else {
                    assert_eq!(info_row(InfoTab::Overview, members, false, 1), InfoRow::Notifications);
                    assert_eq!(info_row(InfoTab::Overview, members, false, 2), InfoRow::AgentServices);
                    assert_eq!(info_row(InfoTab::Overview, members, false, 3), InfoRow::Filler);
                }
            }
        }
    }
    #[test]
    fn service_label_identifies_only_exact_service_identity_on_owner_server() {
        let owner=matrix_sdk::ruma::user_id!("@alice:server");
        assert!(is_hagency_service(matrix_sdk::ruma::user_id!("@_hagency_service:server"),owner));
        assert!(!is_hagency_service(matrix_sdk::ruma::user_id!("@_hagency_service:other"),owner));
        assert!(!is_hagency_service(matrix_sdk::ruma::user_id!("@agent:server"),owner));
    }
    fn draw_information(info: &mut MobileChatInfo, cx: &mut Cx) {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        cx.redraw_all();let pass=DrawPass::new(cx);pass.set_size(cx,dvec2(280.0,775.0));
        let mut list=DrawList2d::new(cx);let overlay=Overlay{draw_list:DrawList::new(cx)};
        let event=DrawEvent::default();let mut draw=CxDraw::new(cx,&event);let mut cx2d=Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass,None);list.begin_always(&mut cx2d);overlay.begin(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(280.0,775.0),Layout::flow_overlay());
        info.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk{width:Size::Fixed(280.0),height:Size::Fixed(775.0),..Walk::default()});
        overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();list.end(&mut cx2d);cx2d.end_pass(&pass);
    }
    #[test]
    fn registered_room_tabs_keep_summary_fixed_and_original_invitation_intent() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut info=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);vm.bx.captured_errors=Some(Vec::new());crate::app::register_widgets(vm);
            let value=script_eval!(vm,{mod.widgets.MobileChatInfo {}});
            let info=MobileChatInfo::script_from_value(vm,value);assert!(vm.take_errors().is_empty());info
        });
        let room=RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Room A".into()),matrix_sdk::ruma::room_id!("!a:server").to_owned());
        info.room=Some(room.clone());info.data=Some(ChatInfoData{room,topic:"A topic".into(),members:vec![],banned_members:HashSet::new(),mode:None});
        info.agent_command=Some(("exact-original-input".into(),"original-key".into()));
        draw_information(&mut info,&mut cx);
        assert_eq!(info.view.label(&mut cx,ids!(summary_name)).text(),"Room A");
        assert!(!info.view.modal(&mut cx,ids!(agent_picker_modal)).is_open());
        assert!(!info.view.view(&mut cx,ids!(member_controls)).visible());
        assert!(!info.view.navigation_bar_button(&mut cx,ids!(history_search)).is_empty());
        let summary=info.view.view(&mut cx,ids!(summary)).area().rect(&cx);
        info.select_tab(&mut cx,InfoTab::Members);draw_information(&mut info,&mut cx);
        let controls=info.view.view(&mut cx,ids!(member_controls)).area().rect(&cx);
        assert!(controls.pos.y>=summary.pos.y+summary.size.y-1.0,"picker follows fixed summary");
        let members_list = info.view.portal_list(&mut cx, ids!(list)).area().rect(&cx);
        info.agent_picker_open = true;
        info.view.modal(&mut cx, ids!(agent_picker_modal)).open(&mut cx);
        draw_information(&mut info, &mut cx);
        assert!(info.view.modal(&mut cx, ids!(agent_picker_modal)).is_open());
        assert_eq!(info.view.portal_list(&mut cx, ids!(list)).area().rect(&cx), members_list,
            "opening the overlay must not move or shrink the members list");
        let dialog = info.view.view(&mut cx, ids!(agent_picker_modal.content)).area().rect(&cx);
        assert!(dialog.size.x > 200.0 && dialog.size.x <= 400.0);
        // A taller dialog can start above the member buttons and still be
        // centered. Check its center and bounds instead of a background row.
        assert!((dialog.pos.y + dialog.size.y * 0.5 - 775.0 * 0.5).abs() <= 1.0,
            "dialog is centered in the window: {dialog:?}");
        assert!(dialog.pos.y >= 0.0 && dialog.pos.y + dialog.size.y <= 775.0);
        let invite=info.view.button(&mut cx,ids!(invite_people)).area().rect(&cx);
        let agent=info.view.button(&mut cx,ids!(invite_my_agent)).area().rect(&cx);
        assert!((invite.pos.y-agent.pos.y).abs()<1.0,"member actions share a compact row");
        assert!(invite.size.y<=32.1 && agent.size.y<=32.1);
        assert!(invite.pos.x+invite.size.x<=agent.pos.x && agent.pos.x+agent.size.x<=280.0);
        assert!(info.view.portal_list(&mut cx,ids!(list)).area().rect(&cx).size.y>200.0,"invitation controls must leave space for members");
        info.choosing_notifications=true;info.select_tab(&mut cx,InfoTab::Overview);draw_information(&mut info,&mut cx);
        assert!(!info.choosing_notifications);
        assert!(!info.view.modal(&mut cx,ids!(agent_picker_modal)).is_open());
        assert_eq!(info.agent_command,Some(("exact-original-input".into(),"original-key".into())),"tab navigation doesn't duplicate or discard invitation key");
        assert_eq!(info.view.view(&mut cx,ids!(summary)).area().rect(&cx),summary);
        cx.with_vm(|vm|assert!(vm.take_errors().is_empty()));
        info.services_open=true;draw_information(&mut info,&mut cx);
        assert!(info.view.view(&mut cx,ids!(services_panel)).visible());
        assert!(!info.view.view(&mut cx,ids!(information_list)).visible());
        assert_eq!(info.view.button(&mut cx,ids!(services_back)).text(),crate::i18n::tr("Back"));
        info.select_tab(&mut cx,InfoTab::Members);draw_information(&mut info,&mut cx);
        assert!(!info.services_open);
        assert!(!info.view.view(&mut cx,ids!(services_panel)).visible());
        assert!(info.view.view(&mut cx,ids!(information_list)).visible());
        info.clear();assert_eq!(info.tab,InfoTab::Overview);assert!(info.agent_command.is_none());
    }
    #[test]
    fn room_agent_picker_results_cannot_cross_room_account_or_original_call() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut info=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);vm.bx.captured_errors=Some(Vec::new());crate::app::register_widgets(vm);
            let value=script_eval!(vm,{mod.widgets.MobileChatInfo {}});
            let info=MobileChatInfo::script_from_value(vm,value);assert!(vm.take_errors().is_empty());info
        });
        let owner=matrix_sdk::ruma::user_id!("@owner:server").to_owned();
        let room=matrix_sdk::ruma::room_id!("!target:server").to_owned();
        info.owner=Some(owner.clone());info.origin="https://server/".into();info.epoch=4;info.room=Some(RoomNameId::empty(room.clone()));info.agent_request=8;
        let mut result=RoomAgentsLoaded {owner,origin:"https://server/".into(),epoch:4,room_id:room,request:8,binding:false,result:Ok(serde_json::json!({"roomId":"!target:server","projectId":"project"}))};
        assert!(info.agent_response_matches(&result));
        result.result=Ok(serde_json::json!({"roomId":"!other:server"}));assert!(!info.agent_response_matches(&result));
        result.binding=true;result.result=Ok(serde_json::json!({"creation":{"binding":{"roomId":"!target:server"}}}));assert!(info.agent_response_matches(&result));
        result.result=Ok(serde_json::json!({"creation":{"binding":{"roomId":"!other:server"}}}));assert!(!info.agent_response_matches(&result));
        assert!(info.agent_call_matches(&result));
        info.agent_busy=true;
        info.finish_agent_request(&mut cx,&result);
        assert!(!info.agent_busy, "a malformed completion must release its exact call");
        assert!(!info.agent_added);
        assert!(info.view.label(&mut cx,ids!(agent_status)).text().contains("invalid binding result"));
        result.result=Ok(serde_json::json!({"commandState":"pending","creation":{"binding":{"roomId":"!target:server","state":"pending"}}}));
        info.finish_agent_request(&mut cx,&result);
        assert!(info.agent_pending && !info.agent_added && !info.agent_busy);
        result.result=Ok(serde_json::json!({"commandState":"active","creation":{"binding":{"roomId":"!target:server","state":"active"}}}));
        info.finish_agent_request(&mut cx,&result);
        assert!(info.agent_added && !info.agent_pending && !info.agent_busy);
        result.result=Err("agent_service_invite_permission_required".into());
        info.finish_agent_request(&mut cx,&result);
        assert!(!info.agent_added && !info.agent_busy);
        assert!(info.view.label(&mut cx,ids!(agent_status)).text().contains("permission to invite"));
        result.result=Err("offline".into());assert!(info.agent_response_matches(&result));
        result.epoch=3;assert!(!info.agent_response_matches(&result));result.epoch=4;
        result.origin="https://other/".into();assert!(!info.agent_response_matches(&result));result.origin="https://server/".into();
        result.request=7;assert!(!info.agent_response_matches(&result));result.request=8;
        result.owner=matrix_sdk::ruma::user_id!("@other:server").to_owned();assert!(!info.agent_response_matches(&result));
        info.clear();assert!(!info.agent_response_matches(&result));
    }
    #[test]
    fn late_members_cannot_cross_room_server_or_account_epoch() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut info = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.MobileChatInfo {}});
            MobileChatInfo::script_from_value(vm, value)
        });
        let owner = matrix_sdk::ruma::user_id!("@alice:server").to_owned();
        let room_a = matrix_sdk::ruma::room_id!("!a:server").to_owned();
        let room_b = matrix_sdk::ruma::room_id!("!b:server").to_owned();
        info.owner = Some(owner.clone());
        info.origin = "https://a.test/".into();
        info.epoch = 11;
        info.request = 10;
        info.room = Some(RoomNameId::empty(room_a.clone()));
        let mut result = ChatInfoLoaded {
            owner,
            origin: "https://a.test/".into(),
            epoch: 11,
            room_id: room_a.clone(),
            request: 10,
            result: Ok(ChatInfoData {
                room: RoomNameId::empty(room_a),
                topic: String::new(),
                members: vec![], banned_members: HashSet::new(),
                mode: None,
            }),
        };
        assert!(info.matches_response(&result));
        info.room = Some(RoomNameId::empty(room_b.clone()));
        assert!(
            !info.matches_response(&result),
            "same request cannot fill another Room"
        );
        result.room_id = room_b.clone();
        assert!(
            !info.matches_response(&result),
            "payload must agree with requested Room"
        );
        result.result = Err("offline".into());
        assert!(info.matches_response(&result));
        info.epoch = 12;
        assert!(!info.matches_response(&result));
        info.epoch = 11;
        info.origin = "https://b.test/".into();
        assert!(!info.matches_response(&result));
        info.origin = "https://a.test/".into();
        info.clear();
        assert!(!info.matches_response(&result));
    }
}
