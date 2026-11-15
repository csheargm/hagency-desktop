//! UX A4: a small "who can ask" badge next to each of the owner's own Agents
//! in a room's member list ("asks: only owner", "asks: room members").
//!
//! Data flow: the member list already lists this account's Agents
//! (`Command::Agents`) when room info opens; it hands that listing to
//! [`refresh`], which maps puppet Matrix IDs to owned Agent IDs and reads each
//! Agent's own request policy once (`LocalAction::AgentPolicy`). Results are
//! cached per Agent for the session; drawing only reads the cache. Members
//! that are not this owner's Agents never get a badge. Clicking an own
//! Agent's row opens the native agent panel ([`open_own_agent`]).
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use serde_json::Value;

use crate::hagency::agent_panel::mapping::WhoCanAsk;
use crate::hagency::ui::HagencyAction;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentAskBadge = RoundedView {
        visible: false width: Fit height: Fit
        padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
        draw_bg +: {color: mod.widgets.RINX_SELECTED border_radius: 4}
        ask_text := Label {width: Fit height: Fit padding: 0 max_lines: 1
            draw_text +: {color: mod.widgets.RINX_ACCENT text_style: theme.font_regular{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}}
    }
}

/// Reserved localpart prefix of Agent puppets (see `hagency::backend::is_agent_invitation`).
/// Puppet localparts are `_hagency_` + the Agent ID, which itself starts with
/// `agt_` (e.g. `@_hagency_agt_01m76q…`).
const PUPPET_PREFIX: &str = "_hagency_";
/// `RequestPolicy::AskOwner` wire name; `mapping` has no simple choice for it.
const REQUESTS_ASK_OWNER: &str = "AskOwner";
/// Upper bound of policy reads per refresh (the member list caps its roster alike).
const MAX_AGENTS: usize = 128;
/// One local policy read must not keep the badge loading forever.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Default)]
struct Cache {
    owner: Option<OwnedUserId>,
    /// Puppet Matrix ID → owned Agent ID, from the latest listing.
    puppets: HashMap<OwnedUserId, String>,
    /// Owned Agent ID → saved `requests` wire value.
    requests: HashMap<String, String>,
    loading: bool,
    generation: u64,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

/// The English badge key for a saved `requests` value; `None` hides the badge.
pub(crate) fn badge_text(requests: &str) -> Option<&'static str> {
    match WhoCanAsk::from_requests(requests) {
        Some(WhoCanAsk::OnlyMe) => Some("asks: only owner"),
        Some(WhoCanAsk::RoomMembers) => Some("asks: room members"),
        Some(WhoCanAsk::Anyone) => Some("asks: anyone"),
        None if requests == REQUESTS_ASK_OWNER => Some("asks: owner first"),
        None => None,
    }
}

/// Whether `puppet` is the reserved puppet of `agent_id` on the owner's server.
pub(crate) fn is_owned_puppet(puppet: &UserId, agent_id: &str, owner: &UserId) -> bool {
    !agent_id.is_empty()
        && puppet.server_name() == owner.server_name()
        && puppet.localpart() == format!("{PUPPET_PREFIX}{}", agent_id.to_lowercase())
}

/// Puppet → Agent ID for the owner's own Agents in a `Command::Agents` listing.
pub(crate) fn puppet_index(rows: &Value, owner: &UserId) -> HashMap<OwnedUserId, String> {
    let mut index = HashMap::new();
    for row in rows.as_array().into_iter().flatten().take(MAX_AGENTS) {
        let Some(agent_id) = row["id"].as_str() else { continue };
        let Some(puppet) = row["puppetMxid"].as_str().and_then(|id| OwnedUserId::try_from(id).ok()) else { continue };
        let foreign_owner = row["ownerMxid"].as_str().is_some_and(|o| o != owner.as_str());
        if !foreign_owner && is_owned_puppet(&puppet, agent_id, owner) {
            index.insert(puppet, agent_id.to_owned());
        }
    }
    index
}

