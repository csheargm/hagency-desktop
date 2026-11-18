//! What the hover card and the profile say about an Agent: its owner, its
//! runtime and its status.
//!
//! - One of this account's own Agents: the owner is us, the status comes from
//!   the listing state plus its local service status (`Call::Status`), and the
//!   runtime from its saved model when it runs on this computer.
//! - Another owner's Agent: the Room's Agent roster (`Command::RoomRoster`)
//!   names its owner and whether it is paused in that Room. Another server's
//!   Agent has no roster row: its own run cards in the Room name its owner and
//!   runtime instead ([`super::agent_room_cards`]), and the Room's Status
//!   list what it does now ("idle", "working on #12").
//!
//! Reads never block drawing: a lookup returns what is cached and starts at
//! most one fetch, whose reply posts [`AgentFactsChanged`].

use std::{collections::{HashMap, HashSet}, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use makepad_widgets::{log, Cx};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};
use serde_json::Value;
use crate::hagency::{agent_panel::mapping, model::{Command, LocalAction}};
use super::own_agents;
pub use super::agent_fact_lines::{fact_lines, owner_label, owner_name};

/// How long an own Agent's service status is shown before it is asked again.
const STATUS_TTL: Duration = Duration::from_secs(30);
/// How long a Room's Agent roster is reused.
const ROSTER_TTL: Duration = Duration::from_secs(60);
/// Upper bound of roster rows read (the member list caps alike).
const MAX_ROWS: usize = 128;

/// Posted when a fetch changed what is known, so an open card or profile re-reads.
#[derive(Clone, Debug)]
pub struct AgentFactsChanged;

/// Whether `action` may change what an open card or profile says about an
/// Agent: a fetch here, a new own-Agents listing, a run card admitted in a
/// room (it can name a federated Agent's owner), or a room's Status list.
pub fn is_facts_update(action: &makepad_widgets::Action) -> bool {
    action.downcast_ref::<AgentFactsChanged>().is_some()
        || action.downcast_ref::<own_agents::OwnAgentsChanged>().is_some()
        || action.downcast_ref::<crate::hagency::pending_questions::PendingQuestionsChanged>().is_some()
        || action.downcast_ref::<crate::home::room_watches::WatchesChanged>().is_some()
}

/// What is known about one Agent; `None` fields are unknown or still loading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFacts {
    /// Set only for this account's own Agents: what "Agent details" opens.
    pub own_agent_id: Option<String>,
    pub owner: Option<OwnedUserId>,
    /// "Claude", "Codex" or "Octos".
    pub runtime: Option<&'static str>,
    /// An English status key ("Running", "Paused", …), translated when shown.
    pub status: Option<&'static str>,
    /// What it does in this Room now ("idle", "working on #12"), already
    /// translated; shown when there is no `status`.
    pub activity: Option<String>,
    /// An own Agent in one of its group rooms: what it does there for the
    /// room's members (ADR 0025).
    pub shared: Option<crate::hagency::room_share::RoomShare>,
}

/// An own Agent's status from its listing `state` and local `service` state
/// (`host::hagency::local` words it); `None` while the service is unread.
pub fn own_status(state: &str, service: Option<&str>) -> Option<&'static str> {
    if state == "suspended" { return Some("Paused"); }
    Some(match service? {
        "running" | "processing" | "waiting" | "starting" | "attention" => "Running",
        "stopped" | "paused" => "Paused",
        "elsewhere" => "On another device",
        "claude_sign_in" => crate::hagency::agent_panel::mapping::CLAUDE_SIGN_IN,
        "codex_sign_in" => crate::hagency::agent_panel::mapping::CODEX_SIGN_IN,
        // The host could not read the service: claim nothing.
        "unknown" => return None,
        _ => "Offline",
    })
}

/// Another owner's Agent: the roster says only whether it is paused here.
pub fn roster_status(binding_state: &str) -> Option<&'static str> {
    match binding_state {
        "suspended" => Some("Paused"),
        "active" => Some("Active in this room"),
        _ => None,
    }
}

/// One roster row: who owns the Agent, and its state in this Room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRow {
    pub owner: OwnedUserId,
    pub binding_state: String,
    /// "claude", "codex" or "octos" when the server shares it.
    // TODO(roster-runtime): the server does not publish it yet; the
    // different-model reviewer pick (ADR 0025) reads it once it does.
    pub runtime: Option<&'static str>,
}

