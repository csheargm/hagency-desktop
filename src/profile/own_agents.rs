//! This account's own Agents, keyed by puppet Matrix ID, from the owner
//! engine's Agents listing. An Agent's private chat uses it to show the
//! "Agent settings" button that opens the native agent panel. A UI hint only: the panel itself still opens
//! only Agents the owner engine lists for this account.

use std::{collections::HashMap, sync::{LazyLock, Mutex}};
use makepad_widgets::{Cx, log};
use matrix_sdk::ruma::{OwnedUserId, UserId};
use serde_json::Value;

/// Upper bound of listed Agents read into the map (the roster caps alike).
const MAX_AGENTS: usize = 128;

/// Posted when a listing changed what the map knows, so open panels can re-check.
#[derive(Clone, Debug)]
pub struct OwnAgentsChanged;

#[derive(Default)]
struct Cache {
    owner: Option<OwnedUserId>,
    puppets: HashMap<OwnedUserId, String>,
    loading: bool,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

/// Puppet → Agent ID for the Agents in a `Command::Agents` listing that belong
/// to `owner`: an exact `puppetMxid` on the owner's server, and no other owner.
pub fn own_puppets(rows: &Value, owner: &UserId) -> HashMap<OwnedUserId, String> {
    let mut index = HashMap::new();
    for row in rows.as_array().into_iter().flatten().take(MAX_AGENTS) {
        let Some(agent_id) = row["id"].as_str().filter(|id| !id.is_empty()) else { continue };
        let Some(puppet) = row["puppetMxid"].as_str().and_then(|id| OwnedUserId::try_from(id).ok()) else { continue };
        let other_owner = row["ownerMxid"].as_str().is_some_and(|o| o != owner.as_str());
        if !other_owner && puppet.server_name() == owner.server_name() {
            index.insert(puppet, agent_id.to_owned());
        }
    }
    index
}

/// The owned Agent behind `user`, from the last listing for the current account.
pub fn agent_for(user: &UserId) -> Option<String> {
    let owner = crate::sliding_sync::current_user_id()?;
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.owner.as_ref() != Some(&owner) {
        return None;
    }
    cache.puppets.get(user).cloned()
}


/// Starts one Agents listing for the current account unless one is running.
pub fn refresh() {
    let Some(owner) = crate::sliding_sync::current_user_id() else { return };
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if cache.owner.as_ref() != Some(&owner) {
            *cache = Cache { owner: Some(owner.clone()), ..Default::default() };
        }
        if cache.loading { return; }
        cache.loading = true;
    }
    crate::sliding_sync::spawn_async_task(async move {
        let listed = match crate::hagency::backend::Service::for_current() {
            Ok(service) => service.execute(::hagency::native_owner::Command::Agents).await,
            Err(code) => Err(code),
        };
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.loading = false;
        if cache.owner.as_ref() != Some(&owner) {
            return; // The account changed meanwhile; its own listing owns the cache.
        }
        match listed {
            Ok(listed) => cache.puppets = own_puppets(&listed["agents"], &owner),
            // No listing yet (signing in, owner engine not ready): own Agents
            // fall back to the user panel, and the next click asks again.
            Err(code) => { log!("Own Agents listing failed: {code}"); return; }
        }
        drop(cache);
        Cx::post_action(OwnAgentsChanged);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(id: &str) -> OwnedUserId { OwnedUserId::try_from(id).unwrap() }

    #[test]
    fn only_the_owners_own_puppets_on_the_owners_server_resolve() {
        let owner = user("@ann:s");
        let rows = json!([
            {"id": "agt_1", "puppetMxid": "@_hagency_agt_1:s", "ownerMxid": "@ann:s"},
            {"id": "agt_2", "puppetMxid": "@_hagency_agt_2:s"},
            {"id": "agt_3", "puppetMxid": "@_hagency_agt_3:s", "ownerMxid": "@bob:s"},
            {"id": "agt_4", "puppetMxid": "@_hagency_agt_4:other"},
            {"id": "", "puppetMxid": "@_hagency_agt_5:s"},
            {"id": "agt_6", "puppetMxid": "not an id"}
        ]);
        let index = own_puppets(&rows, &owner);
        assert_eq!(index.get(&user("@_hagency_agt_1:s")).map(String::as_str), Some("agt_1"));
        assert_eq!(index.get(&user("@_hagency_agt_2:s")).map(String::as_str), Some("agt_2"), "a row without an owner is the listing account's");
        assert_eq!(index.len(), 2, "another owner's, another server's and malformed rows are left out");
        assert!(own_puppets(&json!(null), &owner).is_empty());
    }
}