fn lookup(cache: &Cache, owner: &UserId, member: &UserId) -> Option<&'static str> {
    badge_text(cache.requests.get(own_agent_id(cache, owner, member)?)?)
}

fn own_agent_id<'a>(cache: &'a Cache, owner: &UserId, member: &UserId) -> Option<&'a str> {
    if cache.owner.as_deref() != Some(owner) {
        return None;
    }
    cache.puppets.get(member).map(String::as_str)
}

fn open_action_in(cache: &Cache, owner: &UserId, member: &UserId) -> Option<HagencyAction> {
    own_agent_id(cache, owner, member).map(|agent_id| HagencyAction::OpenAgent { agent_id: agent_id.to_owned() })
}

/// The agent-panel action for a click on `member`'s row, when `member` is one
/// of this account's own Agents; `None` keeps the profile click.
pub(crate) fn open_own_agent(owner: Option<&OwnedUserId>, member: &UserId) -> Option<HagencyAction> {
    open_action_in(&CACHE.lock().unwrap_or_else(|e| e.into_inner()), owner?, member)
}

/// Starts one policy read per owned Agent unless one is already running.
pub(crate) fn refresh(owner: &OwnedUserId, rows: &Value) {
    let index = puppet_index(rows, owner);
    let (generation, agents) = {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if cache.owner.as_ref() != Some(owner) {
            let generation = cache.generation + 1;
            *cache = Cache { owner: Some(owner.clone()), generation, ..Default::default() };
        }
        cache.puppets = index;
        if cache.loading {
            return;
        }
        cache.loading = true;
        let agents: Vec<String> = cache.puppets.values().cloned().collect();
        (cache.generation, agents)
    };
    crate::sliding_sync::spawn_async_task(async move {
        let read = read_policies(agents).await;
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if cache.generation != generation {
            return; // The account changed meanwhile; its own refresh owns the cache.
        }
        cache.loading = false;
        cache.requests = read;
        drop(cache);
        SignalToUI::set_ui_signal();
    });
}

async fn read_policies(agents: Vec<String>) -> HashMap<String, String> {
    use crate::hagency::model::{Command, LocalAction};
    let mut read = HashMap::new();
    // No owner service yet: no badges. The badge is a hint, never an
    // authorization, and the next room-info open retries.
    let Ok(service) = crate::hagency::backend::Service::for_current() else { return read };
    for agent_id in agents {
        let command = Command::Local { agent_id: agent_id.clone(), action: LocalAction::AgentPolicy, input: None, binding_id: None, requester: None };
        // A failed read (an Agent that runs on another device has no local
        // policy here, or a timeout) hides only that Agent's badge.
        if let Ok(Ok(value)) = tokio::time::timeout(READ_TIMEOUT, service.execute(command)).await
            && let Some(requests) = value["policy"]["policy"]["requests"].as_str()
        {
            read.insert(agent_id, requests.to_owned());
        }
    }
    read
}