/// Puppet → roster row for a `RoomRoster` reply's `agents`.
pub fn roster_rows(rows: &Value) -> HashMap<OwnedUserId, RosterRow> {
    rows.as_array().into_iter().flatten().take(MAX_ROWS).filter_map(|row| {
        let puppet = OwnedUserId::try_from(row["puppetMxid"].as_str()?).ok()?;
        let owner = OwnedUserId::try_from(row["ownerMxid"].as_str()?).ok()?;
        let binding_state = row["bindingState"].as_str().unwrap_or_default().to_owned();
        let runtime = match row["runtime"].as_str() { Some("claude") => Some("claude"), Some("codex") => Some("codex"), Some("octos") => Some("octos"), _ => None };
        Some((puppet, RosterRow { owner, binding_state, runtime }))
    }).collect()
}

/// An own Agent's local reply: its service state, and its runtime if known.
#[derive(Clone, Debug)]
struct Local { at: Instant, service: Option<String>, runtime: Option<&'static str>, shares: HashMap<String, crate::hagency::room_share::RoomShare> }

#[derive(Default)]
struct Cache {
    account: Option<OwnedUserId>,
    local: HashMap<String, Local>,
    local_loading: HashSet<String>,
    rosters: HashMap<OwnedRoomId, (Instant, HashMap<OwnedUserId, RosterRow>)>,
    roster_loading: HashSet<OwnedRoomId>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

/// The cache only if it still belongs to `account`: a reply that comes back
/// after an account change is dropped instead of resetting the new cache.
fn cache_if(account: &OwnedUserId) -> Option<std::sync::MutexGuard<'static, Cache>> {
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    (cache.account.as_ref() == Some(account)).then_some(cache)
}

/// The cache, reset for `account` if it belonged to another one (readers).
fn cache_for(account: &OwnedUserId) -> std::sync::MutexGuard<'static, Cache> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.account.as_ref() != Some(account) {
        *cache = Cache { account: Some(account.clone()), ..Default::default() };
    }
    cache
}

/// What is known about the Agent `agent` seen in `room`, starting a fetch for
/// whatever is missing or stale.
pub fn facts(agent: &UserId, room: &RoomId) -> AgentFacts {
    let Some(account) = crate::sliding_sync::current_user_id() else { return AgentFacts::default() };
    own_agents::ensure_listed();
    if let Some(row) = own_agents::row_for(agent) {
        let local = {
            let cache = cache_for(&account);
            cache.local.get(&row.agent_id).cloned()
        };
        if local.as_ref().is_none_or(|l| l.at.elapsed() > STATUS_TTL) { load_local(&account, &row.agent_id); }
        let cards = super::agent_room_cards::in_room(room, agent);
        let status = own_status(&row.state, local.as_ref().and_then(|l| l.service.as_deref()));
        return AgentFacts {
            own_agent_id: Some(row.agent_id),
            owner: Some(account),
            runtime: local.as_ref().and_then(|l| l.runtime).or(cards.runtime),
            status: if cards.working && status == Some("Running") { Some(WORKING) } else { status },
            activity: None,
            shared: local.as_ref().and_then(|l| l.shares.get(room.as_str()).copied()),
        };
    }
    let roster = {
        let cache = cache_for(&account);
        cache.rosters.get(room).map(|(at, rows)| (at.elapsed() > ROSTER_TTL, rows.get(agent).cloned()))
    };
    if roster.as_ref().is_none_or(|(stale, _)| *stale) { load_roster(&account, room.to_owned()); }
    let mut facts = others_facts(roster.and_then(|(_, row)| row), super::agent_room_cards::in_room(room, agent));
    facts.activity = facts.status.is_none().then(|| {
        crate::home::room_watches::activity(room, agent.as_str(), facts.owner.as_ref().map(|o| o.as_str()))
    }).flatten();
    facts
}

/// The status key of an Agent with a run still going in the Room.
const WORKING: &str = "Working on a request";

/// Another owner's Agent from its roster row (this server's projects) and
/// its run cards in the Room (any server): the roster wins where both say.
pub fn others_facts(row: Option<RosterRow>, cards: super::agent_room_cards::CardFacts) -> AgentFacts {
    let paused = row.as_ref().is_some_and(|r| r.binding_state == "suspended");
    let status = match (paused, cards.working) {
        (true, _) => Some("Paused"),
        (false, true) => Some(WORKING),
        (false, false) => row.as_ref().and_then(|r| roster_status(&r.binding_state)),
    };
    AgentFacts {
        own_agent_id: None,
        status,
        runtime: row.as_ref().and_then(|r| r.runtime).map(mapping::harness_label).or(cards.runtime),
        owner: row.map(|r| r.owner).or(cards.owner),
        activity: None,
        shared: None,
    }
}

