//! "<agent> isn't in this room. Add it?" under the composer.
//!
//! Shown while the draft has a typed `@name` that matches none of the room's
//! members but one of this user's own Agents (`live_mentions::agent_not_in_room`).
//! "Add" runs the same bind as the room's Add agent path
//! (`bring_agent::bind_into_rooms`), which reports its outcome as a popup.
//! Sending is never blocked: the hint is only an offer.
use std::sync::{LazyLock, Mutex};
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use super::live_mentions::{agent_not_in_room, own_agents, OwnAgent};
use super::plain_mentions::PlainMentionCandidate;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ComposerAgentHint = #(ComposerAgentHint::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
        padding: Inset{left: 8 right: 8 top: 4 bottom: 2}
        hint_text := Label {width: Fill height: Fit draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
        add_agent := RobrixNeutralIconButton {
            text: #(crate::i18n::tr("Add")) i18n_text: "Add"
            height: 28 padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
            icon_walk: Walk{width: 0 height: 0} spacing: 0
        }
    }
}

/// This account's own Agents, read once per room change.
#[derive(Default)]
struct AgentCache {
    epoch: u64,
    agents: Vec<OwnAgent>,
    loading: bool,
}
static AGENTS: LazyLock<Mutex<AgentCache>> = LazyLock::new(Default::default);

/// Re-reads the Agents listing unless a read is already running.
fn request_agents() {
    {
        let mut cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        if cache.loading { return; }
        cache.loading = true;
    }
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let owner = crate::sliding_sync::current_user_id();
        let listing = match crate::hagency::backend::Service::for_current() {
            Ok(service) => service.execute(crate::hagency::model::Command::Agents).await,
            Err(code) => Err(code),
        };
        let mut cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        cache.loading = false;
        // A failed read only means no hint: sending and the "@" picker work
        // without it, and the next room change reads again.
        if let (Ok(listing), Some(owner)) = (listing, owner) {
            *cache = AgentCache { epoch, agents: own_agents(&listing, &owner), loading: false };
        }
    });
}

fn cached_agents() -> Vec<OwnAgent> {
    let cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
    if cache.epoch == crate::account_session::epoch() { cache.agents.clone() } else { Vec::new() }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ComposerAgentHint {
    #[deref] view: View,
    #[rust] room: Option<(OwnedRoomId, String)>,
    #[rust] shown: Option<OwnAgent>,
    /// Agents already added from this hint, until the member list shows them.
    #[rust] added: Vec<OwnedUserId>,
}

impl Widget for ComposerAgentHint {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event
            && self.view.button(cx, ids!(add_agent)).clicked(actions)
        {
            self.add_shown(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ComposerAgentHint {
    fn add_shown(&mut self, cx: &mut Cx) {
        let (Some(agent), Some((room_id, room_name))) = (self.shown.take(), self.room.clone()) else { return };
        crate::hagency::bring_agent::bind_into_rooms(agent.name, agent.puppet.to_string(), vec![(room_id.to_string(), room_name)]);
        self.added.push(agent.puppet);
        self.show(cx, None);
    }

    fn show(&mut self, cx: &mut Cx, agent: Option<OwnAgent>) {
        if self.shown == agent { return; }
        if let Some(agent) = &agent {
            let text = crate::i18n::tr("{0} isn't in this room. Add it?").replace("{0}", &agent.name);
            self.view.label(cx, ids!(hint_text)).set_text(cx, &text);
        }
        self.view.set_visible(cx, agent.is_some());
        self.shown = agent;
        self.view.redraw(cx);
    }
}

impl ComposerAgentHintRef {
    /// Points the hint at a room; a new room re-reads the Agents listing.
    pub(super) fn set_room(&self, cx: &mut Cx, room_id: &OwnedRoomId, room_name: &str) {
        let Some(mut inner) = self.borrow_mut() else { return };
        if inner.room.as_ref().is_some_and(|(id, _)| id == room_id) { return; }
        inner.room = Some((room_id.clone(), room_name.to_owned()));
        inner.added.clear();
        inner.show(cx, None);
        request_agents();
    }

    /// Re-evaluates the hint for the current draft `text`.
    pub(super) fn update(&self, cx: &mut Cx, text: &str, members: &[PlainMentionCandidate<'_>]) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let agents: Vec<OwnAgent> = cached_agents().into_iter().filter(|a| !inner.added.contains(&a.puppet)).collect();
        let agent = agent_not_in_room(text, members, &agents).cloned();
        inner.show(cx, agent);
    }
}