/// Shows or hides the `ask_badge` inside a member row for `member`.
pub(crate) fn show(cx: &mut Cx, row: &WidgetRef, owner: Option<&OwnedUserId>, member: &UserId) {
    let text = owner.and_then(|owner| lookup(&CACHE.lock().unwrap_or_else(|e| e.into_inner()), owner, member));
    row.view(cx, ids!(ask_badge)).set_visible(cx, text.is_some());
    if let Some(text) = text {
        row.label(cx, ids!(ask_text)).set_text(cx, crate::i18n::tr(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::agent_panel::mapping::WHO_CAN_ASK;
    use serde_json::json;

    fn user(id: &str) -> OwnedUserId { OwnedUserId::try_from(id).unwrap() }

    #[test]
    fn policy_maps_to_badge_text_and_unknown_hides_it() {
        assert_eq!(badge_text("Deny"), Some("asks: only owner"));
        assert_eq!(badge_text("Allow"), Some("asks: room members"));
        assert_eq!(badge_text("AskOwner"), Some("asks: owner first"));
        assert_eq!(badge_text(""), None);
        assert_eq!(badge_text("Something"), None);
        for choice in WHO_CAN_ASK.iter().filter_map(|c| c.requests()) {
            assert!(badge_text(choice).is_some(), "{choice} has a badge");
        }
    }

    #[test]
    fn puppets_match_only_the_owners_own_agents() {
        let owner = user("@alice:s");
        let rows = json!([
            {"id": "agt_01ABC", "puppetMxid": "@_hagency_agt_01abc:s"},
            {"id": "agt_02def", "puppetMxid": "@_hagency_agt_02def:s", "ownerMxid": "@alice:s"},
            {"id": "agt_03BOB", "puppetMxid": "@_hagency_agt_03bob:s", "ownerMxid": "@bob:s"},
            {"id": "agt_04FOR", "puppetMxid": "@_hagency_agt_04for:other"},
            {"id": "agt_05MIS", "puppetMxid": "@_hagency_agt_99zzz:s"},
            {"id": "agt_06PFX", "puppetMxid": "@_hagency_agt_06pfx_x:s"},
            {"id": "", "puppetMxid": "@_hagency_agt_:s"},
            {"id": "agt_07BAD", "puppetMxid": "not a user id"},
            {"puppetMxid": "@_hagency_agt_08:s"}
        ]);
        let index = puppet_index(&rows, &owner);
        assert_eq!(index.len(), 2, "{index:?}");
        assert_eq!(index[&user("@_hagency_agt_01abc:s")], "agt_01ABC");
        assert_eq!(index[&user("@_hagency_agt_02def:s")], "agt_02def");
        assert!(!is_owned_puppet(&user("@alice:s"), "alice", &owner), "people are never Agents");
    }

    #[test]
    fn badge_shows_only_for_cached_own_agents_of_the_current_account() {
        let owner = user("@alice:s");
        let puppet = user("@_hagency_agt_01abc:s");
        let mut cache = Cache { owner: Some(owner.clone()), ..Default::default() };
        cache.puppets.insert(puppet.clone(), "01ABC".into());
        assert_eq!(lookup(&cache, &owner, &puppet), None, "no badge before the policy is read");
        cache.requests.insert("01ABC".into(), "Deny".into());
        assert_eq!(lookup(&cache, &owner, &puppet), Some("asks: only owner"));
        assert_eq!(lookup(&cache, &user("@bob:s"), &puppet), None, "another account sees nothing");
        assert_eq!(lookup(&cache, &owner, &user("@_hagency_agt_02x:s")), None, "someone else's Agent");
        assert_eq!(lookup(&cache, &owner, &owner), None, "a person");
    }

    #[test]
    fn own_agent_row_opens_the_agent_panel_and_others_keep_the_profile() {
        let owner = user("@alice:s");
        let puppet = user("@_hagency_agt_01abc:s");
        let mut cache = Cache { owner: Some(owner.clone()), ..Default::default() };
        cache.puppets = puppet_index(&json!([{"id": "agt_01ABC", "puppetMxid": puppet.as_str()}]), &owner);
        match open_action_in(&cache, &owner, &puppet) {
            Some(HagencyAction::OpenAgent { agent_id }) => assert_eq!(agent_id, "agt_01ABC"),
            _ => panic!("own Agent row must open its agent panel"),
        }
        assert!(open_action_in(&cache, &owner, &user("@bob:s")).is_none(), "people keep the profile");
        assert!(open_action_in(&cache, &owner, &user("@_hagency_agt_02x:s")).is_none(), "others' Agents keep the profile");
        assert!(open_action_in(&cache, &user("@bob:s"), &puppet).is_none(), "another account never opens it");
        assert!(open_own_agent(None, &puppet).is_none(), "signed out");
    }
}