fn load_local(account: &OwnedUserId, agent_id: &str) {
    if !cache_for(account).local_loading.insert(agent_id.to_owned()) { return; }
    let (account, agent_id) = (account.clone(), agent_id.to_owned());
    crate::sliding_sync::spawn_async_task(async move {
        let status = crate::host::hagency::local::Call::Status(agent_id.clone()).execute(account.as_str()).await;
        let runtime = match &status {
            Ok(status) if status["here"] == true => saved_runtime(&agent_id).await,
            _ => None,
        };
        let Some(mut cache) = cache_if(&account) else { return };
        cache.local_loading.remove(&agent_id);
        let shares = status.as_ref().map(group_room_shares).unwrap_or_default();
        let service = match status {
            Ok(status) => Some(status["service"]["state"].as_str().unwrap_or("unknown").to_owned()),
            // An unread status is not "Offline": show none until a later hover asks again.
            Err(code) => { log!("Agent status for the hover card failed: {code}"); None }
        };
        cache.local.insert(agent_id, Local { at: Instant::now(), service, runtime, shares });
        drop(cache);
        Cx::post_action(AgentFactsChanged);
    });
}

/// The owner's choices per group room of an own Agent's status reply.
fn group_room_shares(status: &Value) -> HashMap<String, crate::hagency::room_share::RoomShare> {
    mapping::watch_rooms(status).into_iter()
        .filter_map(|(id, _)| crate::hagency::room_share::for_room(&status["approvals"], &id).map(|share| (id, share)))
        .collect()
}

/// The runtime label of an Agent that runs here.
async fn saved_runtime(agent_id: &str) -> Option<&'static str> {
    let service = crate::hagency::backend::Service::for_current().ok()?;
    runtime_kind(&service, agent_id).await.map(mapping::harness_label)
}

/// "claude", "codex" or "octos" for an own Agent that runs here, from its
/// saved model's credential; `None` when it is not known on this device.
pub(crate) async fn runtime_kind(service: &crate::hagency::backend::Service, agent_id: &str) -> Option<&'static str> {
    let command = Command::Local { agent_id: agent_id.into(), action: LocalAction::AgentModel, input: None, binding_id: None, requester: None };
    let model = service.execute(command).await.ok()?;
    let credential = model["modelProfile"]["credential_ref"].as_str().filter(|c| !c.is_empty())?;
    Some(mapping::runtime_of(credential))
}

// TODO(R3): mobile_chat_info::fetch_member_owners reads the same roster for
// member grouping; move both onto this loader in a refactor session (R8).
fn load_roster(account: &OwnedUserId, room: OwnedRoomId) {
    if !cache_for(account).roster_loading.insert(room.clone()) { return; }
    let account = account.clone();
    crate::sliding_sync::spawn_async_task(async move {
        let rows = match room_roster(room.as_str()).await {
            Ok(rows) => rows,
            // A Room outside any project has no roster: owners stay unknown.
            Err(code) => { log!("Room Agent roster for the hover card unavailable: {code}"); HashMap::new() }
        };
        let Some(mut cache) = cache_if(&account) else { return };
        cache.roster_loading.remove(&room);
        cache.rosters.insert(room, (Instant::now(), rows));
        drop(cache);
        Cx::post_action(AgentFactsChanged);
    });
}

async fn room_roster(room: &str) -> Result<HashMap<OwnedUserId, RosterRow>, String> {
    room_roster_with(&crate::hagency::backend::Service::for_current()?, room).await
}

/// The project roster of `room`'s Agents, every owner's, keyed by puppet.
pub(crate) async fn room_roster_with(service: &crate::hagency::backend::Service, room: &str) -> Result<HashMap<OwnedUserId, RosterRow>, String> {
    let context = service.agents_for_registered_room(room).await?;
    let project = context["projectId"].as_str().filter(|id| !id.is_empty()).ok_or("room_discovery_unavailable")?;
    let roster = service.execute(Command::RoomRoster { project_id: project.into(), room_id: room.into() }).await?;
    Ok(roster_rows(&roster["agents"]))
}

#[cfg(test)]
#[path = "agent_facts_tests.rs"]
mod tests;
