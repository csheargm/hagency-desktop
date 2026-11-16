//! "<agent> isn't in this room. Add it?" under the composer.
//!
//! Shown while the draft has a typed `@name` that matches none of the room's
//! members but one of this user's own Agents (`live_mentions::agent_not_in_room`).
//! "Add" runs the same bind as the room's Add agent path
//! (`bring_agent::bind_into_rooms`), which reports its outcome as a popup.
//! Once that add succeeds, the composer turns the typed `@name` into a real mention
//! (`ComposerAgentHintRef::added`). Sending is never blocked: the hint is only an offer.
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

/// How long a listing answers before a typed name re-reads it, so an Agent
/// created after the room was opened (or a failed read) is still offered.
const AGENTS_FRESH_FOR: std::time::Duration = std::time::Duration::from_secs(15);

/// This account's own Agents, read on room change and again once stale.
#[derive(Default)]
struct AgentCache {
    epoch: u64,
    agents: Vec<OwnAgent>,
    /// The account epoch of the read in flight, if any.
    loading: Option<u64>,
    /// When the last read for `epoch` finished, successful or not.
    read_at: Option<std::time::Instant>,
}

/// Whether a typed name should re-read the listing.
fn is_stale(read_at: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    read_at.is_none_or(|at| now.duration_since(at) >= AGENTS_FRESH_FOR)
}
static AGENTS: LazyLock<Mutex<AgentCache>> = LazyLock::new(Default::default);

/// Re-reads the Agents listing unless a read is already running.
fn request_agents() {
    let epoch = crate::account_session::epoch();
    {
        let mut cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        // A read for an earlier account must not hold off this account's read.
        if cache.loading == Some(epoch) { return; }
        cache.loading = Some(epoch);
    }
    crate::account_session::spawn(async move {
        let owner = crate::sliding_sync::current_user_id();
        let listing = match crate::hagency::backend::Service::for_current() {
            Ok(service) => service.execute(crate::hagency::model::Command::Agents).await,
            Err(code) => Err(code),
        };
        let mut cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        if cache.loading != Some(epoch) || epoch != crate::account_session::epoch() { return; }
        cache.loading = None;
        // A failed read only means no hint: sending and the "@" picker work
        // without it, and the next typed name after `AGENTS_FRESH_FOR` reads again.
        if let (Ok(listing), Some(owner)) = (listing, owner) {
            cache.agents = own_agents(&listing, &owner);
            cache.epoch = epoch;
        }
        cache.read_at = Some(std::time::Instant::now());
        drop(cache);
        // Wake the UI so the waiting draft is re-checked against the new listing.
        makepad_widgets::SignalToUI::set_ui_signal();
    });
}

fn cached_agents() -> Vec<OwnAgent> {
    let cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
    if cache.epoch == crate::account_session::epoch() { cache.agents.clone() } else { Vec::new() }
}

/// Re-reads the listing when a typed name arrives and the last read is old.
fn refresh_if_stale() {
    let epoch = crate::account_session::epoch();
    let stale = {
        let cache = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
        cache.epoch != epoch || is_stale(cache.read_at, std::time::Instant::now())
    };
    if stale { request_agents(); }
}

/// The bind started from this hint finished: on success the composer
/// mentions the Agent; on failure the hint may offer it again.
#[derive(Debug)]
struct HintBound {
    hint: WidgetUid,
    epoch: u64,
    room: OwnedRoomId,
    agent: OwnAgent,
    ok: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ComposerAgentHint {
    #[deref] view: View,
    #[rust] room: Option<(OwnedRoomId, String)>,
    #[rust] shown: Option<OwnAgent>,
    /// Agents already added from this hint, until the member list shows them.
    #[rust] added: Vec<OwnedUserId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    /// Regression: the listing was read once per room, so an Agent created
    /// after opening the room (or a failed read) was never offered.
    #[test]
    fn listing_is_reread_once_stale() {
        let now = Instant::now();
        assert!(is_stale(None, now), "never read");
        assert!(!is_stale(Some(now), now));
        assert!(is_stale(Some(now - AGENTS_FRESH_FOR - Duration::from_millis(1)), now));
    }
}

impl Widget for ComposerAgentHint {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(add_agent)).clicked(actions) { self.add_shown(cx); }
            // A failed add may be offered again for the same typed name.
            if let Some(failed) = bound_here(actions, self.widget_uid()).filter(|b| !b.ok) {
                self.added.retain(|puppet| *puppet != failed.agent.puppet);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ComposerAgentHint {
    fn add_shown(&mut self, cx: &mut Cx) {
        let (Some(agent), Some((room_id, room_name))) = (self.shown.take(), self.room.clone()) else { return };
        let (hint, epoch, bound, room) = (self.widget_uid(), crate::account_session::epoch(), agent.clone(), room_id.clone());
        crate::hagency::bring_agent::bind_into_rooms_then(agent.name, agent.puppet.to_string(), vec![(room_id.to_string(), room_name)], move |result| {
            Cx::post_action(HintBound { hint, epoch, room: room.clone(), agent: bound.clone(), ok: result.is_ok() });
        });
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

/// This account's bind outcome for the hint `hint`, if `actions` carry one.
fn bound_here(actions: &Actions, hint: WidgetUid) -> Option<&HintBound> {
    actions.iter().filter_map(|a| a.downcast_ref::<HintBound>())
        .find(|b| b.hint == hint && b.epoch == crate::account_session::epoch())
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

    /// The Agent this hint added once its bind succeeded, if `actions` carry it.
    pub(super) fn added(&self, actions: &Actions) -> Option<OwnAgent> {
        let room = self.borrow().and_then(|inner| inner.room.as_ref().map(|(id, _)| id.clone()))?;
        // A bind for the room the user has since left must not mention here.
        bound_here(actions, self.widget_uid()).filter(|b| b.ok && b.room == room).map(|b| b.agent.clone())
    }

    /// Re-evaluates the hint for the current draft `text`.
    pub(super) fn update(&self, cx: &mut Cx, text: &str, members: &[PlainMentionCandidate<'_>]) {
        let Some(mut inner) = self.borrow_mut() else { return };
        if text.contains('@') { refresh_if_stale(); }
        let agents: Vec<OwnAgent> = cached_agents().into_iter().filter(|a| !inner.added.contains(&a.puppet)).collect();
        let agent = agent_not_in_room(text, members, &agents).cloned();
        inner.show(cx, agent);
    }
}
