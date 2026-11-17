//! What the hover card and the profile say about an Agent: its owner, its
//! runtime and its status.
//!
//! - One of this account's own Agents: the owner is us, the status comes from
//!   the listing state plus its local service status (`Call::Status`), and the
//!   runtime from its saved model when it runs on this computer.
//! - Another owner's Agent: the Room's Agent roster (`Command::RoomRoster`)
//!   names its owner and whether it is paused in that Room. The server shares
//!   neither its runtime nor its live status with other people, so those stay
//!   unknown, and a Room that is not in a project has no roster at all.
//!
//! Reads never block drawing: a lookup returns what is cached and starts at
//! most one fetch, whose reply posts [`AgentFactsChanged`].

use std::{collections::{HashMap, HashSet}, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use makepad_widgets::{log, Cx};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};
use serde_json::Value;
use crate::hagency::{agent_panel::mapping, model::{Command, LocalAction}};
use super::own_agents;

/// How long an own Agent's service status is shown before it is asked again.
const STATUS_TTL: Duration = Duration::from_secs(30);
/// How long a Room's Agent roster is reused.
const ROSTER_TTL: Duration = Duration::from_secs(60);
/// Upper bound of roster rows read (the member list caps alike).
const MAX_ROWS: usize = 128;

/// Posted when a fetch changed what is known, so an open card or profile re-reads.
#[derive(Clone, Debug)]
pub struct AgentFactsChanged;

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
}

/// An own Agent's status from its listing `state` and local `service` state
/// (`host::hagency::local` words it); `None` while the service is unread.
pub fn own_status(state: &str, service: Option<&str>) -> Option<&'static str> {
    if state == "suspended" { return Some("Paused"); }
    Some(match service? {
        "running" | "processing" | "waiting" | "starting" | "attention" => "Running",
        "stopped" | "paused" => "Paused",
        "elsewhere" => "On another device",
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
}

/// Puppet → roster row for a `RoomRoster` reply's `agents`.
pub fn roster_rows(rows: &Value) -> HashMap<OwnedUserId, RosterRow> {
    rows.as_array().into_iter().flatten().take(MAX_ROWS).filter_map(|row| {
        let puppet = OwnedUserId::try_from(row["puppetMxid"].as_str()?).ok()?;
        let owner = OwnedUserId::try_from(row["ownerMxid"].as_str()?).ok()?;
        let binding_state = row["bindingState"].as_str().unwrap_or_default().to_owned();
        Some((puppet, RosterRow { owner, binding_state }))
    }).collect()
}

/// An own Agent's local reply: its service state, and its runtime if known.
#[derive(Clone, Debug)]
struct Local { at: Instant, service: Option<String>, runtime: Option<&'static str> }

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
        return AgentFacts {
            own_agent_id: Some(row.agent_id),
            owner: Some(account),
            runtime: local.as_ref().and_then(|l| l.runtime),
            status: own_status(&row.state, local.as_ref().and_then(|l| l.service.as_deref())),
        };
    }
    let roster = {
        let cache = cache_for(&account);
        cache.rosters.get(room).map(|(at, rows)| (at.elapsed() > ROSTER_TTL, rows.get(agent).cloned()))
    };
    if roster.as_ref().is_none_or(|(stale, _)| *stale) { load_roster(&account, room.to_owned()); }
    let row = roster.and_then(|(_, row)| row);
    AgentFacts {
        own_agent_id: None,
        status: row.as_ref().and_then(|r| roster_status(&r.binding_state)),
        owner: row.map(|r| r.owner),
        runtime: None,
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
        let service = match status {
            Ok(status) => Some(status["service"]["state"].as_str().unwrap_or("unknown").to_owned()),
            // An unread status is not "Offline": show none until a later hover asks again.
            Err(code) => { log!("Agent status for the hover card failed: {code}"); None }
        };
        cache.local.insert(agent_id, Local { at: Instant::now(), service, runtime });
        drop(cache);
        Cx::post_action(AgentFactsChanged);
    });
}

/// The runtime of an Agent that runs here, from its saved model's credential.
async fn saved_runtime(agent_id: &str) -> Option<&'static str> {
    let service = crate::hagency::backend::Service::for_current().ok()?;
    let command = Command::Local { agent_id: agent_id.into(), action: LocalAction::AgentModel, input: None, binding_id: None, requester: None };
    let model = service.execute(command).await.ok()?;
    let credential = model["modelProfile"]["credential_ref"].as_str().filter(|c| !c.is_empty())?;
    Some(mapping::harness_label(mapping::runtime_of(credential)))
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
    let service = crate::hagency::backend::Service::for_current()?;
    let context = service.agents_for_registered_room(room).await?;
    let project = context["projectId"].as_str().filter(|id| !id.is_empty()).ok_or("room_discovery_unavailable")?;
    let roster = service.execute(Command::RoomRoster { project_id: project.into(), room_id: room.into() }).await?;
    Ok(roster_rows(&roster["agents"]))
}

/// The owner's name as `room` (or their profile) shows it, else their Matrix ID.
pub fn owner_name(cx: &mut Cx, owner: &UserId, room: Option<&OwnedRoomId>) -> String {
    super::user_profile_cache::get_user_display_name_for_room(cx, owner.to_owned(), room, true)
        .into_option().filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| owner.to_string())
}

/// The fact lines under an Agent's name. `owner` is the owner's shown name.
/// The profile leads with "Agent of …", the card with "Owner: …"; another
/// owner's Agent ends with who may see its settings.
pub fn fact_lines(facts: &AgentFacts, owner: Option<&str>, profile: bool) -> Vec<String> {
    use crate::i18n::{format, tr};
    let mut lines = Vec::new();
    lines.push(match (owner, profile) {
        (Some(name), true) => format("Agent of {owner}", &[("owner", name.to_owned())]),
        (Some(name), false) => format("Owner: {owner}", &[("owner", name.to_owned())]),
        (None, _) => tr("Owner not shared in this room").to_owned(),
    });
    if let Some(runtime) = facts.runtime {
        lines.push(format("Runtime: {runtime}", &[("runtime", runtime.to_owned())]));
    }
    if let Some(status) = facts.status {
        lines.push(format("Status: {status}", &[("status", tr(status).to_owned())]));
    }
    if facts.own_agent_id.is_none() {
        lines.push(match owner {
            Some(name) => format("Settings are visible only to {owner}", &[("owner", name.to_owned())]),
            None => tr("Settings are visible only to its owner").to_owned(),
        });
    }
    lines
}

#[cfg(test)]
#[path = "agent_facts_tests.rs"]
mod tests;
