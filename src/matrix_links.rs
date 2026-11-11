//! Application Matrix permalinks. Parsing is never permission to join or send.
//! One replaceable intent owns its original account, resolver and UI delivery.
use crate::{app::SelectedRoom, room::BasicRoomDetails, utils::RoomNameId};
use makepad_widgets::{Action, Cx, WidgetActionCast, WidgetActionOptionApi};
use matrix_sdk::{
    Client, Room, RoomState,
    ruma::{
        OwnedEventId, OwnedRoomId, OwnedRoomOrAliasId, OwnedServerName, RoomId,
        matrix_uri::{MatrixId, MatrixToUri, MatrixUri, UriAction},
    },
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const MAX_URL_BYTES: usize = 8192;
const MAX_VIA: usize = 16;
// The original SDK HTTP response is buffered before this inspection cap.
const MAX_EVENT_BYTES: usize = 512 * 1024;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Link {
    room: OwnedRoomOrAliasId,
    event: Option<OwnedEventId>,
    via: Vec<OwnedServerName>,
    action: Option<UriAction>,
}
/// Preserve the existing in-conversation profile UI without parsing long input.
pub(crate) fn is_user_profile_link(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES
        && (MatrixToUri::parse(url).is_ok_and(|uri| matches!(uri.id(), MatrixId::User(_)))
            || MatrixUri::parse(url).is_ok_and(|uri| matches!(uri.id(), MatrixId::User(_))))
}
/// None means an unrelated URL. Some(Err) is recognized and consumed natively.
pub(crate) fn parse(url: &str) -> Option<Result<Link, String>> {
    // Avoid parsing or allocating an arbitrarily long URL before admission.
    if url.len() > MAX_URL_BYTES {
        let matrix = url
            .get(..7)
            .is_some_and(|value| value.eq_ignore_ascii_case("matrix:"))
            || url.split_once("://").is_some_and(|(_, rest)| {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
                let host = authority
                    .rsplit('@')
                    .next()
                    .unwrap_or_default()
                    .split(':')
                    .next()
                    .unwrap_or_default();
                host.eq_ignore_ascii_case("matrix.to")
            });
        return matrix.then(|| Err(crate::i18n::tr("This Matrix link is too long.").into()));
    }
    let recognized = url
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("matrix:"))
        || url::Url::parse(url)
            .ok()
            .is_some_and(|u| u.host_str() == Some("matrix.to"));
    if !recognized {
        return None;
    }
    Some((|| {
        if url.len() > MAX_URL_BYTES {
            return Err(crate::i18n::tr("This Matrix link is too long.").into());
        }
        let (id, via, action) = if let Ok(uri) = MatrixToUri::parse(url) {
            (uri.id().clone(), uri.via().to_vec(), None)
        } else {
            let uri = MatrixUri::parse(url)
                .map_err(|_| crate::i18n::tr("This Matrix link is invalid."))?;
            (uri.id().clone(), uri.via().to_vec(), uri.action().cloned())
        };
        if via.len() > MAX_VIA {
            return Err(crate::i18n::tr("This Matrix link has too many routing servers.").into());
        }
        let (room, event) = match id {
            MatrixId::Room(room) => (room.into(), None),
            MatrixId::RoomAlias(alias) => (alias.into(), None),
            MatrixId::Event(room, event) => (room, Some(event)),
            MatrixId::User(_) => {
                return Err(crate::i18n::tr(
                    "Open a user link from a conversation to view its profile.",
                )
                .into());
            }
            _ => return Err(crate::i18n::tr("This Matrix link target is unsupported.").into()),
        };
        if action
            .as_ref()
            .is_some_and(|action| !matches!(action, UriAction::Join))
        {
            return Err(crate::i18n::tr(
                "This Matrix link action is unsupported. No room or chat was created.",
            )
            .into());
        }
        Ok(Link {
            room,
            event,
            via,
            action,
        })
    })())
}
/// Common URL entry; recognized failures never fall through to an external app.
pub(crate) fn open(url: &str) -> bool {
    let Some(link) = parse(url) else {
        return false;
    };
    Cx::post_action(opening(link));
    true
}

