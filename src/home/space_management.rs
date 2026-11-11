//! Native room/space creation and space membership management.
//! The parent link is authoritative; a failed advisory backlink is reported
//! separately so a successful creation is never retried as a second room.
use std::collections::{HashMap, HashSet};
use anyhow::{Result, anyhow, ensure};
use makepad_widgets::*;
use matrix_sdk::{Client, Room, RoomState};
use matrix_sdk::ruma::{
    OwnedRoomId, OwnedServerName, OwnedUserId, RoomId,
    api::client::room::{
        Visibility,
        create_room::v3::{CreationContent, Request, RoomPreset},
    },
    events::{
        InitialStateEvent, StateEventType,
        room::encryption::RoomEncryptionEventContent,
        space::{child::SpaceChildEventContent, parent::SpaceParentEventContent},
    },
    room::RoomType,
    serde::Raw,
};
use crate::{
    matrix_context,
    shared::navigation_bar_button::NavigationBarButtonWidgetExt,
    sliding_sync::{current_user_id, get_client, spawn_async_task},
    utils::RoomNameId,
};

#[derive(Clone, Debug, Default)]
pub struct CreateRoomConfig {
    pub name: String,
    pub topic: String,
    pub public: bool,
    pub encrypted: bool,
    pub space: bool,
    pub parent: Option<OwnedRoomId>,
}

impl CreateRoomConfig {
    fn request(&self) -> Result<Request> {
        let name = self.name.trim();
        ensure!(
            !name.is_empty() && name.chars().count() <= 255,
            "Enter a name of 1–255 characters."
        );
        ensure!(self.topic.chars().count() <= 4096, "The topic is too long.");
        ensure!(
            !self.public || !self.encrypted || self.space,
            "Public rooms cannot use this encrypted-room preset."
        );
        let mut request = Request::new();
        request.name = Some(name.into());
        request.topic = Some(self.topic.trim().into());
        request.visibility = if self.public {
            Visibility::Public
        } else {
            Visibility::Private
        };
        request.preset = Some(if self.public {
            RoomPreset::PublicChat
        } else {
            RoomPreset::PrivateChat
        });
        if self.space {
            let mut creation = CreationContent::new();
            creation.room_type = Some(RoomType::Space);
            request.creation_content = Some(Raw::new(&creation)?);
        } else if self.encrypted {
            request.initial_state.push(
                InitialStateEvent::with_empty_state_key(
                    RoomEncryptionEventContent::with_recommended_defaults(),
                )
                .to_raw_any(),
            );
        }
        Ok(request)
    }
}

struct FreshState {
    space: bool,
    name: RoomNameId,
    topic: String,
    levels: matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels,
    children: HashSet<OwnedRoomId>,
}

