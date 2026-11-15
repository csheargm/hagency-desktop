//! The "Agent settings" button in the chat header of an Agent's private chat:
//! a room whose only other joined member is one of this account's Agents.
//! It (and a tap on the chat title) opens that Agent's native panel.

use std::sync::atomic::{AtomicU64, Ordering};
use makepad_widgets::*;
use matrix_sdk::{RoomMemberships, ruma::OwnedRoomId};
use super::own_agents;

/// A private chat has the owner and the Agent only.
const PRIVATE_CHAT_MEMBERS: u64 = 2;
/// Tags each lookup so a late reply for a previously shown room is ignored.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// The other joined member of a two-member room, read from the local store.
#[derive(Clone, Debug)]
struct PrivateChatPeer {
    request: u64,
    peer: Option<matrix_sdk::ruma::OwnedUserId>,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentChatSettingsButton = #(AgentChatSettingsButton::register_widget(vm)) {
        visible: false, width: Fit, height: Fit
        button := NavigationBarButton {
            tooltip_below: true
            width: 36 height: 36 padding: 8
            tooltip_text: #(crate::i18n::tr("Agent settings"))
            Icon {icon_walk: Walk{width: 20 height: 20} draw_icon +: {svg: mod.widgets.ICON_SETTINGS color: mod.widgets.RINX_INK}}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct AgentChatSettingsButton {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] room_id: Option<OwnedRoomId>,
    #[rust] request: u64,
    #[rust] peer: Option<matrix_sdk::ruma::OwnedUserId>,
}

impl Widget for AgentChatSettingsButton {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(found) = action.downcast_ref::<PrivateChatPeer>().filter(|f| f.request == self.request) {
                self.peer = found.peer.clone();
                self.show_if_agent(cx);
            } else if action.downcast_ref::<own_agents::OwnAgentsChanged>().is_some() {
                self.show_if_agent(cx);
            }
        }
        if self.visible && crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt::navigation_bar_button(&self.view, cx, ids!(button)).clicked(actions) {
            self.open(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AgentChatSettingsButton {
    /// Shows the button for `room_id` once it is known to be an Agent's private chat.
    /// Cheap to call on every draw: only a new room starts a lookup.
    pub fn set_room(&mut self, cx: &mut Cx, room_id: Option<&OwnedRoomId>) {
        if self.room_id.as_ref() == room_id { return; }
        self.room_id = room_id.cloned();
        self.peer = None;
        self.visible = false;
        self.request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        if let Some(room_id) = room_id { spawn_peer_lookup(room_id.clone(), self.request); }
        self.view.redraw(cx);
    }

    /// The Agent this private chat belongs to, when it is one of ours.
    pub fn agent_id(&self) -> Option<String> {
        own_agents::agent_for(self.peer.as_deref()?)
    }

    /// Opens the Agent's native panel; `false` when this isn't an Agent's private chat.
    pub fn open(&self, cx: &mut Cx) -> bool {
        let Some(agent_id) = self.agent_id() else { return false };
        cx.action(crate::hagency::ui::HagencyAction::OpenAgent { agent_id });
        true
    }

    fn show_if_agent(&mut self, cx: &mut Cx) {
        let peer_is_agent = self.peer.as_deref().is_some_and(super::profile_route::is_agent);
        if peer_is_agent && self.agent_id().is_none() { own_agents::refresh(); }
        self.visible = self.agent_id().is_some();
        self.view.redraw(cx);
    }
}

impl AgentChatSettingsButtonRef {
    /// See [`AgentChatSettingsButton::set_room()`].
    pub fn set_room(&self, cx: &mut Cx, room_id: Option<&OwnedRoomId>) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_room(cx, room_id); }
    }

    /// Opens the Agent's panel when `event` is a tap on `title` in an Agent's private chat.
    pub fn handle_title_tap(&self, cx: &mut Cx, event: &Event, title: Area) {
        let Some(inner) = self.borrow() else { return };
        if inner.agent_id().is_none() { return; }
        if let Hit::FingerUp(fue) = event.hits(cx, title) && fue.is_over && fue.was_tap() {
            inner.open(cx);
        }
    }
}

/// Finds the other joined member of a two-member room from the local store.
fn spawn_peer_lookup(room_id: OwnedRoomId, request: u64) {
    crate::sliding_sync::spawn_async_task(async move {
        let me = crate::sliding_sync::current_user_id();
        let room = crate::sliding_sync::get_client().and_then(|client| client.get_room(&room_id));
        let peer = match room {
            Some(room) if room.joined_members_count() <= PRIVATE_CHAT_MEMBERS => {
                // A failed store read means the room's members are unknown here:
                // no button, the same as for any room that isn't a private chat.
                let members = room.members_no_sync(RoomMemberships::JOIN).await.unwrap_or_default();
                members.into_iter().map(|m| m.user_id().to_owned()).find(|id| Some(id) != me.as_ref())
            }
            _ => None,
        };
        Cx::post_action(PrivateChatPeer { request, peer });
    });
}