/// Route URLs delivered by the operating system or a subsequent app launch.
pub(crate) fn handle_app_open(cx: &mut Cx, event: &makepad_widgets::makepad_platform::event::AppOpenEvent) {
    for item in &event.items {
        if let Some(link) = item.url().and_then(parse) {
            cx.action(opening(link));
        }
    }
}

fn opening(link: Result<Link, String>) -> LinkAction {
    let epoch = crate::account_session::epoch();
    let ticket = crate::sliding_sync::get_client().map(|client| Ticket::new(&client));
    LinkAction::Open(link, ticket, epoch)
}

pub(crate) fn publish_error(epoch: u64, message: String) {
    crate::shared::popup_list::enqueue_scoped_popup_notification(
        epoch,
        message,
        crate::shared::popup_list::PopupKind::Error,
        Some(10.0),
    );
}

type Current = Arc<dyn Fn() -> bool + Send + Sync>;
#[derive(Clone)]
pub struct Ticket(Arc<TicketInner>);
struct TicketInner {
    current: Current,
    epoch: u64,
    original: Client,
    live: AtomicBool,
}
impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixLinkTicket")
            .field("live", &self.is_current())
            .finish_non_exhaustive()
    }
}
impl Ticket {
    fn new(client: &Client) -> Self {
        let epoch = crate::account_session::epoch();
        let original = client.clone();
        let owner = client.clone();
        let revision = crate::sliding_sync::client_revision();
        let current: Current = Arc::new(move || {
            epoch == crate::account_session::epoch()
                && !crate::sliding_sync::account_changing()
                && !crate::logout::logout_state_machine::is_logout_in_progress()
                && revision == crate::sliding_sync::client_revision()
                && crate::matrix_context::is_current(&owner)
        });
        Self(Arc::new(TicketInner {
            current,
            epoch,
            original,
            live: AtomicBool::new(true),
        }))
    }
    pub fn is_current(&self) -> bool {
        self.0.live.load(Ordering::Acquire) && (self.0.current)()
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn cancel(&self) {
        self.0.live.store(false, Ordering::Release);
    }
    fn check(&self) -> Result<(), String> {
        self.is_current().then_some(()).ok_or_else(|| {
            crate::i18n::tr("Matrix navigation was cancelled or the account changed.").into()
        })
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Navigation {
    pub ticket: Ticket,
    pub selected: SelectedRoom,
    pub event: Option<OwnedEventId>,
}
impl Navigation {
    pub fn is_current(&self) -> bool {
        self.ticket.is_current()
    }
}
#[derive(Clone, Debug)]
pub(crate) enum LinkAction {
    Open(Result<Link, String>, Option<Ticket>, u64),
    Confirm(Ticket),
    Dismiss(Ticket),
    Finished(Ticket, Result<Outcome, String>),
    EnterHome(Navigation),
    Navigate(Navigation),
}

/// Both queued UI steps retain the original intent, including their root switch.
pub(crate) fn publish_navigation(cx: &mut Cx, navigation: Navigation) {
    cx.action(LinkAction::EnterHome(navigation));
}
#[derive(Clone, Debug)]
pub(crate) struct ResolvedRoom {
    room: OwnedRoomId,
    via: Vec<OwnedServerName>,
    event: Option<OwnedEventId>,
}
#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    Ready(Navigation),
    Confirmation {
        ticket: Ticket,
        details: Box<BasicRoomDetails>,
        is_space: bool,
        room: ResolvedRoom,
    },
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Resolving,
    Consent,
    Joining,
    Delivering,
}
struct Pending {
    ticket: Ticket,
    phase: Phase,
    room: Option<ResolvedRoom>,
}
#[derive(Default)]
pub(crate) struct Controller {
    pending: Option<Pending>,
    job: Option<tokio::task::JoinHandle<()>>,
}
pub(crate) enum Effect {
    CloseJoin,
    Join {
        ticket: Ticket,
        details: Box<BasicRoomDetails>,
        is_space: bool,
    },
    Navigate(Navigation),
    Error(u64, String),
}
impl Controller {
    pub fn cancel(&mut self) -> bool {
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.pending.take().is_some_and(|pending| {
            pending.ticket.cancel();
            matches!(pending.phase, Phase::Consent | Phase::Joining)
        })
    }
    fn matches(&self, ticket: &Ticket) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.ticket.same(ticket) && ticket.is_current())
    }
    pub fn cancel_loading(&mut self) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.phase == Phase::Resolving)
        {
            self.cancel();
            return true;
        }
        false
    }
    pub fn poll(&mut self) -> Vec<Effect> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.ticket.is_current())
        {
            return if self.cancel() {
                vec![Effect::CloseJoin]
            } else {
                Vec::new()
            };
        }
        Vec::new()
    }
    pub fn observe_navigation(&mut self, action: &Action) -> Vec<Effect> {
        let Some(pending) = self.pending.as_ref() else {
            return Vec::new();
        };
        let is_navigation = crate::app::workspace_navigation(action)
            || matches!(
                action.downcast_ref(),
                Some(crate::join_leave_room_modal::JoinLeaveRoomModalAction::Open { .. })
            )
            || matches!(
                action.as_widget_action().cast(),
                crate::home::rooms_list::RoomsListAction::Selected(_)
            );
        // Ordinary navigation is always a replacement intent. Internal root
        // switches and delivery carry the original Ticket in LinkAction instead.
        if !pending.ticket.is_current() || is_navigation {
            return if self.cancel() {
                vec![Effect::CloseJoin]
            } else {
                Vec::new()
            };
        }
        Vec::new()
    }
    pub fn action(&mut self, action: &LinkAction) -> Vec<Effect> {
        match action {
            LinkAction::Open(link, original, epoch) => {
                // Refuse stale input before it can even cancel a newer intent.
                if *epoch != crate::account_session::epoch()
                    || original.as_ref().is_some_and(|ticket| !ticket.is_current())
                    || original.is_none() && crate::sliding_sync::get_client().is_some()
                {
                    return Vec::new();
                }
                let close = self.cancel();
                let mut effects = if close {
                    vec![Effect::CloseJoin]
                } else {
                    Vec::new()
                };
                let link = match link {
                    Ok(link) => link.clone(),
                    Err(error) => {
                        effects.push(Effect::Error(*epoch, error.clone()));
                        return effects;
                    }
                };
                let Some(ticket) = original.clone() else {
                    effects.push(Effect::Error(
                        *epoch,
                        crate::i18n::tr("Sign in before opening this Matrix link.").into(),
                    ));
                    return effects;
                };
                let client = ticket.0.original.clone();
                if !ticket.is_current() {
                    effects.push(Effect::Error(
                        *epoch,
                        crate::i18n::tr("The Matrix account changed. Reopen the link.").into(),
                    ));
                    return effects;
                }
                self.pending = Some(Pending {
                    ticket: ticket.clone(),
                    phase: Phase::Resolving,
                    room: None,
                });
                self.spawn(client, ticket, async move |client, ticket| {
                    resolve(&client, link, ticket).await
                });
                effects
            }
            LinkAction::Finished(ticket, result) if self.matches(ticket) => {
                let mut effects = Vec::new();
                let pending = self.pending.as_mut().unwrap();
                if !matches!(pending.phase, Phase::Resolving | Phase::Joining) {
                    return effects;
                }
                if pending.phase == Phase::Joining {
                    effects.push(Effect::CloseJoin);
                }
                match result {
                    Ok(Outcome::Ready(navigation)) => {
                        pending.phase = Phase::Delivering;
                        effects.push(Effect::Navigate(navigation.clone()));
                    }
                    Ok(Outcome::Confirmation {
                        ticket,
                        details,
                        is_space,
                        room,
                    }) => {
                        pending.phase = Phase::Consent;
                        pending.room = Some(room.clone());
                        effects.push(Effect::Join {
                            ticket: ticket.clone(),
                            details: details.clone(),
                            is_space: *is_space,
                        });
                    }
                    Err(error) => {
                        let error = error.clone();
                        self.cancel();
                        effects.push(Effect::Error(ticket.0.epoch, error));
                    }
                }
                effects
            }
            LinkAction::Confirm(ticket) if self.matches(ticket) => {
                let pending = self.pending.as_mut().unwrap();
                if pending.phase != Phase::Consent {
                    return Vec::new();
                }
                let Some(room) = pending.room.clone() else {
                    return Vec::new();
                };
                let client = ticket.0.original.clone();
                pending.phase = Phase::Joining;
                self.spawn(client, ticket.clone(), async move |client, ticket| {
                    join(&client, room, ticket).await
                });
                Vec::new()
            }
            LinkAction::Dismiss(ticket) if self.matches(ticket) => {
                let joining = self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.phase == Phase::Joining);
                self.cancel();
                let mut effects = vec![Effect::CloseJoin];
                if joining {
                    effects.push(Effect::Error(ticket.0.epoch,crate::i18n::tr("Navigation cancelled. A join already sent to the homeserver may still complete.").into()));
                }
                effects
            }
            _ => Vec::new(),
        }
    }
    fn spawn<F, Fut>(&mut self, client: Client, ticket: Ticket, operation: F)
    where
        F: FnOnce(Client, Ticket) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Outcome, String>> + Send + 'static,
    {
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.job = Some(crate::account_session::spawn(async move {
            let result = tokio::time::timeout(RESOLVE_TIMEOUT, operation(client, ticket.clone()))
                .await
                .unwrap_or_else(|_| Err(crate::i18n::tr("Opening this Matrix link timed out. A join already sent may still complete.").into()));
            if ticket.is_current() {
                Cx::post_action(LinkAction::Finished(ticket, result));
            }
        }));
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn resolve(client: &Client, link: Link, ticket: Ticket) -> Result<Outcome, String> {
    ticket.check()?;
    let mut via = link.via;
    let room = match <&RoomId>::try_from(&*link.room) {
        Ok(room) => room.to_owned(),
        Err(alias) => {
            let response = client
                .resolve_room_alias(alias)
                .await
                .map_err(|error| error.to_string())?;
            ticket.check()?;
            for server in response.servers {
                if via.len() < MAX_VIA && !via.contains(&server) {
                    via.push(server);
                }
            }
            response.room_id
        }
    };
    let resolved = ResolvedRoom {
        room: room.clone(),
        via,
        event: link.event,
    };
    if let Some(room) = client
        .get_room(&room)
        .filter(|room| room.state() == RoomState::Joined)
    {
        return conversation(&room, resolved.event, ticket)
            .await
            .map(Outcome::Ready);
    }
    let preview = crate::sliding_sync::fetch_room_preview_with_avatar(
        client,
        (&*room).into(),
        resolved.via.clone(),
    )
    .await
    .map_err(|error| error.to_string())?;
    ticket.check()?;
    if preview.room_name_id.room_id() != &room {
        return Err(crate::i18n::tr("The room preview did not match this Matrix link.").into());
    }
    let is_space = matches!(
        preview.room_type,
        Some(matrix_sdk::ruma::room::RoomType::Space)
    );
    if is_space && resolved.event.is_some() {
        return Err(crate::i18n::tr("A Space does not have a message timeline.").into());
    }
    Ok(Outcome::Confirmation {
        ticket,
        details: Box::new(preview.into()),
        is_space,
        room: resolved,
    })
}
async fn join(client: &Client, resolved: ResolvedRoom, ticket: Ticket) -> Result<Outcome, String> {
    ticket.check()?;
    let room = if let Some(room) = client
        .get_room(&resolved.room)
        .filter(|room| room.state() == RoomState::Joined)
    {
        room
    } else {
        client
            .join_room_by_id_or_alias((&*resolved.room).into(), &resolved.via)
            .await
            .map_err(|error| error.to_string())?
    };
    ticket.check()?;
    if room.room_id() != resolved.room {
        return Err(crate::i18n::tr(
            "The homeserver joined a different room. Navigation was stopped.",
        )
        .into());
    }
    conversation(&room, resolved.event, ticket)
        .await
        .map(Outcome::Ready)
}
async fn conversation(
    room: &Room,
    event: Option<OwnedEventId>,
    ticket: Ticket,
) -> Result<Navigation, String> {
    ticket.check()?;
    if room.state() != RoomState::Joined {
        return Err(crate::i18n::tr("Join this room before opening its messages.").into());
    }
    let thread = if let Some(event) = &event {
        verified_thread(room, event, &ticket).await?
    } else {
        None
    };
    ticket.check()?;
    let name = RoomNameId::from_room(room).await;
    ticket.check()?;
    let selected = if room.is_space() {
        if event.is_some() {
            return Err(crate::i18n::tr("A Space does not have a message timeline.").into());
        }
        SelectedRoom::Space {
            space_name_id: name,
        }
    } else if let Some(root) = thread {
        SelectedRoom::Thread {
            room_name_id: name,
            thread_root_event_id: root,
        }
    } else {
        SelectedRoom::JoinedRoom { room_name_id: name }
    };
    Ok(Navigation {
        ticket,
        selected,
        event,
    })
}
async fn verified_thread(
    room: &Room,
    event: &OwnedEventId,
    ticket: &Ticket,
) -> Result<Option<OwnedEventId>, String> {
    use matrix_sdk::ruma::{
        api::client::room::get_room_event,
        events::{AnySyncTimelineEvent, room::encrypted::OriginalSyncRoomEncryptedEvent},
        serde::Raw,
    };
    ticket.check()?;
    let response = room
        .client()
        .send(get_room_event::v3::Request::new(
            room.room_id().to_owned(),
            event.clone(),
        ))
        .await
        .map_err(|error| error.to_string())?;
    ticket.check()?;
    if response.event.json().get().len() > MAX_EVENT_BYTES {
        return Err(crate::i18n::tr("The linked event is too large to inspect.").into());
    }
    let mut value: serde_json::Value =
        serde_json::from_str(response.event.json().get()).map_err(|error| error.to_string())?;
    if value.get("event_id").and_then(|v| v.as_str()) != Some(event.as_str())
        || value
            .get("room_id")
            .is_some_and(|id| id.as_str() != Some(room.room_id().as_str()))
    {
        return Err(
            crate::i18n::tr("The returned event did not match this room and message.").into(),
        );
    }
    if value["type"] == "m.room.encrypted" {
        // SDK compatibility can copy an unencrypted outer relation into missing
        // inner content. Inspect only the authenticated inner relationship.
        let outer_thread = value["content"]["m.relates_to"]["rel_type"] == "m.thread";
        if let Some(content) = value.get_mut("content").and_then(|v| v.as_object_mut()) {
            content.remove("m.relates_to");
        }
        let raw: Raw<OriginalSyncRoomEncryptedEvent> =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        let decrypted = room
            .decrypt_event(&raw, None)
            .await
            .map_err(|error| error.to_string())?;
        ticket.check()?;
        if !matches!(
            decrypted.kind,
            matrix_sdk_base::deserialized_responses::TimelineEventKind::Decrypted(_)
        ) {
            return Err(crate::i18n::tr(
                "This encrypted message cannot be decrypted on this device.",
            )
            .into());
        }
        value = serde_json::from_str(decrypted.raw().json().get())
            .map_err(|error| error.to_string())?;
        if outer_thread && value["content"]["m.relates_to"]["rel_type"] != "m.thread" {
            return Err(crate::i18n::tr("The encrypted thread relationship could not be authenticated. Navigation was stopped.").into());
        }
    }
    let typed: Raw<AnySyncTimelineEvent> =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    typed
        .deserialize()
        .map_err(|_| crate::i18n::tr("The linked event is invalid."))?;
    if value["unsigned"].get("redacted_because").is_some() {
        return Err(crate::i18n::tr("The linked message was removed.").into());
    }
    match value["content"].get("m.relates_to") {
        Some(relation) if relation["rel_type"] == "m.thread" => {
            let root = relation
                .get("event_id")
                .and_then(|value| value.as_str())
                .ok_or(crate::i18n::tr("The thread relationship is invalid."))?;
            let root = OwnedEventId::try_from(root)
                .map_err(|_| crate::i18n::tr("The thread relationship is invalid."))?;
            if &root == event {
                return Err(crate::i18n::tr("The message cannot be its own thread reply.").into());
            }
            let root_event = room
                .event(&root, None)
                .await
                .map_err(|error| error.to_string())?;
            ticket.check()?;
            if root_event.event_id() != Some(root.as_ref())
                || root_event
                    .raw()
                    .get_field::<serde_json::Value>("room_id")
                    .map_err(|_| {
                        crate::i18n::tr("The thread root did not match this conversation.")
                    })?
                    .is_some_and(|id| id.as_str() != Some(room.room_id().as_str()))
            {
                return Err(
                    crate::i18n::tr("The thread root did not match this conversation.").into(),
                );
            }
            Ok(Some(root))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests;