async fn fresh_state(client: &Client, room: &Room) -> Result<FreshState> {
    use matrix_sdk::ruma::events::room::{
        create::RoomCreateEventContent,
        power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, RoomPowerLevelsSource},
    };
    matrix_context::ensure_current(client)?;
    let response = client
        .send(
            matrix_sdk::ruma::api::client::state::get_state_events::v3::Request::new(
                room.room_id().to_owned(),
            ),
        )
        .await?;
    matrix_context::ensure_current(client)?;
    let events = response
        .room_state
        .iter()
        .map(|raw| serde_json::from_str::<serde_json::Value>(raw.json().get()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let find = |kind: &str| {
        events
            .iter()
            .find(|e| e["type"] == kind && e["state_key"] == "")
    };
    let create =
        find("m.room.create").ok_or_else(|| anyhow!("The room creation event is unavailable."))?;
    let creation: RoomCreateEventContent = serde_json::from_value(create["content"].clone())?;
    let rules = creation
        .room_version
        .rules()
        .ok_or_else(|| anyhow!("Unsupported room version."))?;
    #[allow(deprecated)]
    let creator = if rules.authorization.use_room_create_sender {
        create["sender"]
            .as_str()
            .and_then(|id| OwnedUserId::try_from(id).ok())
    } else {
        creation.creator.clone()
    };
    let mut creators: Vec<_> = creator.into_iter().collect();
    if rules.authorization.additional_room_creators {
        creators.extend(creation.additional_creators);
    }
    let powers = find("m.room.power_levels")
        .map(|e| serde_json::from_value::<RoomPowerLevelsEventContent>(e["content"].clone()))
        .transpose()?;
    let levels = RoomPowerLevels::new(
        RoomPowerLevelsSource::from(powers),
        &rules.authorization,
        creators,
    );
    let name = find("m.room.name")
        .and_then(|e| e["content"]["name"].as_str())
        .unwrap_or(room.room_id().as_str());
    let topic = find("m.room.topic")
        .and_then(|e| e["content"]["topic"].as_str())
        .unwrap_or_default()
        .to_owned();
    let children = events
        .iter()
        .filter(|e| {
            e["type"] == "m.space.child"
                && e["content"]["via"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
        })
        .filter_map(|e| {
            e["state_key"]
                .as_str()
                .and_then(|id| OwnedRoomId::try_from(id).ok())
        })
        .collect();
    Ok(FreshState {
        space: creation.room_type == Some(RoomType::Space),
        name: RoomNameId::new(
            matrix_sdk::RoomDisplayName::Named(name.into()),
            room.room_id().to_owned(),
        ),
        topic,
        levels,
        children,
    })
}

async fn editable_space(client: &Client, id: &RoomId, event: StateEventType) -> Result<Room> {
    matrix_context::ensure_current(client)?;
    let room = client
        .get_room(id)
        .ok_or_else(|| anyhow!("The space is no longer available."))?;
    ensure!(room.state() == RoomState::Joined, "Join the space first.");
    // Newly created rooms can have incomplete SDK state until sliding sync
    // catches up. Read authoritative state for management and permission checks.
    let state = fresh_state(client, &room).await?;
    ensure!(state.space, "This room is not a space.");
    let user = client.user_id().ok_or_else(|| anyhow!("Sign in first."))?;
    ensure!(
        state.levels.user_can_send_state(user, event),
        "You do not have permission to change this space."
    );
    matrix_context::ensure_current(client)?;
    Ok(room)
}

async fn route(room: &Room) -> Result<Vec<OwnedServerName>> {
    let mut route = room.route().await.unwrap_or_default();
    if route.is_empty() {
        route.extend(room.room_id().server_name().map(ToOwned::to_owned));
    }
    ensure!(!route.is_empty(), "The room has no routing server.");
    Ok(route)
}

async fn link(client: &Client, parent: &RoomId, child: &RoomId, add: bool) -> Result<String> {
    ensure!(parent != child, "A space cannot contain itself.");
    let space = editable_space(client, parent, StateEventType::SpaceChild).await?;
    let child_room = client.get_room(child);
    if add {
        let room = child_room
            .as_ref()
            .ok_or_else(|| anyhow!("Join the room before adding it."))?;
        ensure!(
            room.state() == RoomState::Joined,
            "Join the room before adding it."
        );
        let via = route(room).await?;
        matrix_context::ensure_current(client)?;
        space
            .send_state_event_for_key(child, SpaceChildEventContent::new(via))
            .await?;
    } else {
        space
            .send_state_event_raw("m.space.child", child.as_str(), serde_json::json!({}))
            .await?;
    }
    // Linking succeeds even if the user cannot edit state in the child room.
    if let Some(room) = child_room
        && room.state() == RoomState::Joined
        && let Some(user) = client.user_id()
        && room
            .power_levels()
            .await
            .is_ok_and(|levels| levels.user_can_send_state(user, StateEventType::SpaceParent))
    {
        let result: Result<()> = async {
            matrix_context::ensure_current(client)?;
            if add {
                let content = SpaceParentEventContent::new(route(&space).await?);
                room.send_state_event_for_key(parent, content).await?;
            } else {
                room.send_state_event_raw("m.space.parent", parent.as_str(), serde_json::json!({}))
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            return Ok(format!(
                "Space updated. The room's parent link could not be updated: {error}"
            ));
        }
    }
    Ok(if add {
        "Room added to space."
    } else {
        "Room removed from space. Membership is unchanged."
    }
    .into())
}

// Only a current, successful project listing distinguishes ordinary spaces from
// Agent spaces. A failed lookup must never silently skip room registration.
fn project_for_space(projects: &serde_json::Value, parent: &RoomId) -> Result<Option<String>> {
    let rows = projects["projects"].as_array().ok_or_else(|| anyhow!(crate::i18n::tr("Space types could not be checked. Refresh before adding a room.")))?;
    let mut selected = None;
    for row in rows.iter().filter(|row| row["spaceId"] == parent.as_str()) {
        let active = row["active"].as_bool().ok_or_else(|| anyhow!(crate::i18n::tr("The Project registration is unavailable.")))?;
        let id = row["id"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| anyhow!(crate::i18n::tr("The Project registration is unavailable.")))?;
        if active {
            ensure!(selected.is_none(), "{}", crate::i18n::tr("The space has conflicting Project registrations."));
            selected = Some(id.to_owned());
        }
    }
    Ok(selected)
}

fn registered_rooms(value: &serde_json::Value, project: &str) -> Result<HashMap<OwnedRoomId, bool>> {
    let rows = value["rooms"].as_array().ok_or_else(|| anyhow!(crate::i18n::tr("Room setup could not be checked. Refresh to retry.")))?;
    let mut result = HashMap::new();
    for row in rows {
        ensure!(row["projectId"] == project, "{}", crate::i18n::tr("The room registration belongs to another Project."));
        let id: OwnedRoomId = row["roomId"].as_str().and_then(|id| id.try_into().ok()).ok_or_else(|| anyhow!(crate::i18n::tr("Invalid room registration.")))?;
        let active = row["active"].as_bool().ok_or_else(|| anyhow!(crate::i18n::tr("Invalid room registration.")))?;
        ensure!(result.insert(id, active).is_none_or(|previous| previous == active), "{}", crate::i18n::tr("The room has conflicting registration states."));
    }
    Ok(result)
}

const ROOM_SETUP_INCOMPLETE: &str = "Room linked, but Agent setup is incomplete. Use Finish setup for this room to retry.";
const ROOM_SETUP_COMPLETE: &str = "Room added to Agent space. You can now add Agents.";
const ROOM_SETUP_INACTIVE: &str = "This room's Agent registration is inactive. It cannot be reactivated here.";

// The closures keep the two effects ordered and make recovery testable without
// ever creating a Matrix room. A retry always carries the original two IDs.
async fn finish_existing_room_link<L, LF, A, AF>(
    parent: &RoomId, child: &RoomId, projects: &serde_json::Value, link: L, adopt: A,
) -> Result<String>
where
    L: FnOnce() -> LF, LF: std::future::Future<Output = Result<String>>,
    A: FnOnce(crate::hagency::model::Command) -> AF,
    AF: std::future::Future<Output = std::result::Result<serde_json::Value, String>>,
{
    let project = project_for_space(projects, parent)?;
    let linked = link().await?;
    let Some(project_id) = project else { return Ok(linked) };
    let result = adopt(crate::hagency::model::Command::AdoptRoom {
        project_id: project_id.clone(), room_id: child.to_string(),
    }).await;
    let message = match result {
        Ok(value) if value["room"]["projectId"] == project_id && value["room"]["roomId"] == child.as_str() => {
            if value["room"]["active"] == true { crate::i18n::tr(ROOM_SETUP_COMPLETE).to_owned() }
            else if value["room"]["active"] == false { crate::i18n::tr(ROOM_SETUP_INACTIVE).to_owned() }
            else { crate::i18n::tr(ROOM_SETUP_INCOMPLETE).to_owned() }
        }
        Ok(_) => crate::i18n::tr(ROOM_SETUP_INCOMPLETE).to_owned(),
        Err(error) => format!("{}: {error}", crate::i18n::tr(ROOM_SETUP_INCOMPLETE)),
    };
    // A successful parent link can carry an advisory backlink warning. Keep it
    // visible even when the independent Project registration also succeeds.
    Ok(if linked == "Room added to space." { message } else { format!("{message}\n{linked}") })
}

async fn add_existing_room(client: &Client, parent: &RoomId, child: &RoomId) -> Result<String> {
    matrix_context::ensure_current(client)?;
    let service = crate::hagency::backend::Service::for_current().map_err(|e| anyhow!(e))?;
    let projects = service.execute(crate::hagency::model::Command::Projects).await.map_err(|e| anyhow!(e))?;
    matrix_context::ensure_current(client)?;
    let message = finish_existing_room_link(parent, child, &projects,
        || link(client, parent, child, true),
        |command| async {
            matrix_context::ensure_current(client).map_err(|e| e.to_string())?;
            service.execute(command).await
        },
    ).await?;
    matrix_context::ensure_current(client)?;
    Ok(message)
}

async fn finish_creation(
    client: &Client,
    name: RoomNameId,
    config: CreateRoomConfig,
    agent_space: bool,
    project: Option<String>,
) -> (String, Option<RoomNameId>, bool) {
    let result: Result<()> = async {
        matrix_context::ensure_current(client)?;
        if let Some(parent) = &config.parent {
            link(client, parent, name.room_id(), true).await?;
        }
        if agent_space || project.is_some() {
            matrix_context::ensure_current(client)?;
            let service =
                crate::hagency::backend::Service::for_current().map_err(|e| anyhow!(e))?;
            let command = if agent_space {
                crate::hagency::model::Command::AdoptProject {
                    space_id: name.room_id().to_string(),
                }
            } else {
                crate::hagency::model::Command::AdoptRoom {
                    project_id: project.unwrap(),
                    room_id: name.room_id().to_string(),
                }
            };
            matrix_context::ensure_current(client)?;
            service.execute(command).await.map_err(|e| anyhow!(e))?;
        }
        matrix_context::ensure_current(client)?;
        Ok(())
    }
    .await;
    let (message, pending) = match result {
        Ok(()) => (
            crate::i18n::tr("Created. Continue with the next step below.").to_owned(),
            false,
        ),
        Err(error) => (
            format!(
                "{}: {error}",
                crate::i18n::tr("Created, but setup is incomplete. Use Finish setup to retry.")
            ),
            true,
        ),
    };
    (message, Some(name), pending)
}

#[derive(Clone, Debug)]
struct SpaceChoice {
    name: RoomNameId,
    can_link: bool,
    can_edit: bool,
    topic: String,
    project_id: Option<String>,
}
#[derive(Clone, Debug, Default)]
struct Snapshot {
    projects_known: bool,
    spaces: Vec<SpaceChoice>,
    rooms: Vec<RoomNameId>,
    children: HashSet<OwnedRoomId>,
    registered_rooms: Option<HashMap<OwnedRoomId, bool>>,
}

async fn snapshot(client: &Client, parent: Option<&OwnedRoomId>) -> Result<Snapshot> {
    matrix_context::ensure_current(client)?;
    let user = client.user_id().ok_or_else(|| anyhow!("Sign in first."))?;
    let mut data = Snapshot::default();
    let projects = if let Ok(service) = crate::hagency::backend::Service::for_current() {
        service
            .execute(crate::hagency::model::Command::Projects)
            .await
            .ok()
    } else {
        None
    };
    data.projects_known = projects.as_ref().is_some_and(|v| v["projects"].is_array());
    for room in client.joined_rooms() {
        let mut name = RoomNameId::from_room(&room).await;
        let has_creation = room.get_state_event_static::<matrix_sdk::ruma::events::room::create::RoomCreateEventContent>().await?.is_some();
        if room.is_space() || !has_creation || parent.is_some_and(|id| id == room.room_id()) {
            let state = fresh_state(client, &room).await?;
            name = state.name.clone();
            if state.space {
                let project_id = projects
                    .as_ref()
                    .and_then(|v| v["projects"].as_array())
                    .and_then(|rows| {
                        rows.iter().find(|p| {
                            p["spaceId"] == room.room_id().as_str() && p["active"] == true
                        })
                    })
                    .and_then(|p| p["id"].as_str())
                    .map(str::to_owned);
                data.spaces.push(SpaceChoice {
                    project_id,
                    name: state.name,
                    can_link: state
                        .levels
                        .user_can_send_state(user, StateEventType::SpaceChild),
                    can_edit: state
                        .levels
                        .user_can_send_state(user, StateEventType::RoomName)
                        && state
                            .levels
                            .user_can_send_state(user, StateEventType::RoomTopic),
                    topic: state.topic,
                });
                if parent.is_some_and(|id| id == room.room_id()) {
                    data.children = state.children;
                }
            }
        }
        data.rooms.push(name);
    }
    for id in &data.children {
        if !data.rooms.iter().any(|room| room.room_id() == id) {
            data.rooms.push(RoomNameId::new(
                matrix_sdk::RoomDisplayName::Named(id.to_string()),
                id.clone(),
            ));
        }
    }
    if let Some(project) = data.spaces.iter().find(|space| parent == Some(space.name.room_id())).and_then(|space| space.project_id.as_deref()) {
        if let Ok(service) = crate::hagency::backend::Service::for_current() {
            if let Ok(value) = service.execute(crate::hagency::model::Command::ProjectRooms { project_id: project.into() }).await {
                data.registered_rooms = registered_rooms(&value, project).ok();
            }
        }
    }
    data.spaces
        .sort_by_cached_key(|space| space.name.display().to_lowercase());
    data.rooms
        .sort_by_cached_key(|room| room.display().to_lowercase());
    matrix_context::ensure_current(client)?;
    Ok(data)
}

#[derive(Clone, Debug)]
pub enum SpaceManagementAction {
    Open { parent: Option<RoomNameId> },
    Close,
}
#[derive(Clone, Debug)]
pub struct SpaceManagementChanged {
    pub owner: OwnedUserId,
    pub parent: Option<OwnedRoomId>,
}
#[derive(Debug)]
struct Loaded {
    owner: OwnedUserId,
    request: u64,
    result: Result<Snapshot, String>,
}
#[derive(Debug)]
struct Finished {
    owner: OwnedUserId,
    request: u64,
    parent: Option<OwnedRoomId>,
    result: Result<(String, Option<RoomNameId>, bool), String>,
}
/// Visual state travels with the result, independently of its translated text.
#[derive(Clone, Copy, Debug)]
enum StatusTone { Info, Success, Warning, Error }
impl StatusTone {
    fn role(self) -> &'static str {
        match self {
            Self::Info => "info", Self::Success => "success",
            Self::Warning => "warning", Self::Error => "danger",
        }
    }
}
#[derive(Debug)]
struct LinkFinished {
    owner: OwnedUserId,
    parent: OwnedRoomId,
    message: String,
    tone: StatusTone,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit padding: 0 margin: Inset{} flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Hint = Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style.font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}}
    let Title = Text {draw_text.text_style: theme.font_bold{font_size: (23 * mod.widgets.RINX_TEXT_SCALE)}}
    // Makepad Sdf2d.box uses twice the supplied radius.
    let Section = RoundedView {width: Fill height: Fit flow: Down padding: 18 spacing: 12 draw_bg +: {color: mod.widgets.RINX_SURFACE border_radius: 8 border_size: 0}}
    let Action = RinxButton {grab_key_focus: true height: (44 * mod.widgets.RINX_TEXT_SCALE) icon_walk: Walk{width: 0 height: 0} spacing: 0 draw_bg +: {
        border_radius: 6
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, self.border_radius)
            sdf.fill_keep(self.color.mix(self.color_focus, self.focus).mix(self.color_hover, self.hover).mix(self.color_down, self.down).mix(self.color_disabled, self.disabled))
            sdf.stroke(self.border_color.mix(self.border_color_focus, self.focus), max(self.border_size, self.focus))
            return sdf.result
        }
    }}
    let Primary = Action {width: Fill height: (48 * mod.widgets.RINX_TEXT_SCALE) icon_walk: Walk{width: 0 height: 0} spacing: 0 draw_bg +: {border_radius: 6 color: mod.widgets.RINX_ACCENT color_focus: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT_HOVER color_down: mod.widgets.RINX_ACCENT_DOWN border_size: 0 border_color_focus: mod.widgets.RINX_ON_ACCENT color_disabled: mod.widgets.RINX_BORDER} draw_text +: {color: mod.widgets.RINX_ON_ACCENT color_hover: mod.widgets.RINX_ON_ACCENT color_focus: mod.widgets.RINX_ON_ACCENT color_down: mod.widgets.RINX_ON_ACCENT text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)} color_disabled: mod.widgets.RINX_MUTED}}
    let Quiet = Action {draw_bg +: {color: #0000 color_hover: mod.widgets.RINX_HOVER border_size: 0} draw_text +: {color: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT}}
    let Input = RinxInput {width: Fill height: (44 * mod.widgets.RINX_TEXT_SCALE) padding: Inset{left: 12 right: 12 top: 10 bottom: 10} draw_bg +: {border_size: 0 border_radius: 5} draw_text.text_style.font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
    let Switch = ToggleFlat {
        width: 52 height: 44 text: "" padding: 0
        label_walk: Walk{width: 0 height: 0 margin: Inset{}}
        draw_bg +: {
            color: mod.widgets.RINX_BORDER color_active: mod.widgets.RINX_ACCENT
            knob_color: uniform(mod.widgets.RINX_ON_ACCENT)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let p = (self.rect_size - vec2(44.0, 26.0)) * 0.5
                sdf.box(p.x, p.y, 44.0, 26.0, 13.0)
                sdf.fill(self.color.mix(self.color_active, self.active))
                sdf.circle(p.x + 13.0 + 18.0 * self.active, p.y + 13.0, 10.5)
                sdf.fill(self.knob_color)
                return sdf.result
            }
        }
    }
    let Choice = NavigationBarButton {
        width: Fill height: Fit flow: Right spacing: 14 padding: 16 align: Align{y: 0.5}
        draw_bg +: {
            base_color: instance(mod.widgets.RINX_SURFACE)
            selected_border: instance(mod.widgets.RINX_ACCENT)
            color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED
            border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: 8
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.75, 0.75, self.rect_size.x - 1.5, self.rect_size.y - 1.5, 8.0)
                sdf.fill_keep(self.base_color.mix(self.color_hover, self.hover).mix(self.color_active, self.active))
                sdf.stroke(self.border_color.mix(self.selected_border, max(self.active, self.focus)), 1.0)
                return sdf.result
            }
        }
        badge := RoundedView {width: 40 height: 40 align: Align{x: 0.5 y: 0.5} draw_bg +: {color: mod.widgets.RINX_SELECTED border_radius: 5.5}
            icon := Icon {icon_walk: Walk{width: 22 height: 22} draw_icon +: {color: mod.widgets.RINX_ACCENT}}
        }
        copy := View {width: Fill height: Fit flow: Down spacing: 5
            title := Text {draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
            description := Hint {}
        }
        selection := RoundedView {width: 22 height: 22 align: Align{x: 0.5 y: 0.5} draw_bg +: {color: #0000 border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: 5.5}
            mark := View {width: 12 height: 12 visible: false
                Icon {icon_walk: Walk{width: 12 height: 12} draw_icon +: {svg: ICON_CHECKMARK color: mod.widgets.RINX_ON_ACCENT}}
            }
        }
    }
    mod.widgets.SpaceManagementPanel = #(SpaceManagementPanel::register_widget(vm)) {
        ..mod.widgets.SolidView
        width: Fill height: Fill flow: Down align: Align{x: 0.5} spacing: 0 padding: 0
        draw_bg.color: mod.widgets.RINX_PAGE
        navigation := View {width: Fill height: (56 * mod.widgets.RINX_TEXT_SCALE) padding: Inset{left: 16 right: 16} spacing: 8 align: Align{y: 0.5}
            close := Quiet {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
            Text {width: Fill align: Align{x: 0.5} text: #(crate::i18n::tr("Rooms and spaces")) i18n_text: "Rooms and spaces" draw_text.text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
            refresh := Quiet {text: #(crate::i18n::tr("Refresh")) i18n_text: "Refresh" spacing: 6 draw_icon +: {svg: ICON_ROTATE_CW color: mod.widgets.RINX_ACCENT} icon_walk: Walk{width: 16 height: 16}}
        }
        body := View {width: Fill{max: 600} height: Fill flow: Down spacing: 16 padding: Inset{left: 20 right: 20 top: 8 bottom: 20}
            mode_bar := RoundedView {width: Fill height: Fit padding: 3 spacing: 2 draw_bg +: {color: mod.widgets.RINX_HOVER border_radius: 6}
                create_tab := Action {width: Fill height: (44 * mod.widgets.RINX_TEXT_SCALE) text: #(crate::i18n::tr("Create")) i18n_text: "Create" draw_bg +: {border_size: 0 border_radius: 4}}
                manage_tab := Action {width: Fill height: (44 * mod.widgets.RINX_TEXT_SCALE) text: #(crate::i18n::tr("Manage space")) i18n_text: "Manage space" draw_bg +: {border_size: 0 border_radius: 4}}
            }
            wizard_heading := View {width: Fill height: Fit flow: Down spacing: 8
                View {width: Fill height: Fit align: Align{y: 0.5} spacing: 10
                    wizard_step := Hint {}
                    progress := View {width: 66 height: 3 spacing: 4
                        progress_one := RoundedView {width: Fill height: Fill draw_bg +: {color: mod.widgets.RINX_ACCENT border_radius: 1.5}}
                        progress_two := RoundedView {width: Fill height: Fill draw_bg +: {color: mod.widgets.RINX_BORDER border_radius: 1.5}}
                        progress_three := RoundedView {width: Fill height: Fill draw_bg +: {color: mod.widgets.RINX_BORDER border_radius: 1.5}}
                    }
                }
                wizard_title := Title {}
                wizard_subtitle := Hint {}
            }
            status_card := RoundedView {visible: false width: Fill height: Fit flow: Right padding: 12 spacing: 10 draw_bg +: {color: mod.widgets.RINX_WARNING_BG border_radius: 6}
                status_icon := Icon {icon_walk: Walk{width: 16 height: 16} draw_icon +: {svg: ICON_INFO color: mod.widgets.RINX_WARNING_FG}}
                status := Hint {draw_text.color: mod.widgets.RINX_WARNING_FG}
            }
            parent_controls := Section {
                parent_label := Hint {text: #(crate::i18n::tr("Choose a space for this room")) i18n_text: "Choose a space for this room"}
                parent := DropDown {
                    width: Fill height: (44 * mod.widgets.RINX_TEXT_SCALE) margin: Inset{} padding: Inset{left: 12 right: 36} align: Align{y: 0.5}
                    labels: ["No parent space"]
                    draw_text +: {color: mod.widgets.RINX_INK color_hover: mod.widgets.RINX_INK color_focus: mod.widgets.RINX_INK color_down: mod.widgets.RINX_INK text_style.font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
                    draw_bg +: {
                        color: mod.widgets.RINX_FIELD color_hover: mod.widgets.RINX_HOVER border_color: mod.widgets.RINX_ACCENT arrow_color: mod.widgets.RINX_MUTED
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 5.0)
                            sdf.fill_keep(self.color.mix(self.color_hover, self.hover))
                            sdf.stroke(self.border_color, self.focus)
                            let x = self.rect_size.x - 23.0
                            let y = self.rect_size.y * 0.5 - 2.0
                            sdf.move_to(x - 4.0, y)
                            sdf.line_to(x, y + 4.0)
                            sdf.line_to(x + 4.0, y)
                            sdf.stroke(self.arrow_color, 1.5)
                            return sdf.result
                        }
                    }
                }
            }
            create_form := ScrollYView {width: Fill height: Fill flow: Down spacing: 12 padding: Inset{right: 12}
                scroll_bars +: {scroll_bar_y +: {bar_size: 7 bar_side_margin: 0 draw_bg +: {size: 3 border_size: 0 border_radius: 1.5 color: mod.widgets.RINX_BORDER color_hover: mod.widgets.RINX_MUTED color_drag: mod.widgets.RINX_MUTED}}}
                choose_kind := View {width: Fill height: Fit flow: Down spacing: 12
                    kind_state := View {visible: false kind := DropDown {labels: ["ordinary" "agent" "room"]}}
                    ordinary_kind := Choice {
                        badge +: {icon.draw_icon.svg: ICON_PEOPLE}
                        copy +: {
                            title +: {text: #(crate::i18n::tr("Ordinary space")) i18n_text: "Ordinary space"}
                            description +: {text: #(crate::i18n::tr("A place for people and their conversations.")) i18n_text: "A place for people and their conversations."}
                        }
                    }
                    agent_kind := Choice {
                        badge +: {icon.draw_icon.svg: ICON_ROBOT}
                        copy +: {
                            title +: {text: #(crate::i18n::tr("Agent space")) i18n_text: "Agent space"}
                            description +: {text: #(crate::i18n::tr("Bring people and Agents together.")) i18n_text: "Bring people and Agents together."}
                        }
                    }
                    room_kind := Choice {
                        badge +: {icon.draw_icon.svg: ICON_THREADS}
                        copy +: {
                            title +: {text: #(crate::i18n::tr("Chat room")) i18n_text: "Chat room"}
                            description +: {text: #(crate::i18n::tr("Start a conversation inside a space.")) i18n_text: "Start a conversation inside a space."}
                        }
                    }
                    kind_help := Hint {padding: Inset{left: 4 right: 4 top: 4 bottom: 8}}
                }
                creation_details := View {visible: false width: Fill height: Fit flow: Down spacing: 16
                    Section {
                        Text {text: #(crate::i18n::tr("Name")) i18n_text: "Name"}
                        name := Input {empty_text: #(crate::i18n::tr("Give it a name")) i18n_empty_text: "Give it a name"}
                        Text {text: #(crate::i18n::tr("Description · optional")) i18n_text: "Description · optional"}
                        topic := Input {empty_text: #(crate::i18n::tr("What is this place for?")) i18n_empty_text: "What is this place for?"}
                        name_error := Hint {visible: false draw_text.color: mod.widgets.RINX_DANGER_FG}
                    }
                    privacy := Section {spacing: 6
                        public_row := View {width: Fill height: Fit align: Align{y: 0.5} spacing: 12
                            View {width: Fill height: Fit flow: Down spacing: 4
                                Text {text: #(crate::i18n::tr("Public")) i18n_text: "Public"}
                                Hint {text: #(crate::i18n::tr("Allow people to discover and join.")) i18n_text: "Allow people to discover and join."}
                            }
                            public := Switch {}
                        }
                        encrypted_row := View {width: Fill height: Fit align: Align{y: 0.5} spacing: 12
                            View {width: Fill height: Fit flow: Down spacing: 4
                                Text {text: #(crate::i18n::tr("End-to-end encryption")) i18n_text: "End-to-end encryption"}
                                Hint {text: #(crate::i18n::tr("Only room members can read messages.")) i18n_text: "Only room members can read messages."}
                            }
                            encrypted := Switch {}
                        }
                        private_note := View {visible: false width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.5}
                            Icon {icon_walk: Walk{width: 18 height: 18} draw_icon +: {svg: ICON_LOCK color: mod.widgets.RINX_MUTED}}
                            Hint {text: #(crate::i18n::tr("Private space · members join by invitation.")) i18n_text: "Private space · members join by invitation."}
                        }
                    }
                    details_help := Hint {padding: Inset{left: 4 right: 4 bottom: 8}}
                }
                creation_done := View {visible: false width: Fill height: Fit flow: Down spacing: 16
                    Section {padding: 24 spacing: 16 align: Align{x: 0.5}
                        done_badge := RoundedView {width: 56 height: 56 align: Align{x: 0.5 y: 0.5} draw_bg +: {color: mod.widgets.RINX_SUCCESS_BG border_radius: 28}
                            done_icon := Icon {icon_walk: Walk{width: 26 height: 26} draw_icon +: {svg: ICON_CHECKMARK color: mod.widgets.RINX_SUCCESS_FG}}
                        }
                        created_name := Text {align: Align{x: 0.5} draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        done_help := Hint {}
                    }
                }
            }
            wizard_footer := View {width: Fill height: Fit flow: Down spacing: 10
                choose_actions := View {width: Fill height: Fit
                    wizard_next := Primary {text: #(crate::i18n::tr("Next step")) i18n_text: "Next step"}
                }
                detail_actions := View {visible: false width: Fill height: Fit spacing: 12 align: Align{y: 0.5}
                    wizard_back := Quiet {width: 76 text: #(crate::i18n::tr("Back")) i18n_text: "Back"}
                    create := Primary {text: #(crate::i18n::tr("Create")) i18n_text: "Create"}
                }
                done_actions := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                    finish_setup := Primary {visible: false text: #(crate::i18n::tr("Finish setup")) i18n_text: "Finish setup"}
                    create_child := Primary {visible: false text: #(crate::i18n::tr("Create room")) i18n_text: "Create room"}
                    invite_created := Action {width: Fill text: #(crate::i18n::tr("Invite members")) i18n_text: "Invite members"}
                    created := Quiet {width: Fill text: #(crate::i18n::tr("Open")) i18n_text: "Open"}
                }
            }
            manage_form := View {visible: false width: Fill height: Fill flow: Down spacing: 12
                space_name := Input {empty_text: #(crate::i18n::tr("Space name")) i18n_empty_text: "Space name"}
                space_topic := Input {empty_text: #(crate::i18n::tr("Topic")) i18n_empty_text: "Topic"}
                save := Primary {text: #(crate::i18n::tr("Save space details")) i18n_text: "Save space details"}
                search := Input {empty_text: #(crate::i18n::tr("Filter rooms")) i18n_empty_text: "Filter rooms"}
                rooms := PortalList {width: Fill height: Fill
                    Row := View {width: Fill height: Fit flow: Down padding: 8 spacing: 4
                        name := Text {}
                        id := Hint {}
                        registration := Hint {visible: false}
                        finish_registration := Action {visible: false width: Fill text: #(crate::i18n::tr("Finish setup")) i18n_text: "Finish setup"}
                        toggle := Action {width: Fill}
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SpaceManagementPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    owner: Option<OwnedUserId>,
    #[rust]
    request: u64,
    #[rust]
    active: bool,
    #[rust]
    busy: bool,
    #[rust]
    manage: bool,
    #[rust]
    parent: Option<OwnedRoomId>,
    #[rust]
    data: Snapshot,
    #[rust]
    filtered: Vec<RoomNameId>,
    #[rust]
    query: String,
    #[rust]
    created: Option<RoomNameId>,
    #[rust]
    created_space: bool,
    #[rust]
    wizard_step: usize,
    #[rust]
    setup_pending: bool,
    #[rust]
    setup_config: Option<CreateRoomConfig>,
    #[rust]
    setup_agent_space: bool,
    #[rust]
    setup_project: Option<String>,
    #[cfg(feature = "onboarding-test-support")]
    #[rust]
    review_room_registration: bool,
}

fn next_request() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl SpaceManagementPanel {
    fn current(&self, owner: &OwnedUserId, request: u64) -> bool {
        self.active
            && self.owner.as_ref() == Some(owner)
            && current_user_id().as_ref() == Some(owner)
            && self.request == request
    }
    fn status(&self, cx: &mut Cx, tone: StatusTone, status: &str) {
        self.view.view(cx, ids!(status_card)).set_visible(cx, !status.is_empty());
        self.view
            .label(cx, ids!(status))
            .set_text(cx, crate::i18n::tr(status));
        let theme = crate::theme::snapshot(cx);
        let state = tone.role();
        let bg = theme.role(&format!("color.status.{state}.background"));
        let fg = theme.role(&format!("color.status.{state}.foreground"));
        let mut card = self.view.view(cx, ids!(status_card));
        script_apply_eval!(cx, card, {draw_bg +: {color: #(bg)}});
        let mut text = self.view.label(cx, ids!(status));
        script_apply_eval!(cx, text, {draw_text +: {color: #(fg)}});
        let mut icon = self.view.icon(cx, ids!(status_icon));
        script_apply_eval!(cx, icon, {draw_icon +: {color: #(fg)}});
    }
    fn choice(&self) -> Option<&SpaceChoice> {
        self.data
            .spaces
            .iter()
            .find(|space| Some(space.name.room_id()) == self.parent.as_ref())
    }
    fn room_registration(&self, room: &RoomId) -> Option<&'static str> {
        if self.parent.is_none() { return None }
        if !self.data.projects_known { return Some("Space types could not be checked. Refresh before adding a room.") }
        if !self.data.children.contains(room) { return None }
        if self.choice().and_then(|space| space.project_id.as_ref()).is_none() { return None }
        match self.data.registered_rooms.as_ref().map(|rooms| rooms.get(room)) {
            None => Some("Room setup could not be checked. Refresh to retry."),
            Some(None) => Some("Room is linked. Finish setup to enable Agents."),
            Some(Some(false)) => Some(ROOM_SETUP_INACTIVE),
            Some(Some(true)) => None,
        }
    }
    fn needs_room_registration(&self, room: &RoomId) -> bool {
        self.room_registration(room) == Some("Room is linked. Finish setup to enable Agents.")
    }
    fn sync(&mut self, cx: &mut Cx) {
        let theme = crate::theme::snapshot(cx);
        let kind = self.view.drop_down(cx, ids!(kind)).selected_item();
        let room = kind == 2;
        for (index, id) in [id!(ordinary_kind), id!(agent_kind), id!(room_kind)].into_iter().enumerate() {
            let card = self.view.navigation_bar_button(cx, &[id]);
            card.set_selected(cx, kind == index);
            let mut selection = card.view(cx, ids!(selection));
            let color = if kind == index { theme.accent } else { theme.surface };
            let border = if kind == index { theme.accent } else { theme.border };
            script_apply_eval!(cx, selection, {draw_bg +: {color: #(color) border_color: #(border)}});
            card.view(cx, ids!(mark)).set_visible(cx, kind == index);
        }
        for (id, selected) in [(id!(create_tab), !self.manage), (id!(manage_tab), self.manage)] {
            let mut button = self.view.button(cx, &[id]);
            let color = if selected {theme.surface} else {theme.hover};
            script_apply_eval!(cx, button, {draw_bg +: {color: #(color) color_focus: #(color)}});
        }
        for (index, id) in [id!(progress_one), id!(progress_two), id!(progress_three)].into_iter().enumerate() {
            let mut bar = self.view.view(cx, &[id]);
            let color = if index <= self.wizard_step {theme.accent} else {theme.border};
            script_apply_eval!(cx, bar, {draw_bg +: {color: #(color)}});
        }
        self.view.view(cx, ids!(wizard_heading)).set_visible(cx, !self.manage);
        self.view.view(cx, ids!(wizard_footer)).set_visible(cx, !self.manage);
        self.view.label(cx, ids!(wizard_step)).set_text(cx, crate::i18n::tr(match self.wizard_step {
            0 => "Step 1 of 3", 1 => "Step 2 of 3", _ => "Step 3 of 3",
        }));
        let title = match self.wizard_step {
            0 => "What would you like to create?",
            1 => match kind {0 => "New ordinary space", 1 => "New Agent space", _ => "New chat room"},
            _ if self.setup_pending => "One more step",
            _ if self.created_space => "Your space is ready",
            _ => "Your room is ready",
        };
        self.view.label(cx, ids!(wizard_title)).set_text(cx, crate::i18n::tr(title));
        self.view.label(cx, ids!(wizard_subtitle)).set_text(cx, crate::i18n::tr(match self.wizard_step {
            0 => "Choose a place for your next conversation.",
            1 => "Make it easy for everyone to find their place.",
            _ if self.setup_pending => "Your creation is saved. Complete its connection below.",
            _ => "Bring in your people and start something together.",
        }));
        let help = match kind {
            0 => "Spaces organize rooms. Create a room inside, then invite people.",
            1 => "Hagency connects the project for you. Add rooms, people and Agents next.",
            _ => "The space you choose determines whether Agents can join this room.",
        };
        self.view.label(cx, ids!(kind_help)).set_text(cx, crate::i18n::tr(help));
        self.view.label(cx, ids!(details_help)).set_text(cx, crate::i18n::tr(help));
        for (step, body, actions) in [(0,id!(choose_kind),id!(choose_actions)), (1,id!(creation_details),id!(detail_actions)), (2,id!(creation_done),id!(done_actions))] {
            self.view.view(cx, &[body]).set_visible(cx, self.wizard_step == step);
            self.view.view(cx, &[actions]).set_visible(cx, self.wizard_step == step);
        }
        self.view.view(cx, ids!(parent_controls)).set_visible(cx, self.manage || (room && self.wizard_step == 1));
        self.view.check_box(cx, ids!(encrypted)).set_visible(cx, room);
        self.view.view(cx, ids!(encrypted_row)).set_visible(cx, room);
        let agent_room = room && self.choice().is_some_and(|s| s.project_id.is_some());
        let public_visible = kind != 1 && !agent_room;
        self.view.check_box(cx, ids!(public)).set_visible(cx, public_visible);
        self.view.view(cx, ids!(public_row)).set_visible(cx, public_visible);
        self.view.view(cx, ids!(private_note)).set_visible(cx, kind == 1);
        if self.wizard_step == 1 && room {
            let guidance = if !self.data.projects_known {
                "Space types could not be checked. Refresh before creating a room."
            } else if self.choice().is_none() {
                "Choose a space to continue."
            } else if self.choice().is_some_and(|s| !s.can_link) {
                "You cannot add rooms to this space. Choose a space you can manage."
            } else if agent_room {
                "This Agent space connects your room automatically. Invite people and add Agents after creating it."
            } else { help };
            self.view.label(cx, ids!(details_help)).set_text(cx, crate::i18n::tr(guidance));
        }
        let name = self.view.text_input(cx, ids!(name)).text();
        let topic = self.view.text_input(cx, ids!(topic)).text();
        let name_valid = !name.trim().is_empty() && name.trim().chars().count() <= 255;
        let topic_valid = topic.chars().count() <= 4096;
        let invalid = name.trim().chars().count() > 255 || !topic_valid;
        self.view.label(cx, ids!(name_error)).set_visible(cx, invalid);
        self.view.label(cx, ids!(name_error)).set_text(cx, crate::i18n::tr(if !topic_valid {"The description is too long."} else {"Use a name of 255 characters or fewer."}));
        self.view.button(cx, ids!(create)).set_enabled(cx, !self.busy && name_valid && topic_valid
            && (!room || self.choice().is_some_and(|c| c.can_link) && self.data.projects_known)
            && self.created.is_none());
        self.view.button(cx, ids!(create)).set_text(cx, crate::i18n::tr(if self.busy {"Creating…"} else if room {"Create room"} else {"Create space"}));
        for id in [id!(wizard_next),id!(wizard_back),id!(finish_setup),id!(create_child)] {
            self.view.button(cx, &[id]).set_enabled(cx, !self.busy);
        }
        self.view.button(cx, ids!(finish_setup)).set_visible(cx, self.setup_pending);
        self.view.button(cx, ids!(create_child)).set_visible(cx, self.created_space && !self.setup_pending);
        self.view.button(cx, ids!(invite_created)).set_enabled(cx, !self.busy && !self.setup_pending);
        self.view.button(cx, ids!(invite_created)).set_visible(cx, !self.setup_pending);
        self.view.label(cx, ids!(created_name)).set_text(cx, &self.created.as_ref().map(|r|r.display()).unwrap_or_default());
        self.view.label(cx, ids!(done_help)).set_text(cx, crate::i18n::tr(if self.setup_pending {
            "The space or room exists. Finish its setup here; it will not be created again."
        } else if self.created_space {
            "Next, create a room in this space and invite its members. Space membership does not automatically join its rooms."
        } else if self.setup_project.is_some() {
            "Invite people, then open chat information → Members → Add Agent. The Agent owner must join this space and room. In a group, everyone must @ the Agent they want to answer."
        } else {"Invite people to start chatting. This room belongs to an ordinary space; Agents are not enabled."}));
        let mut badge = self.view.view(cx, ids!(done_badge));
        let mut icon = self.view.icon(cx, ids!(done_icon));
        if self.setup_pending {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_WARNING_BG}});
            script_apply_eval!(cx, icon, {draw_icon +: {svg: mod.widgets.ICON_LINK color: mod.widgets.RINX_WARNING_FG}});
        } else {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_SUCCESS_BG}});
            script_apply_eval!(cx, icon, {draw_icon +: {svg: mod.widgets.ICON_CHECKMARK color: mod.widgets.RINX_SUCCESS_FG}});
        }
        self.view.view(cx, ids!(create_form)).set_visible(cx, !self.manage);
        self.view.view(cx, ids!(manage_form)).set_visible(cx, self.manage);
        self.view.button(cx, ids!(save)).set_enabled(cx, !self.busy && self.choice().is_some_and(|c| c.can_edit));
        self.view.button(cx, ids!(created)).set_visible(cx, self.created.is_some());
        self.filtered = self.data.rooms.iter().filter(|room| {
            Some(room.room_id()) != self.parent.as_ref()
                && (room.display().to_lowercase().contains(&self.query)
                    || room.room_id().as_str().to_lowercase().contains(&self.query))
        }).cloned().collect();
        self.view.redraw(cx);
    }
    fn load(&mut self, cx: &mut Cx) {
        let Some(client) = get_client() else { return };
        let Some(owner) = client.user_id().map(ToOwned::to_owned) else {
            return;
        };
        self.owner = Some(owner.clone());
        self.request = next_request();
        let request = self.request;
        let parent = self.parent.clone();
        self.busy = true;
        self.sync(cx);
        spawn_async_task(async move {
            let result = snapshot(&client, parent.as_ref())
                .await
                .map_err(|e| e.to_string());
            Cx::post_action(Loaded {
                owner,
                request,
                result,
            });
        });
    }
    fn run(
        &mut self,
        cx: &mut Cx,
        operation: impl std::future::Future<Output = Result<(String, Option<RoomNameId>, bool)>>
        + Send
        + 'static,
    ) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        if self.busy || current_user_id().as_ref() != Some(&owner) {
            return;
        }
        self.request = next_request();
        let request = self.request;
        let parent = self.parent.clone();
        self.busy = true;
        self.status(cx, StatusTone::Info, "Saving…");
        self.sync(cx);
        spawn_async_task(async move {
            let result = operation.await.map_err(|e| e.to_string());
            Cx::post_action(Finished {
                owner,
                request,
                parent,
                result,
            });
        });
    }
}

impl Widget for SpaceManagementPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Window owns Tab navigation. The pinned Button handles pointer input
        // only, so activate a focused, enabled room action explicitly.
        if self.active && self.manage && self.owner == current_user_id() && self.view.visible()
            && let Event::KeyDown(key) = event
            && !key.is_repeat && !key.modifiers.is_primary() && !key.modifiers.alt
            && matches!(key.key_code, KeyCode::ReturnKey | KeyCode::Space)
        {
            let rooms = self.view.widget(cx, ids!(rooms));
            let mut selected = None;
            rooms.children(&mut |_, row| {
                if let Some(button) = owner_dashboard::keyboard::controls(&row, cx).into_iter()
                    .find(|control| control.area() == cx.key_focus() && control.borrow::<Button>().is_some())
                { selected = Some((row.widget_uid(), button.widget_uid())); }
            });
            if let Some((row, button)) = selected {
                // PortalList routes its row actions by group; keyboard and
                // pointer activation must carry the same row identity.
                cx.group_widget_actions(rooms.widget_uid(), row, |cx| {
                    cx.widget_action(button, ButtonAction::Clicked(key.modifiers));
                });
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
        if !self.active {
            return;
        }
        if self.owner != current_user_id() {
            self.active = false;
            cx.action(SpaceManagementAction::Close);
            return;
        }
        let Event::Actions(actions) = event else {
            return;
        };
        for action in actions {
            if let Some(done) = action.downcast_ref::<LinkFinished>()
                && self.owner.as_ref() == Some(&done.owner)
                && self.parent.as_ref() == Some(&done.parent)
            {
                self.status(cx, done.tone, &done.message);
            }
            if let Some(loaded) = action.downcast_ref::<Loaded>()
                && self.current(&loaded.owner, loaded.request)
            {
                self.busy = false;
                match &loaded.result {
                    Ok(data) => {
                        self.data = data.clone();
                        let labels = std::iter::once(crate::i18n::tr("No parent space").to_owned())
                            .chain(self.data.spaces.iter().map(|space| {
                                format!(
                                    "{} · {}",
                                    space.name.display(),
                                    crate::i18n::tr(if space.project_id.is_some() {
                                        "Agent space"
                                    } else if data.projects_known {
                                        "Ordinary space"
                                    } else {
                                        "Space type unavailable"
                                    })
                                )
                            }))
                            .collect::<Vec<_>>();
                        self.view.drop_down(cx, ids!(parent)).set_labels(cx, labels);
                        let index = self
                            .data
                            .spaces
                            .iter()
                            .position(|s| Some(s.name.room_id()) == self.parent.as_ref())
                            .map_or(0, |i| i + 1);
                        self.view
                            .drop_down(cx, ids!(parent))
                            .set_selected_item(cx, index);
                        if index == 0 {
                            self.parent = None;
                        }
                        if let Some(space) = self.choice() {
                            self.view
                                .text_input(cx, ids!(space_name))
                                .set_text(cx, &space.name.display());
                            self.view
                                .text_input(cx, ids!(space_topic))
                                .set_text(cx, &space.topic);
                        }
                    }
                    Err(error) => self.status(cx, StatusTone::Error, error),
                }
                self.sync(cx);
            }
            if let Some(done) = action.downcast_ref::<Finished>()
                && self.current(&done.owner, done.request)
            {
                self.busy = false;
                match &done.result {
                    Ok((message, created, pending)) => {
                        if created.is_some() {
                            self.created = created.clone();
                            self.wizard_step = 2;
                            self.view.view(cx, ids!(create_form)).set_scroll_pos(cx, dvec2(0.0, 0.0));
                            self.setup_pending = *pending;
                        }
                        self.status(cx, if *pending { StatusTone::Warning } else { StatusTone::Success }, message);
                        cx.action(SpaceManagementChanged {
                            owner: done.owner.clone(),
                            parent: done.parent.clone(),
                        });
                        self.load(cx);
                    }
                    Err(error) => self.status(cx, StatusTone::Error, error),
                }
                self.sync(cx);
            }
        }
        if self.view.button(cx, ids!(close)).clicked(actions) {
            cx.action(SpaceManagementAction::Close);
        }
        if self.busy {
            return;
        }
        if self.view.button(cx, ids!(refresh)).clicked(actions) {
            self.load(cx);
            return;
        }
        for (index,button) in [(0,id!(ordinary_kind)),(1,id!(agent_kind)),(2,id!(room_kind))] {
            if self.view.navigation_bar_button(cx,&[button]).clicked(actions) {
                self.view.drop_down(cx,ids!(kind)).set_selected_item(cx,index);
                self.sync(cx);return;
            }
        }
        if self.view.button(cx, ids!(wizard_next)).clicked(actions) {
            self.wizard_step = 1;
            self.view.view(cx, ids!(create_form)).set_scroll_pos(cx, dvec2(0.0, 0.0));
            self.sync(cx);
            self.view.text_input(cx, ids!(name)).set_key_focus(cx);
            return;
        }
        if self.view.button(cx, ids!(wizard_back)).clicked(actions) {
            self.wizard_step = 0;
            self.view.view(cx, ids!(create_form)).set_scroll_pos(cx, dvec2(0.0, 0.0));
            self.sync(cx);
            return;
        }
        if self.view.button(cx, ids!(invite_created)).clicked(actions) && !self.setup_pending {
            if let Some(room) = self.created.clone() {
                cx.action(super::invite_modal::InviteModalAction::Open(room));
            }
            return;
        }
        if self.view.button(cx, ids!(create_child)).clicked(actions) && !self.setup_pending {
            self.parent = self.created.as_ref().map(|r| r.room_id().clone());
            self.created = None;
            self.created_space = false;
            self.wizard_step = 1;
            self.setup_pending = false;
            self.view.drop_down(cx, ids!(kind)).set_selected_item(cx, 2);
            self.view.text_input(cx, ids!(name)).set_text(cx, "");
            self.load(cx);
            return;
        }
        if self.view.button(cx, ids!(finish_setup)).clicked(actions) {
            if let (Some(client), Some(room), Some(config)) = (
                get_client(),
                self.created.clone(),
                self.setup_config.clone(),
            ) {
                let agent_space = self.setup_agent_space;
                let project = self.setup_project.clone();
                self.run(cx, async move {
                    Ok(finish_creation(&client, room, config, agent_space, project).await)
                });
            }
            return;
        }
        if self.view.button(cx, ids!(create_tab)).clicked(actions) {
            self.manage = false;
            self.sync(cx);
        }
        if self.view.button(cx, ids!(manage_tab)).clicked(actions) {
            self.manage = true;
            self.sync(cx);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(parent)).selected(actions) {
            self.parent = index
                .checked_sub(1)
                .and_then(|i| self.data.spaces.get(i))
                .map(|s| s.name.room_id().clone());
            self.data.children.clear();
            self.data.registered_rooms = None;
            self.load(cx);
        }
        if self.view.check_box(cx, ids!(public)).changed(actions) == Some(true) {
            self.view.check_box(cx, ids!(encrypted)).set_active(cx, false, Animate::Yes);
        }
        if self.view.check_box(cx, ids!(encrypted)).changed(actions) == Some(true) {
            self.view.check_box(cx, ids!(public)).set_active(cx, false, Animate::Yes);
        }
        if self.view.text_input(cx, ids!(name)).changed(actions).is_some()
            || self.view.text_input(cx, ids!(topic)).changed(actions).is_some() {
            self.sync(cx);
        }
        if let Some(query) = self.view.text_input(cx, ids!(search)).changed(actions) {
            self.query = query.to_lowercase();
            self.sync(cx);
        }
        if self
            .view
            .drop_down(cx, ids!(kind))
            .selected(actions)
            .is_some()
        {
            self.sync(cx);
        }

        if self.view.button(cx, ids!(created)).clicked(actions)
            && let Some(room) = self.created.clone()
        {
            cx.action(SpaceManagementAction::Close);
            if self.created_space {
                cx.action(crate::home::navigation_tab_bar::NavigationBarAction::GoToHome);
                cx.widget_action(
                    self.widget_uid(),
                    crate::home::rooms_list::RoomsListAction::Selected(
                        crate::app::SelectedRoom::Space {
                            space_name_id: room,
                        },
                    ),
                );
            } else {
                cx.action(crate::app::AppStateAction::NavigateToRoom {
                    room_to_close: None,
                    destination_room: crate::room::BasicRoomDetails::RoomId(room),
                });
            }
        }
        #[cfg(feature = "onboarding-test-support")]
        if self.review_room_registration {
            let list = self.view.portal_list(cx, ids!(rooms));
            for (index, item) in list.items_with_actions(actions) {
                if let Some(room) = self.filtered.get(index) {
                    let finish = item.button(cx, ids!(finish_registration)).clicked(actions) && self.needs_room_registration(room.room_id());
                    let add = item.button(cx, ids!(toggle)).clicked(actions) && !self.data.children.contains(room.room_id());
                    if (finish || add) && self.data.projects_known && self.choice().is_some_and(|space| space.can_link) {
                        log!("space-registration-review {}", serde_json::json!({"parent":self.parent,"room":room.room_id(),"project":self.choice().and_then(|space|space.project_id.as_deref()),"retry":finish}));
                        self.busy = true;
                        self.status(cx, StatusTone::Info, "Saving…");
                        self.sync(cx);
                        break;
                    }
                }
            }
            return;
        }
        let Some(client) = get_client() else { return };
        if self.view.button(cx, ids!(create)).clicked(actions) && self.created.is_none() {
            let config = CreateRoomConfig {
                name: self.view.text_input(cx, ids!(name)).text(),
                topic: self.view.text_input(cx, ids!(topic)).text(),
                public: self.view.check_box(cx, ids!(public)).active(cx),
                encrypted: self.view.check_box(cx, ids!(encrypted)).active(cx),
                space: self.view.drop_down(cx, ids!(kind)).selected_item() != 2,
                parent: if self.view.drop_down(cx, ids!(kind)).selected_item() == 2 {
                    self.parent.clone()
                } else {
                    None
                },
            };
            if !config.space && (config.parent.is_none() || !self.data.projects_known) {
                self.status(
                    cx, StatusTone::Warning,
                    "Choose a space and refresh its type before creating the room.",
                );
                return;
            }
            self.created_space = config.space;
            self.setup_agent_space = self.view.drop_down(cx, ids!(kind)).selected_item() == 1;
            self.setup_project = if config.space {
                None
            } else {
                self.choice().and_then(|s| s.project_id.clone())
            };
            let mut config = config;
            if self.setup_agent_space || self.setup_project.is_some() {
                config.public = false;
            }
            self.setup_config = Some(config.clone());
            let agent_space = self.setup_agent_space;
            let project = self.setup_project.clone();
            self.run(cx, async move {
                let request = config.request()?;
                if let Some(parent) = &config.parent {
                    editable_space(&client, parent, StateEventType::SpaceChild).await?;
                }
                matrix_context::ensure_current(&client)?;
                let room = client.create_room(request).await?;
                let name = RoomNameId::new(
                    matrix_sdk::RoomDisplayName::Named(config.name.trim().into()),
                    room.room_id().to_owned(),
                );
                Ok(finish_creation(&client, name, config, agent_space, project).await)
            });
            return;
        }

        if self.view.button(cx, ids!(save)).clicked(actions)
            && let Some(parent) = self.parent.clone()
        {
            let name = self
                .view
                .text_input(cx, ids!(space_name))
                .text()
                .trim()
                .to_owned();
            let topic = self.view.text_input(cx, ids!(space_topic)).text();
            self.run(cx, async move {
                CreateRoomConfig {
                    name: name.clone(),
                    topic: topic.clone(),
                    ..Default::default()
                }
                .request()?;
                let room = editable_space(&client, &parent, StateEventType::RoomName).await?;
                editable_space(&client, &parent, StateEventType::RoomTopic).await?;
                room.set_name(name).await?;
                matrix_context::ensure_current(&client)?;
                match room.set_room_topic(&topic).await {
                    Ok(_) => Ok(("Space details saved.".into(), None, false)),
                    Err(error) => Ok((
                        format!("Space name saved; the topic could not be saved: {error}"),
                        None,
                        false,
                    )),
                }
            });
            return;
        }
        if self.manage
            && let Some(parent) = self.parent.clone()
        {
            let list = self.view.portal_list(cx, ids!(rooms));
            for (index, item) in list.items_with_actions(actions) {
                let finish = item.button(cx, ids!(finish_registration)).clicked(actions);
                if (item.button(cx, ids!(toggle)).clicked(actions) || finish)
                    && let Some(room) = self.filtered.get(index).cloned()
                {
                    let add = !self.data.children.contains(room.room_id());
                    let child = room.room_id().clone();
                    if add || finish {
                        if !self.data.projects_known || !self.choice().is_some_and(|space| space.can_link)
                            || (finish && !self.needs_room_registration(&child)) {
                            self.status(cx, StatusTone::Warning, "Space types could not be checked. Refresh before adding a room.");
                            return;
                        }
                        self.run(cx, async move {
                            Ok((add_existing_room(&client, &parent, &child).await?, None, false))
                        });
                    } else {
                        // The confirmation callback carries the exact account/space/room,
                        // so a later selection change cannot unlink another room.
                        let owner = self.owner.clone();
                        crate::shared::confirmation_modal::confirm_removal(
                            cx,
                            crate::shared::confirmation_modal::ConfirmationModalContent {
                                title_text: crate::i18n::tr("Remove from space?").into(),
                                body_text: format!(
                                    "{}\n{}\n{}",
                                    room.display(),
                                    child,
                                    crate::i18n::tr(
                                        "This removes the space link only. Nobody leaves the room."
                                    )
                                )
                                .into(),
                                on_accept_clicked: Some(Box::new(move |cx| {
                                    if owner != current_user_id() {
                                        return;
                                    }
                                    let Some(owner) = owner.clone() else { return };
                                    let client = client.clone();
                                    let parent = parent.clone();
                                    let child = child.clone();
                                    spawn_async_task(async move {
                                        let result = link(&client, &parent, &child, false).await;
                                        if !matrix_context::is_current(&client) {
                                            return;
                                        }
                                        let (message, tone, popup) = match result {
                                            Ok(message) => (message, StatusTone::Success, crate::shared::popup_list::PopupKind::Success),
                                            Err(error) => (error.to_string(), StatusTone::Error, crate::shared::popup_list::PopupKind::Error),
                                        };
                                        Cx::post_action(LinkFinished {
                                            owner: owner.clone(),
                                            parent: parent.clone(),
                                            message: message.clone(),
                                            tone,
                                        });
                                        crate::shared::popup_list::enqueue_popup_notification(
                                            crate::i18n::tr(&message).to_owned(),
                                            popup,
                                            Some(6.0),
                                        );
                                        Cx::post_action(SpaceManagementChanged {
                                            owner,
                                            parent: Some(parent),
                                        });
                                    });
                                    cx.redraw_all();
                                })),
                                ..Default::default()
                            },
                        );
                    }
                    break;
                }
            }
        }
        for action in actions {
            if let Some(changed) = action.downcast_ref::<SpaceManagementChanged>()
                && self.owner.as_ref() == Some(&changed.owner)
                && self.parent == changed.parent
                && !self.busy
            {
                self.load(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.borrow_mut::<PortalList>() {
                list.set_item_range(cx, 0, self.filtered.len());
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(room) = self.filtered.get(index) else {
                        continue;
                    };
                    let item = list.item(cx, index, id!(Row));
                    item.label(cx, ids!(name)).set_text(cx, &room.display());
                    item.label(cx, ids!(id))
                        .set_text(cx, room.room_id().as_str());
                    item.button(cx, ids!(toggle)).set_text(
                        cx,
                        crate::i18n::tr(if self.data.children.contains(room.room_id()) {
                            "Remove from space"
                        } else {
                            "Add to space"
                        }),
                    );
                    let registration = self.room_registration(room.room_id());
                    item.label(cx, ids!(registration)).set_text(cx, crate::i18n::tr(registration.unwrap_or_default()));
                    item.label(cx, ids!(registration)).set_visible(cx, registration.is_some());
                    let writable = !self.busy && self.choice().is_some_and(|c| c.can_link);
                    item.button(cx, ids!(finish_registration)).set_visible(cx, self.needs_room_registration(room.room_id()));
                    item.button(cx, ids!(finish_registration)).set_enabled(cx, writable);
                    item.button(cx, ids!(toggle)).set_enabled(cx, writable && (self.data.children.contains(room.room_id()) || self.data.projects_known));
                    item.draw_all(cx, scope);
                }
            }
        }
        DrawStep::done()
    }
}

impl SpaceManagementPanelRef {
    /// Offline visual review only: no account, network or creation commands.
    #[cfg(feature = "onboarding-test-support")]
    pub fn review(&self, cx: &mut Cx, kind: usize, step: usize, pending: bool) {
        assert!(
            get_client().is_none(),
            "The wizard review must not use a signed-in profile"
        );
        let Some(mut panel) = self.borrow_mut() else {
            return;
        };
        panel.active = true;
        panel.owner = None;
        panel.manage = false;
        panel.busy = false;
        panel.wizard_step = step;
        panel.setup_pending = pending;
        panel.setup_agent_space = kind == 1;
        panel.setup_project = (kind == 2).then(|| "review-project".into());
        panel.created_space = kind != 2;
        let id: OwnedRoomId = "!review-space:example.invalid".try_into().unwrap();
        let name = RoomNameId::new(
            matrix_sdk::RoomDisplayName::Named("Team space".into()),
            id.clone(),
        );
        panel.data = Snapshot {
            projects_known: true,
            spaces: vec![SpaceChoice {
                name: name.clone(),
                can_link: true,
                can_edit: true,
                topic: String::new(),
                project_id: Some("review-project".into()),
            }],
            ..Default::default()
        };
        panel.parent = Some(id);
        panel.created = (step == 2).then_some(name);
        panel
            .view
            .drop_down(cx, ids!(kind))
            .set_selected_item(cx, kind);
        panel.view.drop_down(cx, ids!(parent)).set_labels(
            cx,
            vec![
                crate::i18n::tr("Choose a space for this room").into(),
                format!("Team · {}", crate::i18n::tr("Agent space")),
            ],
        );
        panel
            .view
            .drop_down(cx, ids!(parent))
            .set_selected_item(cx, 1);
        panel
            .view
            .check_box(cx, ids!(encrypted))
            .set_active(cx, true, Animate::No);
        panel.status(
            cx, if pending { StatusTone::Warning } else { StatusTone::Success },
            if pending {
                "Created, but setup is incomplete. Use Finish setup to retry."
            } else if step >= 2 {
                crate::i18n::tr("Created. Continue with the next step below.")
            } else { "" },
        );
        panel.sync(cx);
    }

    /// Isolated presentation/submission fixture; completions come from explicit
    /// test events. Production link/AdoptRoom ordering is covered separately.
    #[cfg(feature = "onboarding-test-support")]
    pub fn review_manage(&self, cx: &mut Cx, scenario: &str) {
        if scenario == "focus-finish" {
            let Some(panel) = self.borrow() else { return };
            assert!(panel.review_room_registration && get_client().is_none());
            let item = panel.view.portal_list(cx, ids!(rooms)).item(cx, 0, id!(Row));
            let area = item.button(cx, ids!(finish_registration)).area();
            if area.is_valid(cx) { cx.set_key_focus(area); }
            return;
        }
        self.review(cx, 2, 0, false);
        let Some(mut panel) = self.borrow_mut() else { return };
        panel.review_room_registration = true;
        panel.manage = true;
        panel.data.projects_known = scenario != "unknown";
        let room: OwnedRoomId = "!existing-room:example.invalid".try_into().unwrap();
        panel.data.rooms = vec![RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Existing team room".into()), room.clone())];
        if !matches!(scenario, "unlinked" | "unknown") { panel.data.children.insert(room.clone()); }
        panel.data.registered_rooms = Some(HashMap::new());
        if matches!(scenario, "success" | "inactive") { panel.data.registered_rooms.as_mut().unwrap().insert(room, scenario == "success"); }
        panel.status(cx, match scenario { "success" => StatusTone::Success, "failure" | "inactive" => StatusTone::Warning, _ => StatusTone::Info }, match scenario { "failure" => ROOM_SETUP_INCOMPLETE, "success" => ROOM_SETUP_COMPLETE, "inactive" => ROOM_SETUP_INACTIVE, _ => "" });
        panel.sync(cx);
    }

    pub fn action(&self, cx: &mut Cx, modal: ModalRef, action: &SpaceManagementAction) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.request = next_request();
        match action {
            SpaceManagementAction::Open { parent } => {
                inner.active = true;
                #[cfg(feature = "onboarding-test-support")]
                { inner.review_room_registration = false; }
                inner.parent = parent.as_ref().map(|p| p.room_id().clone());
                inner.manage = parent.is_some();
                inner.created = None;
                inner.wizard_step = 0;
                inner.setup_pending = false;
                inner.setup_config = None;
                inner.setup_project = None;
                inner.created_space = false;
                inner.setup_agent_space = false;
                inner.data = Snapshot::default();
                inner.query.clear();
                inner
                    .view
                    .drop_down(cx, ids!(kind))
                    .set_selected_item(cx, 0);
                inner
                    .view
                    .check_box(cx, ids!(public))
                    .set_active(cx, false, Animate::No);
                inner
                    .view
                    .check_box(cx, ids!(encrypted))
                    .set_visible(cx, true);
                inner
                    .view
                    .check_box(cx, ids!(encrypted))
                    .set_active(cx, true, Animate::No);
                inner.view.text_input(cx, ids!(name)).set_text(cx, "");
                inner.view.text_input(cx, ids!(topic)).set_text(cx, "");
                inner.view.text_input(cx, ids!(search)).set_text(cx, "");
                inner.status(cx, StatusTone::Info, "");
                inner.load(cx);
                modal.open(cx);
            }
            SpaceManagementAction::Close => {
                inner.active = false;
                inner.data = Snapshot::default();
                modal.close(cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn panel() -> (Cx, SpaceManagementPanel) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let panel = cx.with_vm(|vm| {
            crate::theme::tests::install(vm, crate::theme::Selection::default());
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm,{mod.widgets.SpaceManagementPanel {}});
            let panel = SpaceManagementPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            panel
        });
        (cx, panel)
    }
    fn space(id: &str, project: Option<&str>, can_link: bool) -> SpaceChoice {
        SpaceChoice {
            name: RoomNameId::new(
                matrix_sdk::RoomDisplayName::Named("Team".into()),
                id.try_into().unwrap(),
            ),
            can_link,
            can_edit: can_link,
            topic: String::new(),
            project_id: project.map(str::to_owned),
        }
    }
    #[tokio::test]
    async fn existing_agent_room_link_retries_the_same_registration_after_lost_reply() {
        use std::cell::RefCell;
        let parent = RoomId::parse("!space:test").unwrap();
        let child = RoomId::parse("!existing:test").unwrap();
        let projects = serde_json::json!({"projects":[{"id":"project","spaceId":parent,"active":true}]});
        let effects = RefCell::new(Vec::new());
        for fail in [true, false] {
            let message = finish_existing_room_link(&parent, &child, &projects,
                || async { effects.borrow_mut().push(("link",parent.to_string(),child.to_string())); Ok("Room added to space.".into()) },
                |command| async {
                    let crate::hagency::model::Command::AdoptRoom {project_id,room_id} = command else { panic!("only register existing room") };
                    effects.borrow_mut().push(("adopt",project_id.clone(),room_id.clone()));
                    if fail { Err("owner_daemon_unavailable".into()) }
                    else { Ok(serde_json::json!({"room":{"projectId":project_id,"roomId":room_id,"active":true}})) }
                }).await.unwrap();
            assert!(message.starts_with(crate::i18n::tr(if fail {ROOM_SETUP_INCOMPLETE} else {ROOM_SETUP_COMPLETE})));
        }
        assert_eq!(*effects.borrow(), vec![("link","!space:test".into(),"!existing:test".into()),("adopt","project".into(),"!existing:test".into()),("link","!space:test".into(),"!existing:test".into()),("adopt","project".into(),"!existing:test".into())]);
    }
    #[tokio::test]
    async fn existing_room_registration_never_runs_before_a_verified_link() {
        let parent = RoomId::parse("!space:test").unwrap();let child = RoomId::parse("!existing:test").unwrap();
        let ordinary = serde_json::json!({"projects":[]});
        let message = finish_existing_room_link(&parent,&child,&ordinary,|| async {Ok("linked".into())},|_| async {panic!("ordinary spaces do not register Agent rooms")}).await.unwrap();
        assert_eq!(message,"linked");
        let unknown = serde_json::json!({});
        assert!(finish_existing_room_link(&parent,&child,&unknown,|| async {panic!("unknown project type must not link")},|_| async {panic!("unknown project type must not adopt")}).await.is_err());
        let agent = serde_json::json!({"projects":[{"id":"p","spaceId":parent,"active":true}]});
        assert!(finish_existing_room_link(&parent,&child,&agent,|| async {Err(anyhow!("permission denied"))},|_| async {panic!("failed link must not adopt")}).await.is_err());
        let inactive = finish_existing_room_link(&parent,&child,&agent,|| async {Ok("Room added to space.".into())},|_| async {Ok(serde_json::json!({"room":{"projectId":"p","roomId":child,"active":false}}))}).await.unwrap();
        assert_eq!(inactive,crate::i18n::tr(ROOM_SETUP_INACTIVE));
        let wrong = finish_existing_room_link(&parent,&child,&agent,|| async {Ok("Room added to space.".into())},|_| async {Ok(serde_json::json!({"room":{"projectId":"p","roomId":"!other:test","active":true}}))}).await.unwrap();
        assert_eq!(wrong,crate::i18n::tr(ROOM_SETUP_INCOMPLETE));
    }
    #[test]
    fn manage_space_reconstructs_incomplete_registration_from_server_state() {
        let (_,mut panel)=panel();let candidate=space("!space:test",Some("p"),true);
        panel.parent=Some(candidate.name.room_id().clone());panel.data.spaces=vec![candidate];panel.data.projects_known=true;
        let room:OwnedRoomId="!existing:test".try_into().unwrap();panel.data.children.insert(room.clone());
        assert!(!panel.needs_room_registration(&room),"unknown registration is not assumed absent");
        panel.data.registered_rooms=Some(registered_rooms(&serde_json::json!({"rooms":[]}),"p").unwrap());
        assert!(panel.needs_room_registration(&room),"reopening derives retry from linked room plus missing registration");
        for active in [false,true] {
            panel.data.registered_rooms=Some(registered_rooms(&serde_json::json!({"rooms":[{"projectId":"p","roomId":room,"active":active}]}),"p").unwrap());
            assert!(!panel.needs_room_registration(&room));
            assert_eq!(panel.room_registration(&room),if active {None}else{Some(ROOM_SETUP_INACTIVE)});
        }
        assert!(registered_rooms(&serde_json::json!({"rooms":[{"projectId":"foreign","roomId":room,"active":true}]}),"p").is_err());
    }
    #[test]
    fn malformed_project_or_conflicting_room_state_never_looks_ordinary_or_ready() {
        let parent = RoomId::parse("!space:test").unwrap();
        for row in [serde_json::json!({"spaceId":parent,"id":"p"}),serde_json::json!({"spaceId":parent,"id":"p","active":"yes"}),serde_json::json!({"spaceId":parent,"active":true})] {
            assert!(project_for_space(&serde_json::json!({"projects":[row]}),&parent).is_err());
        }
        let row=serde_json::json!({"spaceId":parent,"id":"p","active":true});
        assert!(project_for_space(&serde_json::json!({"projects":[row.clone(),row]}),&parent).is_err());
        let rows=serde_json::json!({"rooms":[{"projectId":"p","roomId":"!r:test","active":true},{"projectId":"p","roomId":"!r:test","active":false}]});
        assert!(registered_rooms(&rows,"p").is_err());
    }
    #[test]
    fn creation_wizard_explains_three_paths_and_requires_a_known_writable_space() {
        let (mut cx, mut panel) = panel();
        panel.view.text_input(&mut cx, ids!(name)).set_text(&mut cx, "Team");
        for kind in 0..3 {
            panel
                .view
                .drop_down(&mut cx, ids!(kind))
                .set_selected_item(&mut cx, kind);
            panel.wizard_step = 0;
            panel.sync(&mut cx);
            assert!(panel.view.view(&mut cx, ids!(choose_kind)).visible());
            assert!(!panel.view.view(&mut cx, ids!(creation_details)).visible());
            assert!(!panel.view.label(&mut cx, ids!(kind_help)).text().is_empty());
            panel.wizard_step = 1;
            panel.sync(&mut cx);
            assert!(panel.view.view(&mut cx, ids!(creation_details)).visible());
            assert_eq!(
                panel.view.view(&mut cx, ids!(parent_controls)).visible(),
                kind == 2
            );
            assert_eq!(
                panel.view.check_box(&mut cx, ids!(encrypted)).visible(),
                kind == 2
            );
            assert_eq!(
                panel
                    .view
                    .button(&mut cx, ids!(create))
                    .borrow()
                    .unwrap()
                    .enabled(),
                kind != 2
            );
        }
        let candidate = space("!space:test", Some("project"), true);
        panel.parent = Some(candidate.name.room_id().clone());
        panel.data.spaces = vec![candidate];
        panel.sync(&mut cx);
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(create))
                .borrow()
                .unwrap()
                .enabled(),
            "unknown project association must not create an unregistered Agent room"
        );
        panel.data.projects_known = true;
        panel.sync(&mut cx);
        assert!(
            panel
                .view
                .button(&mut cx, ids!(create))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(!panel.view.check_box(&mut cx, ids!(public)).visible());
        panel.data.spaces[0].can_link = false;
        panel.sync(&mut cx);
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(create))
                .borrow()
                .unwrap()
                .enabled()
        );
    }
    #[test]
    fn creation_wizard_keeps_created_room_visible_during_setup_failure() {
        let (mut cx, mut panel) = panel();
        panel.created = Some(space("!created:test", None, true).name);
        panel.wizard_step = 2;
        panel.created_space = true;
        panel.setup_pending = true;
        panel.sync(&mut cx);
        assert!(panel.view.button(&mut cx, ids!(finish_setup)).visible());
        assert!(!panel.view.button(&mut cx, ids!(create_child)).visible());
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(create))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(invite_created))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(panel.view.button(&mut cx, ids!(created)).visible());
        panel.setup_pending = false;
        panel.sync(&mut cx);
        assert!(!panel.view.button(&mut cx, ids!(finish_setup)).visible());
        assert!(panel.view.button(&mut cx, ids!(create_child)).visible());
        assert!(
            panel
                .view
                .button(&mut cx, ids!(invite_created))
                .borrow()
                .unwrap()
                .enabled()
        );
    }
    #[test]
    fn creation_tab_resumes_incomplete_setup_instead_of_starting_a_duplicate() {
        let (mut cx,mut panel)=panel();
        panel.active=true;panel.manage=true;panel.wizard_step=2;panel.setup_pending=true;
        panel.created=Some(space("!created:test",None,true).name);panel.sync(&mut cx);
        let uid=panel.view.button(&mut cx,ids!(create_tab)).widget_uid();
        let actions=cx.capture_actions(|cx|cx.widget_action(uid,ButtonAction::Clicked(KeyModifiers::default())));
        panel.handle_event(&mut cx,&Event::Actions(actions),&mut Scope::empty());
        assert!(!panel.manage);assert!(panel.created.is_some());assert_eq!(panel.wizard_step,2);assert!(panel.setup_pending);
    }
    #[test]
    fn space_creation_sets_room_type_and_never_encrypts_spaces() {
        let request = CreateRoomConfig {
            name: "Project".into(),
            space: true,
            encrypted: true,
            ..Default::default()
        }
        .request()
        .unwrap();
        assert_eq!(
            request
                .creation_content
                .unwrap()
                .deserialize()
                .unwrap()
                .room_type,
            Some(RoomType::Space)
        );
        assert!(request.initial_state.is_empty());
        assert_eq!(request.visibility, Visibility::Private);
    }
    #[test]
    fn private_room_encryption_and_invalid_names() {
        let mut config = CreateRoomConfig {
            name: "Team".into(),
            encrypted: true,
            ..Default::default()
        };
        assert_eq!(config.request().unwrap().initial_state.len(), 1);
        config.public = true;
        assert!(config.request().is_err());
        config.public = false;
        config.name = " ".into();
        assert!(config.request().is_err());
        config.name = "界".repeat(256);
        assert!(config.request().is_err());
    }
}
